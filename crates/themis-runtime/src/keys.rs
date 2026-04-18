//! Per-lab SSH key generation and lookup.
//!
//! Every lab gets its own ed25519 keypair, stored under
//! `$XDG_STATE_HOME/themisd/keys/<lab>/`. The public key is injected into
//! each guest VM via cloud-init; the private key is used by themisd (and
//! the CLI, when the user runs `themis ssh <node>`) to authenticate into
//! guests.
//!
//! Key generation shells out to `ssh-keygen` to avoid coupling to a
//! specific Rust crypto crate and to produce files in the exact OpenSSH
//! format every tool already knows how to consume.

use std::path::{Path, PathBuf};

use tokio::process::Command;
use tracing::{debug, instrument};

use themis_core::{Error, Result};

/// A resolved keypair on disk.
#[derive(Debug, Clone)]
pub struct LabKeyPair {
    /// Path to the private key file (OpenSSH format, no passphrase).
    pub private_key_path: PathBuf,
    /// Path to the public key file (OpenSSH format).
    pub public_key_path: PathBuf,
    /// Contents of the public key (ready to be put into `authorized_keys`).
    pub public_key: String,
}

/// Return the directory where a lab's keys live:
/// `<base_dir>/<lab_name>/`. The base dir is typically
/// `$XDG_STATE_HOME/themisd/keys/`.
pub fn lab_key_dir(base_dir: &Path, lab_name: &str) -> PathBuf {
    base_dir.join(lab_name)
}

/// Generate a new ed25519 SSH keypair for `lab_name` inside `base_dir`.
///
/// Creates the lab key directory if it does not exist. Writes `id_ed25519`
/// and `id_ed25519.pub`. Returns the resolved [`LabKeyPair`].
///
/// If keys already exist at the expected paths, this function returns the
/// existing keypair without regenerating.
#[instrument(skip(base_dir), fields(lab = lab_name))]
pub async fn ensure_lab_keypair(base_dir: &Path, lab_name: &str) -> Result<LabKeyPair> {
    let dir = lab_key_dir(base_dir, lab_name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| Error::Runtime(format!("could not create key dir {}: {e}", dir.display())))?;

    let private = dir.join("id_ed25519");
    let public = dir.join("id_ed25519.pub");

    if private.is_file() && public.is_file() {
        debug!(path = %private.display(), "re-using existing lab keypair");
        let pub_bytes = tokio::fs::read_to_string(&public)
            .await
            .map_err(|e| Error::Runtime(format!("read public key: {e}")))?;
        return Ok(LabKeyPair {
            private_key_path: private,
            public_key_path: public,
            public_key: pub_bytes.trim().to_string(),
        });
    }

    let private_str = private
        .to_str()
        .ok_or_else(|| Error::Runtime("private key path is not UTF-8".into()))?;

    let output = Command::new("ssh-keygen")
        .args([
            "-t", "ed25519",
            "-f", private_str,
            "-N", "",
            "-C", &format!("themis-lab-{}", lab_name),
            "-q",
        ])
        .output()
        .await
        .map_err(|e| Error::Runtime(format!("failed to spawn ssh-keygen: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Runtime(format!(
            "ssh-keygen failed for lab {lab_name}: {}",
            stderr.trim()
        )));
    }

    let pub_bytes = tokio::fs::read_to_string(&public)
        .await
        .map_err(|e| Error::Runtime(format!("read freshly generated public key: {e}")))?;

    debug!(path = %private.display(), "generated new lab keypair");

    Ok(LabKeyPair {
        private_key_path: private,
        public_key_path: public,
        public_key: pub_bytes.trim().to_string(),
    })
}

/// Look up an existing lab keypair without generating one.
///
/// Returns `Err(Error::Runtime)` if the keys do not exist.
pub async fn load_lab_keypair(base_dir: &Path, lab_name: &str) -> Result<LabKeyPair> {
    let dir = lab_key_dir(base_dir, lab_name);
    let private = dir.join("id_ed25519");
    let public = dir.join("id_ed25519.pub");

    if !private.is_file() || !public.is_file() {
        return Err(Error::Runtime(format!(
            "no keypair found for lab '{lab_name}' in {}",
            dir.display()
        )));
    }

    let pub_bytes = tokio::fs::read_to_string(&public)
        .await
        .map_err(|e| Error::Runtime(format!("read public key: {e}")))?;

    Ok(LabKeyPair {
        private_key_path: private,
        public_key_path: public,
        public_key: pub_bytes.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_key_dir_joins_paths() {
        let d = lab_key_dir(Path::new("/var/lib/themisd/keys"), "alpha");
        assert_eq!(d, PathBuf::from("/var/lib/themisd/keys/alpha"));
    }

    #[tokio::test]
    async fn load_missing_keypair_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_lab_keypair(tmp.path(), "does-not-exist").await.unwrap_err();
        match err {
            Error::Runtime(msg) => assert!(msg.contains("no keypair")),
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    // `ensure_lab_keypair` invokes `ssh-keygen`, which requires the binary on
    // PATH in the test environment. We gate it with `#[ignore]` by default so
    // CI isn't dependent on OpenSSH being installed.
    #[tokio::test]
    #[ignore]
    async fn ensure_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let kp1 = ensure_lab_keypair(tmp.path(), "my-lab").await.unwrap();
        assert!(kp1.private_key_path.is_file());
        assert!(kp1.public_key_path.is_file());
        assert!(kp1.public_key.starts_with("ssh-ed25519"));

        // Second call should load, not regenerate.
        let kp2 = ensure_lab_keypair(tmp.path(), "my-lab").await.unwrap();
        assert_eq!(kp1.public_key, kp2.public_key);
    }
}
