//! SSH orchestration — `russh` wrapper with parallel-execution helpers.
//!
//! Provides an async SSH client for Themis runtime operations: pushing NOS configs,
//! running chaos primitives, and waiting for freshly-booted VMs to become reachable.
//!
//! # Design notes
//!
//! * Authentication supports password and key-file modes (`SshAuth`).
//! * `write_file` avoids an SFTP dependency by base64-encoding content and
//!   piping it through `echo … | base64 -d > <path>` — safe for binary payloads.
//! * `parallel_exec` fans out across hosts via `tokio::task::JoinSet`, preserving
//!   input order in the result vector.
//! * `wait_for_reachable` polls with exponential back-off up to the caller-supplied
//!   timeout, making it suitable for post-boot readiness gates.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use russh::client::{self, Config as RusshConfig, Handle};
use russh::{ChannelMsg, Disconnect};
use tokio::task::JoinSet;
use tracing::{debug, warn};

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration needed to open one SSH connection.
#[derive(Debug, Clone)]
pub struct SshConfig {
    /// Hostname or IP address of the remote.
    pub host: String,
    /// SSH port (usually 22).
    pub port: u16,
    /// Login username.
    pub user: String,
    /// Authentication credential.
    pub auth: SshAuth,
    /// Wall-clock limit for the initial TCP + SSH handshake.
    pub connect_timeout: Duration,
}

/// Authentication credential variants.
#[derive(Debug, Clone)]
pub enum SshAuth {
    /// Plaintext password authentication.
    Password(String),
    /// PEM / OpenSSH private-key file path (no passphrase).
    KeyFile(std::path::PathBuf),
}

/// Result of a remote command execution.
#[derive(Debug, Clone)]
pub struct ExecResult {
    /// Decoded UTF-8 stdout (invalid bytes replaced with U+FFFD).
    pub stdout: String,
    /// Decoded UTF-8 stderr (invalid bytes replaced with U+FFFD).
    pub stderr: String,
    /// Process exit code; 0 conventionally means success.
    pub exit_code: i32,
}

// ── Internal handler ──────────────────────────────────────────────────────────

/// Minimal `russh::client::Handler` that accepts any host key.
///
/// SECURITY NOTE: Accepting any key is acceptable for a lab emulator operating
/// in an isolated private network. A production deployment should verify keys
/// against a known-hosts store; that can be added to this struct later without
/// changing the public API.
struct AcceptAllHandler;

#[async_trait]
impl client::Handler for AcceptAllHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

// ── SshClient ─────────────────────────────────────────────────────────────────

/// A live, authenticated SSH session to a single remote host.
///
/// Obtain one with [`SshClient::connect`]; release the connection with
/// [`SshClient::disconnect`].
pub struct SshClient {
    handle: Handle<AcceptAllHandler>,
}

impl SshClient {
    /// Open a new SSH connection and authenticate.
    ///
    /// Times out after `config.connect_timeout`.
    pub async fn connect(config: &SshConfig) -> themis_core::Result<Self> {
        let addr = format!("{}:{}", config.host, config.port);
        debug!(host = %config.host, port = config.port, user = %config.user, "opening SSH connection");

        let russh_cfg = Arc::new(RusshConfig {
            inactivity_timeout: Some(config.connect_timeout),
            ..<_>::default()
        });

        // Enforce the connect timeout ourselves so it covers both TCP and the
        // SSH handshake, not just the TCP SYN.
        let mut handle = tokio::time::timeout(
            config.connect_timeout,
            client::connect(russh_cfg, addr.as_str(), AcceptAllHandler),
        )
        .await
        .map_err(|_| {
            themis_core::Error::Runtime(format!(
                "SSH connect to {} timed out after {:?}",
                addr, config.connect_timeout
            ))
        })?
        .map_err(|e| themis_core::Error::Runtime(format!("SSH handshake failed for {addr}: {e}")))?;

        // Authenticate.
        let authed = match &config.auth {
            SshAuth::Password(pw) => handle
                .authenticate_password(config.user.clone(), pw.clone())
                .await
                .map_err(|e| {
                    themis_core::Error::Runtime(format!(
                        "SSH password auth error for {addr}: {e}"
                    ))
                })?,

            SshAuth::KeyFile(path) => {
                let key_pair = russh::keys::load_secret_key(path, None).map_err(|e| {
                    themis_core::Error::Runtime(format!(
                        "failed to load SSH key {:?}: {e}",
                        path
                    ))
                })?;
                handle
                    .authenticate_publickey(config.user.clone(), Arc::new(key_pair))
                    .await
                    .map_err(|e| {
                        themis_core::Error::Runtime(format!(
                            "SSH pubkey auth error for {addr}: {e}"
                        ))
                    })?
            }
        };

        if !authed {
            return Err(themis_core::Error::Runtime(format!(
                "SSH authentication rejected by {addr} for user '{}'",
                config.user
            )));
        }

        debug!(host = %config.host, "SSH authenticated");
        Ok(Self { handle })
    }

