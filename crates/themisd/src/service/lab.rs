//! LabService — lab lifecycle: define, list, inspect, deploy, destroy, estimate.

use tonic::{Request, Response, Status};

use themis_compiler::{estimator, expander, loader};
use themis_proto::{
    lab_service_server::LabService,
    DefineRequest, DefineResponse,
    DestroyRequest, DestroyResponse,
    DeployRequest, DeployResponse,
    EstimateRequest, EstimateResponse,
    InspectRequest, InspectResponse,
    LabSummary,
    ListRequest, ListResponse,
};

use crate::daemon_deps::DaemonDeps;
use crate::lifecycle::LabState;
use crate::state;

pub struct LabServiceImpl {
    deps: DaemonDeps,
}

impl LabServiceImpl {
    pub fn new(deps: DaemonDeps) -> Self {
        Self { deps }
    }
}

#[tonic::async_trait]
impl LabService for LabServiceImpl {
    /// Define a new lab from a Themisfile (content or path).
    async fn define(
        &self,
        request: Request<DefineRequest>,
    ) -> Result<Response<DefineResponse>, Status> {
        let req = request.into_inner();

        // Resolve content: prefer themisfile_content, fall back to path.
        let content = if !req.themisfile_content.is_empty() {
            req.themisfile_content.clone()
        } else if !req.themisfile_path.is_empty() {
            tokio::fs::read_to_string(&req.themisfile_path)
                .await
                .map_err(|e| {
                    Status::invalid_argument(format!(
                        "cannot read Themisfile at '{}': {e}",
                        req.themisfile_path
                    ))
                })?
        } else {
            return Err(Status::invalid_argument(
                "must supply either themisfile_content or themisfile_path",
            ));
        };

        // Parse to extract metadata; also validates the file is well-formed.
        let doc = loader::parse_themisfile(&content)
            .map_err(|e| Status::invalid_argument(format!("themisfile parse error: {e}")))?;

        let lab_name = doc.name.clone();

        // Check for duplicate.
        if let Some(_existing) = state::get_lab(&self.deps.db, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("db error: {e}")))?
        {
            return Err(Status::already_exists(format!(
                "lab '{lab_name}' already exists"
            )));
        }

        state::insert_lab(
            &self.deps.db,
            &lab_name,
            &doc.template,
            &doc.platform,
            doc.wan_interface.as_deref(),
            &content,
            LabState::Defined,
        )
        .await
        .map_err(|e| Status::internal(format!("db insert_lab: {e}")))?;

        tracing::info!(lab = %lab_name, "lab defined");

