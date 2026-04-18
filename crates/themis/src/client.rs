//! gRPC client factory with lazy-start support.
//!
//! # Socket resolution order
//!
//! 1. `--socket <PATH>` flag (passed in as `socket_override`)
//! 2. `THEMIS_SOCKET` environment variable (already resolved by clap)
//! 3. XDG default: `$XDG_RUNTIME_DIR/themisd.sock`
//!
//! # Lazy-start
//!
//! When the socket is absent AND `auto_start` is true:
//!   1. Locate `themisd` binary.
//!   2. Spawn it as a detached session leader.
//!   3. Poll `DaemonService::Health` every 200ms for up to 10s.
//!   4. Return error if the daemon never becomes ready.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use tonic::transport::Channel;
use tracing::{debug, info, warn};

use themis_proto::{
    daemon_service_client::DaemonServiceClient, HealthRequest,
};

// ── Public API ────────────────────────────────────────────────────────────────

/// gRPC channel handles for each service.
pub struct ThemisClient {
    pub channel: Channel,
}

/// Connect to themisd, optionally starting it if the socket is absent.
///
/// `socket_path` is already resolved by the caller (see `resolve_socket`).
pub async fn connect_or_start(socket_path: PathBuf, auto_start: bool) -> Result<ThemisClient> {
    // 1. Fast path: socket exists and daemon is ready.
    if socket_path.exists() {
        match try_connect(&socket_path).await {
            Ok(ch) => return Ok(ThemisClient { channel: ch }),
            Err(e) => {
                warn!(path = %socket_path.display(), error = %e, "stale socket — daemon not responding");
                bail!(
                    "socket '{}' exists but daemon is not responding. \
                     Try removing the socket file and restarting themisd.",
                    socket_path.display()
                );
            }
        }
    }

    // 2. Socket absent.
    if !auto_start {
        bail!(
            "themisd is not running (socket '{}' not found). \
             Start it with `themisd` or remove --no-auto-start.",
            socket_path.display()
        );
    }

    // 3. Spawn themisd.
    let daemon_bin = locate_themisd_bin()?;
    info!(bin = %daemon_bin.display(), "auto-starting themisd");
    spawn_daemon(&daemon_bin)?;

    // 4. Poll until ready.
    wait_until_ready(&socket_path, Duration::from_secs(10)).await?;

    let ch = try_connect(&socket_path)
        .await
        .context("daemon became ready but connection failed")?;
    Ok(ThemisClient { channel: ch })
}

/// Resolve the socket path from flag, env, and XDG defaults.
///
/// The flag/env resolution is already handled by clap (`--socket` and
/// `THEMIS_SOCKET`). Here we just apply the XDG fallback when nothing was
/// passed.
pub fn resolve_socket(flag: Option<PathBuf>) -> PathBuf {
    if let Some(p) = flag {
        return p;
    }
    // XDG default (mirrors themisd::paths::DaemonPaths::from_env().socket).
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("themisd.sock");
    }
    let uid = effective_uid();
    PathBuf::from(format!("/tmp/themisd-{uid}.sock"))
}

// ── Connection helpers ────────────────────────────────────────────────────────

/// Build a tonic `Channel` over a Unix socket using the tower `service_fn`
/// pattern (tonic has no first-class Unix-socket constructor).
async fn try_connect(socket_path: &std::path::Path) -> Result<Channel> {
    let path = socket_path.to_path_buf();
    let channel = tonic::transport::Endpoint::try_from("http://[::]:0")
        .context("endpoint construction")?
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(&path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .context("connect to unix socket")?;
    Ok(channel)
}

// ── Daemon location ────────────────────────────────────────────────────────────

/// Locate the `themisd` binary.
///
/// Resolution order:
///   1. `THEMIS_DAEMON_BIN` env var.
///   2. Sibling of the current executable (same directory).
///   3. `$PATH` lookup.
fn locate_themisd_bin() -> Result<PathBuf> {
    // 1. Env override.
    if let Ok(p) = std::env::var("THEMIS_DAEMON_BIN") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        bail!("THEMIS_DAEMON_BIN='{}' is not a file", path.display());
    }

    // 2. Sibling of current exe.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("themisd");
            if sibling.is_file() {
                debug!(path = %sibling.display(), "found themisd as sibling of themis");
                return Ok(sibling);
            }
        }
    }

    // 3. PATH lookup.
    which_themisd().context("could not find themisd binary; install it or set THEMIS_DAEMON_BIN")
}

/// Find `themisd` on `$PATH` by iterating PATH entries.
fn which_themisd() -> Result<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("themisd");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("themisd not found on PATH")
}

// ── Spawn helper ──────────────────────────────────────────────────────────────

/// Spawn `themisd` as a detached session leader (survives CLI exit).
fn spawn_daemon(bin: &PathBuf) -> Result<()> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // On Unix: make the child a session leader so it doesn't inherit our
    // controlling terminal and won't be killed when we exit.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid() is async-signal-safe and has no preconditions.
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;
    Ok(())
}

#[cfg(unix)]
fn libc_setsid() {
    extern "C" {
        fn setsid() -> i32;
    }
    // SAFETY: called in pre_exec after fork, before exec; no Rust allocator is
    // active yet. setsid() has no preconditions.
    unsafe { setsid() };
}

// ── Ready polling ─────────────────────────────────────────────────────────────

/// Poll `Health` every 200ms until ready or timeout.
async fn wait_until_ready(socket_path: &std::path::Path, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut interval = tokio::time::interval(Duration::from_millis(200));

    loop {
        interval.tick().await;

        if tokio::time::Instant::now() > deadline {
            bail!(
                "themisd did not become ready within {}s (socket: {})",
                timeout.as_secs(),
                socket_path.display()
            );
        }

        // Socket might not exist yet.
        if !socket_path.exists() {
            continue;
        }

        if let Ok(ch) = try_connect(socket_path).await {
            let mut client = DaemonServiceClient::new(ch);
            if let Ok(resp) = client.health(HealthRequest {}).await {
                if resp.into_inner().ready {
                    debug!("themisd is ready");
                    return Ok(());
                }
            }
        }
    }
}

// ── UID helper ────────────────────────────────────────────────────────────────

fn effective_uid() -> u32 {
    #[cfg(unix)]
    {
        extern "C" {
            fn getuid() -> u32;
        }
        // SAFETY: getuid() never fails and has no preconditions.
        unsafe { getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_socket_with_xdg() {
        // Temporarily set XDG_RUNTIME_DIR and verify socket path.
        // Use a temp env in a thread-local-friendly way.
        let orig = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/9999");
        let p = resolve_socket(None);
        // Restore.
        match orig {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        assert_eq!(p, PathBuf::from("/run/user/9999/themisd.sock"));
    }

    #[test]
    fn resolve_socket_flag_takes_precedence() {
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/9999");
        let flag = PathBuf::from("/custom/path.sock");
        let p = resolve_socket(Some(flag.clone()));
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(p, flag);
    }
}