    /// Execute a command on the remote and collect all output.
    ///
    /// Waits for the channel to close before returning, so `ExecResult` always
    /// contains the complete stdout/stderr.
    pub async fn exec(&mut self, command: &str) -> themis_core::Result<ExecResult> {
        debug!(cmd = %command, "SSH exec");

        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| themis_core::Error::Runtime(format!("channel_open_session: {e}")))?;

        channel
            .exec(true, command)
            .await
            .map_err(|e| themis_core::Error::Runtime(format!("channel exec: {e}")))?;

        let mut stdout_bytes: Vec<u8> = Vec::new();
        let mut stderr_bytes: Vec<u8> = Vec::new();
        let mut exit_code: i32 = -1;

        loop {
            let Some(msg) = channel.wait().await else {
                break;
            };
            match msg {
                ChannelMsg::Data { ref data } => {
                    stdout_bytes.extend_from_slice(data);
                }
                ChannelMsg::ExtendedData { ref data, ext } => {
                    // ext == 1 is SSH_EXTENDED_DATA_STDERR per RFC 4254
                    if ext == 1 {
                        stderr_bytes.extend_from_slice(data);
                    } else {
                        stdout_bytes.extend_from_slice(data);
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => {
                    exit_code = exit_status as i32;
                    // Do NOT break here: there may still be buffered Data frames
                    // arriving after the exit-status message.
                }
                ChannelMsg::Eof | ChannelMsg::Close => {
                    // Continue draining until channel.wait() returns None.
                }
                _ => {}
            }
        }

        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
            exit_code,
        })
    }

    /// Upload `content` to `remote_path` on the target host.
    ///
    /// Uses the base64-pipe trick to avoid an SFTP dependency:
    /// ```text
    /// echo <base64> | base64 -d > /remote/path
    /// ```
    /// Safe for binary payloads; the only limit is the remote shell's argument
    /// length (practically ~2 MiB). For large files, chunking can be added later.
    pub async fn write_file(
        &mut self,
        remote_path: &Path,
        content: &[u8],
    ) -> themis_core::Result<()> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);

        let remote_path_str = remote_path
            .to_str()
            .ok_or_else(|| {
                themis_core::Error::Runtime(format!(
                    "remote path is not valid UTF-8: {:?}",
                    remote_path
                ))
            })?;

        // Ensure parent directory exists before writing.
        let parent = remote_path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".");
        let cmd = format!(
            "mkdir -p {parent} && echo {encoded} | base64 -d > {remote_path_str}",
            parent = shell_escape_single(parent),
            encoded = encoded,
            remote_path_str = shell_escape_single(remote_path_str),
        );

        let result = self.exec(&cmd).await?;
        if result.exit_code != 0 {
            return Err(themis_core::Error::Runtime(format!(
                "write_file to {:?} failed (exit {}): {}",
                remote_path, result.exit_code, result.stderr.trim()
            )));
        }
        Ok(())
    }

    /// Gracefully close the SSH connection.
    ///
    /// Consumes `self` so callers cannot accidentally re-use a closed session.
    pub async fn disconnect(self) -> themis_core::Result<()> {
        self.handle
            .disconnect(Disconnect::ByApplication, "", "English")
            .await
            .map_err(|e| themis_core::Error::Runtime(format!("SSH disconnect: {e}")))?;
        Ok(())
    }
}

// ── Standalone helpers ─────────────────────────────────────────────────────────

