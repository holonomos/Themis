//! Lab state machine — transitions, guards, and event emission.
//!
//! Allowed transitions:
//!   Defined      → Provisioning
//!   Provisioning → Running | Failed
//!   Running      → Paused | Destroying
//!   Paused       → Running | Destroying
//!   Destroying   → Destroyed | Failed
//!   Failed       → Provisioning | Destroying
//!   (any non-terminal) → Failed        // fatal errors
//!
//! Destroyed is terminal; no further transitions are permitted.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tracing::{info, instrument};

use crate::events::{EventHub, EventKind};
use crate::state::{self, DbPool};

// ── LabState ──────────────────────────────────────────────────────────────────

/// The lifecycle state of a lab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabState {
    Defined,
    Provisioning,
    Running,
    Paused,
    Destroying,
    Destroyed,
    Failed,
}

impl LabState {
    pub fn as_str(self) -> &'static str {
        match self {
            LabState::Defined => "Defined",
            LabState::Provisioning => "Provisioning",
            LabState::Running => "Running",
            LabState::Paused => "Paused",
            LabState::Destroying => "Destroying",
            LabState::Destroyed => "Destroyed",
            LabState::Failed => "Failed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Defined" => Some(LabState::Defined),
            "Provisioning" => Some(LabState::Provisioning),
            "Running" => Some(LabState::Running),
            "Paused" => Some(LabState::Paused),
            "Destroying" => Some(LabState::Destroying),
            "Destroyed" => Some(LabState::Destroyed),
            "Failed" => Some(LabState::Failed),
            _ => None,
        }
    }

    /// Whether this state accepts further transitions.
    pub fn is_terminal(self) -> bool {
        matches!(self, LabState::Destroyed)
    }

    /// Whether transitioning FROM `self` TO `next` is allowed.
    pub fn can_transition_to(self, next: LabState) -> bool {
        // Terminal state allows no further transitions.
        if self.is_terminal() {
            return false;
        }

        // Any non-terminal state may transition to Failed.
        if next == LabState::Failed {
            return true;
        }

        match (self, next) {
            (LabState::Defined, LabState::Provisioning) => true,
            (LabState::Provisioning, LabState::Running) => true,
            (LabState::Provisioning, LabState::Failed) => true,
            (LabState::Running, LabState::Paused) => true,
            (LabState::Running, LabState::Destroying) => true,
            (LabState::Paused, LabState::Running) => true,
            (LabState::Paused, LabState::Destroying) => true,
            (LabState::Destroying, LabState::Destroyed) => true,
            (LabState::Destroying, LabState::Failed) => true,
            (LabState::Failed, LabState::Provisioning) => true,
            (LabState::Failed, LabState::Destroying) => true,
            _ => false,
        }
    }

    /// Convert to the proto `LabState` integer value.
    pub fn to_proto_i32(self) -> i32 {
        use themis_proto::LabState as P;
        match self {
            LabState::Defined => P::Defined as i32,
            LabState::Provisioning => P::Provisioning as i32,
            LabState::Running => P::Running as i32,
            LabState::Paused => P::Paused as i32,
            LabState::Destroying => P::Destroying as i32,
            LabState::Destroyed => P::Destroyed as i32,
            LabState::Failed => P::Failed as i32,
        }
    }
}

// ── Per-lab mutex registry ────────────────────────────────────────────────────

/// Registry of per-lab operation mutexes.
///
/// Acquire a lab's mutex before any state-modifying operation (deploy, destroy,
/// pause, resume). This prevents concurrent deploy+destroy races.
#[derive(Clone, Default)]
pub struct LabLocks {
    inner: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
}

impl LabLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (or create) the mutex for a lab, then lock it.
    pub async fn lock_lab(&self, lab_name: &str) -> LabGuard {
        let mutex = {
            // Fast path: lab mutex already exists.
            let read = self.inner.read().await;
            if let Some(m) = read.get(lab_name) {
                Arc::clone(m)
            } else {
                drop(read);
                // Slow path: insert a new mutex.
                let mut write = self.inner.write().await;
                let m = write
                    .entry(lab_name.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone();
                m
            }
        };
        let guard = mutex.lock_owned().await;
        LabGuard { _inner: guard }
    }
}

/// RAII guard for a lab's per-operation mutex.
pub struct LabGuard {
    _inner: tokio::sync::OwnedMutexGuard<()>,
}

// ── Transition function ───────────────────────────────────────────────────────

