//! RuntimeService — push-config, chaos, pause, resume.
//!
//! Pause/Resume/RunChaos are stubbed — they return `UNIMPLEMENTED` with a clear
//! message. PushConfig is implemented fully.

use tonic::{Request, Response, Status};

use themis_compiler::{expander, loader, renderer};
use themis_proto::{
    runtime_service_server::RuntimeService,
    PauseRequest, PauseResponse,
    PushConfigRequest, PushConfigResponse,
    ResumeRequest, ResumeResponse,
    RunChaosRequest, RunChaosResponse,
};
use themis_runtime::{keys, ssh};

use crate::daemon_deps::DaemonDeps;
use crate::lifecycle::LabState;
use crate::state;

const SSH_USER: &str = "themis";

pub struct RuntimeServiceImpl {
    deps: DaemonDeps,
}

impl RuntimeServiceImpl {
    pub fn new(deps: DaemonDeps) -> Self {
        Self { deps }
    }
}

#[tonic::async_trait]
impl RuntimeService for RuntimeServiceImpl {
    /// Push configs to a running lab's nodes (all or a named subset).
    async fn push_config(
        &self,
        request: Request<PushConfigRequest>,
    ) -> Result<Response<PushConfigResponse>, Status> {
        let req = request.into_inner();
        let lab_name = req.lab.clone();

        let row = state::get_lab(&self.deps.db, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("db: {e}")))?
            .ok_or_else(|| Status::not_found(format!("lab '{lab_name}' not found")))?;

        if row.state != LabState::Running {
            return Err(Status::failed_precondition(format!(
                "lab '{lab_name}' is not Running (state: {:?})",
                row.state
            )));
        }

        let doc = loader::parse_themisfile(&row.themisfile)
            .map_err(|e| Status::invalid_argument(format!("themisfile: {e}")))?;
        let wan = doc.wan_interface.as_deref().unwrap_or("");
        let topology = expander::expand_with_builtins(
            &doc.name, &doc.template, &doc.platform, wan, &doc.parameters,
        )
        .map_err(|e| tonic::Status::from(e))?;

        let rendered = renderer::render_with_builtin_platforms(&topology, &row.platform)
            .map_err(|e| tonic::Status::from(e))?;

        let keypair = keys::load_lab_keypair(&self.deps.paths.keys_dir, &lab_name)
            .await
            .map_err(|e| Status::internal(format!("load keypair: {e}")))?;

        let mut nodes_updated: u32 = 0;
        for node_config in &rendered.nodes {
            let node_name = &node_config.node_name;

            // Filter to requested nodes if specified.
            if !req.nodes.is_empty() && !req.nodes.contains(node_name) {
                continue;
            }

            let node = match topology.nodes.get(node_name) {
                Some(n) => n,
                None => continue,
            };

            let ssh_config = ssh::SshConfig {
                host: node.mgmt_ip.to_string(),
                port: 22,
                user: SSH_USER.to_string(),
                auth: ssh::SshAuth::KeyFile(keypair.private_key_path.clone()),
                connect_timeout: std::time::Duration::from_secs(30),
            };

            match push_node_configs(&ssh_config, node_config, &row.platform).await {
                Ok(()) => {
                    nodes_updated += 1;
                    tracing::info!(lab = %lab_name, node = %node_name, "config pushed");
                }
                Err(e) => {
                    tracing::warn!(lab = %lab_name, node = %node_name, "push failed: {e}");
                    // Continue with other nodes — report partial success.
                }
            }
        }

        Ok(Response::new(PushConfigResponse { nodes_updated }))
    }

    /// Run a chaos scenario. **Deferred — Phase 10.**
    async fn run_chaos(
        &self,
        _request: Request<RunChaosRequest>,
    ) -> Result<Response<RunChaosResponse>, Status> {
        Err(Status::unimplemented(
            "RunChaos is not yet implemented — deferred to Phase 10 (Chaos DSL)",
        ))
    }

    /// Pause a running lab. **Deferred.**
    async fn pause(
        &self,
        _request: Request<PauseRequest>,
    ) -> Result<Response<PauseResponse>, Status> {
        Err(Status::unimplemented(
            "Pause is not yet implemented",
        ))
    }

    /// Resume a paused lab. **Deferred.**
    async fn resume(
        &self,
        _request: Request<ResumeRequest>,
    ) -> Result<Response<ResumeResponse>, Status> {
        Err(Status::unimplemented(
            "Resume is not yet implemented",
        ))
    }
}

async fn push_node_configs(
    ssh_config: &ssh::SshConfig,
    node_config: &renderer::NodeConfig,
    platform_name: &str,
) -> Result<(), String> {
    let mut client = ssh::SshClient::connect(ssh_config)
        .await
        .map_err(|e| e.to_string())?;

    for (path, content) in &node_config.files {
        client
            .write_file(path, content.as_bytes())
            .await
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }

    let reload_cmd = find_reload_command(platform_name);
    let result = client
        .exec(&reload_cmd)
        .await
        .map_err(|e| format!("exec reload: {e}"))?;

    if result.exit_code != 0 {
        return Err(format!(
            "reload '{}' exited {}: {}",
            reload_cmd, result.exit_code, result.stderr.trim()
        ));
    }

    client.disconnect().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn find_reload_command(platform_name: &str) -> String {
    let platforms = themis_platforms::builtin();
    platforms
        .iter()
        .find(|p| p.name() == platform_name)
        .map(|p| p.reload_command().to_string())
        .unwrap_or_else(|| "true".to_string())
}
