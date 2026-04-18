//! Shared daemon dependencies threaded through deploy/destroy operations.

use std::sync::Arc;
use tokio::sync::Notify;

use crate::events::EventHub;
use crate::lifecycle::LabLocks;
use crate::paths::DaemonPaths;
use crate::state::DbPool;

/// All shared dependencies that service handlers and background tasks need.
///
/// Cheap to clone — every field is already wrapped in `Arc`.
#[derive(Clone)]
pub struct DaemonDeps {
    /// SQLite access layer.
    pub db: DbPool,
    /// Event broadcast hub.
    pub hub: EventHub,
    /// Per-lab operation mutexes.
    pub locks: LabLocks,
    /// Resolved XDG paths.
    pub paths: DaemonPaths,
    /// Set when the daemon is ready (reconciliation complete).
    pub ready: Arc<Notify>,
    /// Signalled when a graceful shutdown is requested.
    pub shutdown: Arc<tokio::sync::Notify>,
    /// Whether the daemon has completed startup reconciliation.
    pub is_ready: Arc<std::sync::atomic::AtomicBool>,
}

impl DaemonDeps {
    pub fn new(db: DbPool, paths: DaemonPaths) -> Self {
        Self {
            db,
            hub: EventHub::new(1024),
            locks: LabLocks::new(),
            paths,
            ready: Arc::new(Notify::new()),
            shutdown: Arc::new(tokio::sync::Notify::new()),
            is_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark the daemon as ready and notify any waiters.
    pub fn mark_ready(&self) {
        self.is_ready
            .store(true, std::sync::atomic::Ordering::Release);
        self.ready.notify_waiters();
    }

    pub fn ready(&self) -> bool {
        self.is_ready.load(std::sync::atomic::Ordering::Acquire)
    }
}