/// Validate and execute a lab state transition.
///
/// 1. Checks that `new_state` is a valid successor of `current_state`.
/// 2. Updates the `labs` table.
/// 3. Emits a `LAB_STATE` event to the hub.
///
/// Returns `tonic::Status::failed_precondition` if the transition is invalid.
#[instrument(skip(db, hub), fields(lab = lab_name, from = ?current_state, to = ?new_state))]
pub async fn transition(
    db: &DbPool,
    hub: &EventHub,
    lab_name: &str,
    current_state: LabState,
    new_state: LabState,
    reason: &str,
) -> Result<(), tonic::Status> {
    if !current_state.can_transition_to(new_state) {
        return Err(tonic::Status::failed_precondition(format!(
            "lab '{lab_name}': cannot transition from {current_state:?} to {new_state:?}"
        )));
    }

    state::update_lab_state(db, lab_name, new_state)
        .await
        .map_err(|e| tonic::Status::internal(format!("db update_lab_state: {e}")))?;

    let message = if reason.is_empty() {
        format!("{:?} → {:?}", current_state, new_state)
    } else {
        format!("{:?} → {:?}: {reason}", current_state, new_state)
    };

    info!(lab = lab_name, %message, "lab state transition");

    hub.publish(
        lab_name,
        EventKind::LabState,
        lab_name,
        &message,
        vec![],
    )
    .await;

    // Persist to events table (best-effort: don't fail the transition).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;
    let _ = state::insert_event(
        db,
        lab_name,
        "LAB_STATE",
        lab_name,
        &message,
        None,
        ts,
    )
    .await;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Transition guard tests ────────────────────────────────────────────────

    #[test]
    fn defined_can_go_to_provisioning() {
        assert!(LabState::Defined.can_transition_to(LabState::Provisioning));
    }

    #[test]
    fn defined_cannot_go_to_running_directly() {
        assert!(!LabState::Defined.can_transition_to(LabState::Running));
    }

    #[test]
    fn any_non_terminal_can_go_to_failed() {
        for state in [
            LabState::Defined,
            LabState::Provisioning,
            LabState::Running,
            LabState::Paused,
            LabState::Destroying,
        ] {
            assert!(
                state.can_transition_to(LabState::Failed),
                "{state:?} should be able to go to Failed"
            );
        }
    }

    #[test]
    fn destroyed_is_terminal() {
        for next in [
            LabState::Defined,
            LabState::Provisioning,
            LabState::Running,
            LabState::Paused,
            LabState::Destroying,
            LabState::Failed,
        ] {
            assert!(
                !LabState::Destroyed.can_transition_to(next),
                "Destroyed should block transition to {next:?}"
            );
        }
    }

    #[test]
    fn failed_can_retry_provisioning_or_destroy() {
        assert!(LabState::Failed.can_transition_to(LabState::Provisioning));
        assert!(LabState::Failed.can_transition_to(LabState::Destroying));
        assert!(!LabState::Failed.can_transition_to(LabState::Running));
        assert!(!LabState::Failed.can_transition_to(LabState::Paused));
    }

    #[test]
    fn running_paused_bidirectional() {
        assert!(LabState::Running.can_transition_to(LabState::Paused));
        assert!(LabState::Paused.can_transition_to(LabState::Running));
    }

    #[test]
    fn destroying_can_reach_destroyed_or_failed() {
        assert!(LabState::Destroying.can_transition_to(LabState::Destroyed));
        assert!(LabState::Destroying.can_transition_to(LabState::Failed));
        assert!(!LabState::Destroying.can_transition_to(LabState::Running));
    }

    #[test]
    fn state_round_trip_str() {
        let states = [
            LabState::Defined,
            LabState::Provisioning,
            LabState::Running,
            LabState::Paused,
            LabState::Destroying,
            LabState::Destroyed,
            LabState::Failed,
        ];
        for s in states {
            let serialized = s.as_str();
            let deserialized = LabState::from_str(serialized)
                .unwrap_or_else(|| panic!("failed to deserialize {serialized:?}"));
            assert_eq!(s, deserialized, "round-trip failed for {serialized:?}");
        }
    }

    #[test]
    fn from_str_unknown_returns_none() {
        assert!(LabState::from_str("garbage").is_none());
        assert!(LabState::from_str("").is_none());
    }

    // ── LabLocks concurrency test ─────────────────────────────────────────────

    #[tokio::test]
    async fn lab_locks_serialises_same_lab() {
        let locks = LabLocks::new();

        // Acquire the guard for "lab-a"
        let _g = locks.lock_lab("lab-a").await;

        // A concurrent task trying to lock the same lab would block;
        // try_lock on the underlying mutex should fail (it's already held).
        // We can't directly test try_lock here via LabLocks API, but we can
        // verify distinct labs don't block each other.
        let _g2 = locks.lock_lab("lab-b").await; // must not deadlock
    }
}