/// Poll until an SSH handshake succeeds or `timeout` expires.
///
/// Useful as a post-boot readiness gate for freshly created VMs.
/// Uses exponential back-off starting at 500 ms, capped at 5 s.
///
/// # Errors
/// Returns [`themis_core::Error::Runtime`] if the timeout expires before a
/// successful connection.
pub async fn wait_for_reachable(
    config: &SshConfig,
    timeout: Duration,
) -> themis_core::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut delay = Duration::from_millis(500);
    const MAX_DELAY: Duration = Duration::from_secs(5);

    loop {
        // Probe with a short individual timeout so a stalled TCP SYN does not
        // eat the entire caller-supplied budget.
        let probe_timeout = delay.min(Duration::from_secs(5));
        let probe_config = SshConfig {
            connect_timeout: probe_timeout,
            ..config.clone()
        };

        match SshClient::connect(&probe_config).await {
            Ok(client) => {
                // Connected — disconnect cleanly and return success.
                if let Err(e) = client.disconnect().await {
                    warn!("disconnect after reachability probe failed: {e}");
                }
                return Ok(());
            }
            Err(e) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(themis_core::Error::Runtime(format!(
                        "SSH host {} not reachable within {:?}: {}",
                        config.host, timeout, e
                    )));
                }
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                let sleep_for = delay.min(remaining);
                debug!(
                    host = %config.host,
                    retry_in = ?sleep_for,
                    "SSH not yet reachable, retrying"
                );
                tokio::time::sleep(sleep_for).await;
                delay = (delay * 2).min(MAX_DELAY);
            }
        }
    }
}

/// Run `command` on every host in `configs` concurrently.
///
/// Returns one `(hostname, Result<ExecResult>)` pair per input entry, **in the
/// same order** as `configs`. Per-host failures do not abort sibling tasks.
///
/// # Example
/// ```no_run
/// # use themis_runtime::ssh::{SshConfig, SshAuth, parallel_exec};
/// # use std::time::Duration;
/// # #[tokio::main] async fn main() {
/// let cfgs = vec![
///     SshConfig { host: "10.0.0.1".into(), port: 22, user: "admin".into(),
///                 auth: SshAuth::Password("pass".into()),
///                 connect_timeout: Duration::from_secs(10) },
/// ];
/// let results = parallel_exec(&cfgs, "uname -r").await.unwrap();
/// for (host, res) in results {
///     println!("{host}: {:?}", res.map(|r| r.stdout));
/// }
/// # }
/// ```
pub async fn parallel_exec(
    configs: &[SshConfig],
    command: &str,
) -> themis_core::Result<Vec<(String, themis_core::Result<ExecResult>)>> {
    let mut set: JoinSet<(String, themis_core::Result<ExecResult>)> = JoinSet::new();

    // Spawn one task per host.  We clone both the config and the command string
    // so each task is fully independent.
    for cfg in configs {
        let cfg = cfg.clone();
        let cmd = command.to_owned();
        set.spawn(async move {
            let host = cfg.host.clone();
            let result = run_on_host(&cfg, &cmd).await;
            (host, result)
        });
    }

    // Collect results.  JoinSet does not preserve spawn order, so we
    // gather into a map keyed by hostname then reconstruct the input order.
    // (Hosts are assumed to be unique within a single parallel_exec call.)
    let mut map: std::collections::HashMap<String, themis_core::Result<ExecResult>> =
        std::collections::HashMap::with_capacity(configs.len());

    while let Some(join_result) = set.join_next().await {
        match join_result {
            Ok((host, exec_result)) => {
                map.insert(host, exec_result);
            }
            Err(e) => {
                // A task panic — surface as a Runtime error.  We cannot
                // recover the host name from a JoinError, so log and skip.
                warn!("parallel_exec task panicked: {e}");
            }
        }
    }

    // Reconstruct input order.
    let ordered = configs
        .iter()
        .map(|cfg| {
            let result = map.remove(&cfg.host).unwrap_or_else(|| {
                Err(themis_core::Error::Runtime(format!(
                    "task for host {} did not complete (possible panic)",
                    cfg.host
                )))
            });
            (cfg.host.clone(), result)
        })
        .collect();

    Ok(ordered)
}

// ── Private helpers ────────────────────────────────────────────────────────────

/// Connect, exec, disconnect — the three-step sequence used by [`parallel_exec`].
async fn run_on_host(config: &SshConfig, command: &str) -> themis_core::Result<ExecResult> {
    let mut client = SshClient::connect(config).await?;
    let result = client.exec(command).await;
    // Attempt graceful disconnect regardless of exec outcome.
    if let Err(e) = client.disconnect().await {
        warn!(host = %config.host, "disconnect after exec failed: {e}");
    }
    result
}

