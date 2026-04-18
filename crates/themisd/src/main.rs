//! themisd — Themis userspace daemon.
//!
//! Entry point. Responsibilities:
//!   1. Tracing init.
//!   2. XDG path resolution.
//!   3. SQLite open + schema init.
//!   4. Startup reconciliation.
//!   5. gRPC server bind on Unix socket.
//!   6. Signal handling (SIGTERM / SIGINT) + DaemonService::Shutdown.
//!   7. Graceful shutdown (drain in-flight ops, close socket, cleanup).

mod daemon_deps;
mod deploy;
mod destroy;
mod events;
mod lifecycle;
mod paths;
mod reconcile;
mod service;
mod state;

use std::sync::Arc;

use anyhow::{Context, Result};
use tonic::transport::Server;
use tracing::{error, info};

use themis_proto::{
    daemon_service_server::DaemonServiceServer,
    lab_service_server::LabServiceServer,
    runtime_service_server::RuntimeServiceServer,
    stream_service_server::StreamServiceServer,
};

use daemon_deps::DaemonDeps;
use paths::DaemonPaths;
use service::{
    daemon::DaemonServiceImpl,
    lab::LabServiceImpl,
    runtime::RuntimeServiceImpl,
    stream::StreamServiceImpl,
};

#[tokio::main]
async fn main() -> Result<()> {
    // ── Tracing ───────────────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("themisd {} starting", env!("CARGO_PKG_VERSION"));

    // ── XDG paths ─────────────────────────────────────────────────────────────
    let paths = DaemonPaths::from_env();
    info!(socket = %paths.socket.display(), db = %paths.state_db.display(), "paths resolved");

    // Ensure critical directories exist.
    for dir in [&paths.log_dir, &paths.keys_dir, &paths.cache_dir] {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("create dir '{}'", dir.display()))?;
    }

    // ── SQLite ────────────────────────────────────────────────────────────────
    let db = state::DbPool::open(&paths.state_db)
        .await
        .context("open SQLite database")?;

    info!(path = %paths.state_db.display(), "SQLite open");

    // ── Daemon dependencies ───────────────────────────────────────────────────
    let deps = DaemonDeps::new(db, paths.clone());

    // ── Startup reconciliation ────────────────────────────────────────────────
    reconcile::reconcile_all(&deps.db, &deps.hub).await;
    deps.mark_ready();
    info!("daemon ready");

    // ── Unix socket listener ──────────────────────────────────────────────────
    let socket_path = paths.socket.clone();

    // Remove stale socket file from a previous run.
    if socket_path.exists() {
        tokio::fs::remove_file(&socket_path)
            .await
            .with_context(|| format!("remove stale socket '{}'", socket_path.display()))?;
    }

    // Ensure parent directory of the socket exists (XDG_RUNTIME_DIR should,
    // but the /tmp fallback may not have the subdirectory).
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create socket dir '{}'", parent.display()))?;
    }

    let uds = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("bind Unix socket at '{}'", socket_path.display()))?;

    info!(socket = %socket_path.display(), "gRPC server listening");

    let incoming = tokio_stream::wrappers::UnixListenerStream::new(uds);

    // ── gRPC services ─────────────────────────────────────────────────────────
    let daemon_svc = DaemonServiceServer::new(DaemonServiceImpl::new(deps.clone()));
    let lab_svc = LabServiceServer::new(LabServiceImpl::new(deps.clone()));
    let runtime_svc = RuntimeServiceServer::new(RuntimeServiceImpl::new(deps.clone()));
    let stream_svc = StreamServiceServer::new(StreamServiceImpl::new(deps.clone()));

    // ── Signal handling ───────────────────────────────────────────────────────
    let shutdown_signal = shutdown_signal(deps.shutdown.clone());

    // ── Serve ─────────────────────────────────────────────────────────────────
    let server = Server::builder()
        .add_service(daemon_svc)
        .add_service(lab_svc)
        .add_service(runtime_svc)
        .add_service(stream_svc)
        .serve_with_incoming_shutdown(incoming, shutdown_signal);

    if let Err(e) = server.await {
        error!("gRPC server error: {e}");
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    info!("shutting down");
    if socket_path.exists() {
        let _ = tokio::fs::remove_file(&socket_path).await;
    }
    info!("themisd stopped");
    Ok(())
}

/// Returns a future that resolves when a shutdown signal is received.
/// Listens for SIGTERM, SIGINT, or an internal `Notify` from the Shutdown RPC.
async fn shutdown_signal(notify: Arc<tokio::sync::Notify>) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {
            info!("received SIGTERM");
        }
        _ = sigint.recv() => {
            info!("received SIGINT");
        }
        _ = notify.notified() => {
            info!("shutdown requested via RPC");
        }
    }
}
