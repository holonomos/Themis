//! DaemonService — health, version, shutdown.

use tonic::{Request, Response, Status};

use themis_proto::{
    daemon_service_server::DaemonService,
    HealthRequest, HealthResponse,
    ShutdownRequest, ShutdownResponse,
    VersionRequest, VersionResponse,
};

use crate::daemon_deps::DaemonDeps;

pub struct DaemonServiceImpl {
    deps: DaemonDeps,
}

impl DaemonServiceImpl {
    pub fn new(deps: DaemonDeps) -> Self {
        Self { deps }
    }
}

#[tonic::async_trait]
impl DaemonService for DaemonServiceImpl {
    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            ready: self.deps.ready(),
        }))
    }

    async fn version(
        &self,
        _request: Request<VersionRequest>,
    ) -> Result<Response<VersionResponse>, Status> {
        Ok(Response::new(VersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_commit: option_env!("GIT_COMMIT").unwrap_or("unknown").to_string(),
        }))
    }

    async fn shutdown(
        &self,
        _request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        tracing::info!("shutdown requested via gRPC");
        self.deps.shutdown.notify_one();
        Ok(Response::new(ShutdownResponse {}))
    }
}