/// Wrap a string in single quotes, escaping any embedded single quotes.
///
/// `'foo bar'` → safe for shell insertion; `it's` → `'it'"'"'s'`.
fn shell_escape_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'"'"'"#))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── SshConfig construction ────────────────────────────────────────────────

    #[test]
    fn ssh_config_password_round_trip() {
        let cfg = SshConfig {
            host: "192.168.1.1".into(),
            port: 22,
            user: "admin".into(),
            auth: SshAuth::Password("s3cr3t".into()),
            connect_timeout: Duration::from_secs(10),
        };
        assert_eq!(cfg.host, "192.168.1.1");
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.user, "admin");
        assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
        matches!(&cfg.auth, SshAuth::Password(pw) if pw == "s3cr3t");
    }

    #[test]
    fn ssh_config_key_file_round_trip() {
        let cfg = SshConfig {
            host: "10.0.0.1".into(),
            port: 2222,
            user: "root".into(),
            auth: SshAuth::KeyFile(PathBuf::from("/home/user/.ssh/id_ed25519")),
            connect_timeout: Duration::from_secs(5),
        };
        assert_eq!(cfg.port, 2222);
        if let SshAuth::KeyFile(ref p) = cfg.auth {
            assert_eq!(p, &PathBuf::from("/home/user/.ssh/id_ed25519"));
        } else {
            panic!("expected SshAuth::KeyFile");
        }
    }

    // ── ExecResult construction ───────────────────────────────────────────────

    #[test]
    fn exec_result_fields() {
        let r = ExecResult {
            stdout: "hello\n".into(),
            stderr: String::new(),
            exit_code: 0,
        };
        assert_eq!(r.exit_code, 0);
        assert!(r.stderr.is_empty());
    }

    // ── Base64 write_file encoding ────────────────────────────────────────────

    #[test]
    fn base64_encode_decode_roundtrip() {
        let content = b"[bgp]\nrouter-id 10.0.0.1\n\x00\xFF binary safe";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .expect("decode must succeed");
        assert_eq!(decoded, content);
    }

    #[test]
    fn base64_encode_is_ascii() {
        let content = b"\x00\x01\x02\x03\xFE\xFF";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        assert!(encoded.is_ascii(), "base64 output must be ASCII-safe");
    }

    // ── shell_escape_single ───────────────────────────────────────────────────

    #[test]
    fn shell_escape_plain_string() {
        assert_eq!(shell_escape_single("/etc/frr/frr.conf"), "'/etc/frr/frr.conf'");
    }

    #[test]
    fn shell_escape_string_with_spaces() {
        assert_eq!(shell_escape_single("/path/with spaces/file"), "'/path/with spaces/file'");
    }

    #[test]
    fn shell_escape_string_with_single_quote() {
        // it's  →  'it'"'"'s'
        assert_eq!(shell_escape_single("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn shell_escape_empty_string() {
        assert_eq!(shell_escape_single(""), "''");
    }

    // ── write_file command shape ──────────────────────────────────────────────

    #[test]
    fn write_file_command_contains_base64_and_path() {
        // Manually replicate the command template to verify the shape without
        // needing a live host.
        let content = b"hello world";
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let path = "/etc/frr/frr.conf";
        let parent = "/etc/frr";

        let cmd = format!(
            "mkdir -p {parent} && echo {encoded} | base64 -d > {path}",
            parent = shell_escape_single(parent),
            encoded = encoded,
            path = shell_escape_single(path),
        );

        assert!(cmd.contains("mkdir -p '/etc/frr'"));
        assert!(cmd.contains("base64 -d"));
        assert!(cmd.contains("'/etc/frr/frr.conf'"));
        assert!(cmd.contains(&encoded));
    }

    // ── SshAuth clone ─────────────────────────────────────────────────────────

    #[test]
    fn ssh_auth_clone() {
        let a = SshAuth::Password("pw".into());
        let b = a.clone();
        matches!(b, SshAuth::Password(ref p) if p == "pw");

        let c = SshAuth::KeyFile(PathBuf::from("/key"));
        let d = c.clone();
        matches!(d, SshAuth::KeyFile(ref p) if p == &PathBuf::from("/key"));
    }

    // ── SshConfig clone (needed by parallel_exec) ─────────────────────────────

    #[test]
    fn ssh_config_clone() {
        let original = SshConfig {
            host: "host-a".into(),
            port: 22,
            user: "u".into(),
            auth: SshAuth::Password("p".into()),
            connect_timeout: Duration::from_secs(3),
        };
        let cloned = original.clone();
        assert_eq!(original.host, cloned.host);
        assert_eq!(original.port, cloned.port);
    }
}