        Ok(Response::new(DefineResponse { name: lab_name }))
    }

    /// List all labs.
    async fn list(
        &self,
        _request: Request<ListRequest>,
    ) -> Result<Response<ListResponse>, Status> {
        let labs = state::list_labs(&self.deps.db)
            .await
            .map_err(|e| Status::internal(format!("db list_labs: {e}")))?;

        let mut summaries = Vec::with_capacity(labs.len());
        for lab in &labs {
            let node_count = state::get_nodes(&self.deps.db, &lab.name)
                .await
                .map(|n| n.len() as u32)
                .unwrap_or(0);

            summaries.push(LabSummary {
                name: lab.name.clone(),
                template: lab.template.clone(),
                platform: lab.platform.clone(),
                state: lab.state.to_proto_i32(),
                node_count,
                created_unix: lab.created_unix,
            });
        }

        Ok(Response::new(ListResponse { labs: summaries }))
    }

    /// Inspect a single lab.
    async fn inspect(
        &self,
        request: Request<InspectRequest>,
    ) -> Result<Response<InspectResponse>, Status> {
        let lab_name = request.into_inner().name;

        let row = state::get_lab(&self.deps.db, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("db get_lab: {e}")))?
            .ok_or_else(|| Status::not_found(format!("lab '{lab_name}' not found")))?;

        let node_count = state::get_nodes(&self.deps.db, &lab_name)
            .await
            .map(|n| n.len() as u32)
            .unwrap_or(0);

        let summary = LabSummary {
            name: row.name.clone(),
            template: row.template.clone(),
            platform: row.platform.clone(),
            state: row.state.to_proto_i32(),
            node_count,
            created_unix: row.created_unix,
        };

        // Try to serialize the topology; if the lab isn't deployed yet, this
        // may fail — return empty bytes in that case (not an error).
        let topology_json = try_serialize_topology(&row).await.unwrap_or_default();

        Ok(Response::new(InspectResponse {
            summary: Some(summary),
            topology_json,
        }))
    }

    /// Deploy a lab (async — returns immediately; work happens in background).
    async fn deploy(
        &self,
        request: Request<DeployRequest>,
    ) -> Result<Response<DeployResponse>, Status> {
        let lab_name = request.into_inner().name;

        // Validate that the lab exists before spawning.
        let row = state::get_lab(&self.deps.db, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("db get_lab: {e}")))?
            .ok_or_else(|| Status::not_found(format!("lab '{lab_name}' not found")))?;

        if !matches!(row.state, LabState::Defined | LabState::Failed) {
            return Err(Status::failed_precondition(format!(
                "lab '{lab_name}' is in state {:?}; can only deploy from Defined or Failed",
                row.state
            )));
        }

        let deps = self.deps.clone();
        let lab = lab_name.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::deploy::run(&lab, &deps).await {
                tracing::error!(lab = %lab, error = %e.message(), "deploy task failed");
            }
        });

        tracing::info!(lab = %lab_name, "deploy task spawned");
        Ok(Response::new(DeployResponse {}))
    }

    /// Destroy a lab (async — returns immediately; work happens in background).
    async fn destroy(
        &self,
        request: Request<DestroyRequest>,
    ) -> Result<Response<DestroyResponse>, Status> {
        let lab_name = request.into_inner().name;

        // Validate that the lab exists and is in a destroyable state.
        let row = state::get_lab(&self.deps.db, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("db get_lab: {e}")))?
            .ok_or_else(|| Status::not_found(format!("lab '{lab_name}' not found")))?;

        let destroyable = matches!(
            row.state,
            LabState::Running | LabState::Paused | LabState::Failed | LabState::Destroying
        );
        if !destroyable {
            return Err(Status::failed_precondition(format!(
                "lab '{lab_name}' is in state {:?}; must be Running, Paused, Failed, or Destroying",
                row.state
            )));
        }

        let deps = self.deps.clone();
        let lab = lab_name.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::destroy::run(&lab, &deps).await {
                tracing::error!(lab = %lab, error = %e.message(), "destroy task failed");
            }
        });

        tracing::info!(lab = %lab_name, "destroy task spawned");
        Ok(Response::new(DestroyResponse {}))
    }

    /// Estimate resource requirements for a lab.
    async fn estimate(
        &self,
        request: Request<EstimateRequest>,
    ) -> Result<Response<EstimateResponse>, Status> {
        let lab_name = request.into_inner().name;

        let row = state::get_lab(&self.deps.db, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("db get_lab: {e}")))?
            .ok_or_else(|| Status::not_found(format!("lab '{lab_name}' not found")))?;

        let doc = loader::parse_themisfile(&row.themisfile)
            .map_err(|e| Status::invalid_argument(format!("themisfile parse: {e}")))?;

        let wan_interface = doc.wan_interface.as_deref().unwrap_or("");
        let topology = expander::expand_with_builtins(
            &doc.name,
            &doc.template,
            &doc.platform,
            wan_interface,
            &doc.parameters,
        )
        .map_err(|e| tonic::Status::from(e))?;

        let est = estimator::estimate_with_builtin_platforms(&topology, &row.platform)
            .map_err(|e| tonic::Status::from(e))?;

        Ok(Response::new(EstimateResponse {
            total_vcpu: est.total_vcpu,
            nominal_memory_mb: est.nominal_memory_mb,
            projected_memory_mb_after_ksm: est.projected_memory_mb_after_ksm,
            total_disk_gb: est.total_disk_gb,
        }))
    }
}

/// Try to build + serialize the topology from a lab row's stored Themisfile.
/// Returns Ok(bytes) on success, Ok(vec![]) on any parse/expand failure.
async fn try_serialize_topology(row: &crate::state::LabRow) -> Result<Vec<u8>, ()> {
    let doc = loader::parse_themisfile(&row.themisfile).map_err(|_| ())?;
    let wan = doc.wan_interface.as_deref().unwrap_or("");
    let topology = expander::expand_with_builtins(
        &doc.name,
        &doc.template,
        &doc.platform,
        wan,
        &doc.parameters,
    )
    .map_err(|_| ())?;
    serde_json::to_vec(&topology).map_err(|_| ())
}
