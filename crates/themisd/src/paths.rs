//! XDG path resolution for themisd.
//!
//! Convention:
//!   Socket:     $XDG_RUNTIME_DIR/themisd.sock
//!               fallback: /tmp/themisd-<uid>.sock
//!   State DB:   $XDG_STATE_HOME/themisd/state.db
//!               fallback: ~/.local/state/themisd/state.db
//!   Logs:       $XDG_STATE_HOME/themisd/logs/
//!   SSH keys:   $XDG_STATE_HOME/themisd/keys/
//!   Cache:      $XDG_CACHE_HOME/themisd/<lab>/
//!               fallback: ~/.cache/themisd/<lab>/
//!   Golden img: $XDG_DATA_HOME/themisd/golden-images/<platform>.qcow2
//!               fallback: ~/.local/share/themisd/golden-images/<platform>.qcow2

use std::path::PathBuf;

/// All runtime-resolved paths used by the daemon.
#[derive(Debug, Clone)]
pub struct DaemonPaths {
    /// Unix socket path for the gRPC server.
    pub socket: PathBuf,
    /// SQLite database file.
    pub state_db: PathBuf,
    /// Directory for rotated daemon logs.
    pub log_dir: PathBuf,
    /// Base directory for per-lab SSH keypairs.
    pub keys_dir: PathBuf,
    /// Base directory for per-lab cache (disks, seed ISOs).
    pub cache_dir: PathBuf,
    /// Directory containing golden base images.
    pub golden_images_dir: PathBuf,
}

impl DaemonPaths {
    /// Resolve all paths from the current environment.
    pub fn from_env() -> Self {
        let runtime_dir = runtime_dir();
        let state_home = state_home();
        let cache_home = cache_home();
        let data_home = data_home();

        let themisd_state = state_home.join("themisd");
        let themisd_cache = cache_home.join("themisd");
        let themisd_data = data_home.join("themisd");

        Self {
            socket: runtime_dir.join("themisd.sock"),
            state_db: themisd_state.join("state.db"),
            log_dir: themisd_state.join("logs"),
            keys_dir: themisd_state.join("keys"),
            cache_dir: themisd_cache,
            golden_images_dir: themisd_data.join("golden-images"),
        }
    }

    /// Per-lab cache directory: `<cache_dir>/<lab_name>/`.
    pub fn lab_cache_dir(&self, lab_name: &str) -> PathBuf {
        self.cache_dir.join(lab_name)
    }

    /// Per-lab seed ISO directory: `<cache_dir>/<lab_name>/seeds/`.
    pub fn lab_seed_dir(&self, lab_name: &str) -> PathBuf {
        self.lab_cache_dir(lab_name).join("seeds")
    }

    /// Per-lab disk directory: `<cache_dir>/<lab_name>/disks/`.
    pub fn lab_disk_dir(&self, lab_name: &str) -> PathBuf {
        self.lab_cache_dir(lab_name).join("disks")
    }

    /// Golden image path for a given platform:
    /// `<golden_images_dir>/<platform>.qcow2`.
    pub fn golden_image(&self, platform: &str) -> PathBuf {
        self.golden_images_dir.join(format!("{platform}.qcow2"))
    }
}

// ── XDG resolution helpers ─────────────────────────────────────────────────────

fn runtime_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d);
    }
    // Fallback: /tmp/themisd-<uid>
    let uid = effective_uid();
    PathBuf::from(format!("/tmp/themisd-{uid}"))
}

fn state_home() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(d);
    }
    home_dir().join(".local").join("state")
}

fn cache_home() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(d);
    }
    home_dir().join(".cache")
}

fn data_home() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(d);
    }
    home_dir().join(".local").join("share")
}

fn home_dir() -> PathBuf {
    // Use HOME env first, then getpwuid as last resort.
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h);
    }
    PathBuf::from("/tmp")
}

/// Return the effective UID of the current process.
fn effective_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid() has no preconditions and never fails.
        extern "C" {
            fn getuid() -> u32;
        }
        // The extern fn is unsafe to call.
        unsafe { getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_cache_dir_includes_lab_name() {
        let paths = DaemonPaths {
            socket: PathBuf::from("/run/themisd.sock"),
            state_db: PathBuf::from("/state/state.db"),
            log_dir: PathBuf::from("/state/logs"),
            keys_dir: PathBuf::from("/state/keys"),
            cache_dir: PathBuf::from("/cache/themisd"),
            golden_images_dir: PathBuf::from("/data/golden-images"),
        };

        let lab_cache = paths.lab_cache_dir("my-lab");
        assert_eq!(lab_cache, PathBuf::from("/cache/themisd/my-lab"));
    }

    #[test]
    fn lab_seed_dir_is_under_cache() {
        let paths = DaemonPaths {
            socket: PathBuf::from("/run/themisd.sock"),
            state_db: PathBuf::from("/state/state.db"),
            log_dir: PathBuf::from("/state/logs"),
            keys_dir: PathBuf::from("/state/keys"),
            cache_dir: PathBuf::from("/cache/themisd"),
            golden_images_dir: PathBuf::from("/data/golden-images"),
        };

        assert_eq!(
            paths.lab_seed_dir("my-lab"),
            PathBuf::from("/cache/themisd/my-lab/seeds")
        );
        assert_eq!(
            paths.lab_disk_dir("my-lab"),
            PathBuf::from("/cache/themisd/my-lab/disks")
        );
    }

    #[test]
    fn golden_image_has_qcow2_extension() {
        let paths = DaemonPaths {
            socket: PathBuf::from("/run/themisd.sock"),
            state_db: PathBuf::from("/state/state.db"),
            log_dir: PathBuf::from("/state/logs"),
            keys_dir: PathBuf::from("/state/keys"),
            cache_dir: PathBuf::from("/cache/themisd"),
            golden_images_dir: PathBuf::from("/data/golden-images"),
        };

        assert_eq!(
            paths.golden_image("frr-fedora"),
            PathBuf::from("/data/golden-images/frr-fedora.qcow2")
        );
    }

    #[test]
    fn from_env_with_xdg_vars() {
        // Temporarily set XDG vars and verify they're respected.
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        std::env::set_var("XDG_STATE_HOME", "/custom/state");
        std::env::set_var("XDG_CACHE_HOME", "/custom/cache");
        std::env::set_var("XDG_DATA_HOME", "/custom/data");

        let paths = DaemonPaths::from_env();

        // Clean up before any asserts that might panic.
        std::env::remove_var("XDG_RUNTIME_DIR");
        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::remove_var("XDG_DATA_HOME");

        assert_eq!(paths.socket, PathBuf::from("/run/user/1000/themisd.sock"));
        assert_eq!(paths.state_db, PathBuf::from("/custom/state/themisd/state.db"));
        assert_eq!(paths.log_dir, PathBuf::from("/custom/state/themisd/logs"));
        assert_eq!(paths.keys_dir, PathBuf::from("/custom/state/themisd/keys"));
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("/custom/cache/themisd")
        );
        assert_eq!(
            paths.golden_images_dir,
            PathBuf::from("/custom/data/themisd/golden-images")
        );
    }
}
