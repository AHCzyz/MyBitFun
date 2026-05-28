//! Runtime capability API

use crate::api::app_state::AppState;
use bitfun_core::runtime_ports::registry::get_global_runtime_registry;
use bitfun_core::service::runtime::{RuntimeCommandCapability, RuntimeManager};
use tauri::State;

#[tauri::command]
pub async fn get_runtime_capabilities(
    _state: State<'_, AppState>,
) -> Result<Vec<RuntimeCommandCapability>, String> {
    let manager = RuntimeManager::new().map_err(|e| e.to_string())?;
    Ok(manager.get_capabilities())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeDto {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub available: bool,
    pub error: Option<String>,
    pub supports_steer: bool,
    pub supports_thinking: bool,
    pub autonomous_tools: bool,
}

#[tauri::command]
pub async fn list_agent_runtimes() -> Result<Vec<AgentRuntimeDto>, String> {
    let registry = get_global_runtime_registry();
    let registered = registry.list_all();
    log::info!("[runtime_api] registry has {} runtimes", registered.len());

    if registered.is_empty() {
        log::warn!("[runtime_api] registry empty, returning fallback");
        return Ok(fallback_runtimes());
    }

    // Health check each runtime individually — one failure must not break the rest.
    let mut result = Vec::new();
    for rt in registered.iter() {
        let (available, error) = match rt.health_check().await {
            Ok(()) => {
                log::info!("[runtime_api] health_check {} => OK", rt.id());
                (true, None)
            }
            Err(e) => {
                log::info!("[runtime_api] health_check {} => ERR: {}", rt.id(), e.message);
                (false, Some(e.message.clone()))
            }
        };
        result.push(AgentRuntimeDto {
            id: rt.id().to_string(),
            display_name: rt.display_name().to_string(),
            description: rt.capabilities().description.clone(),
            available,
            error,
            supports_steer: rt.capabilities().supports_steer,
            supports_thinking: rt.capabilities().supports_thinking,
            autonomous_tools: rt.capabilities().autonomous_tools,
        });
    }
    log::info!("[runtime_api] returning {} runtimes", result.len());
    Ok(result)
}

fn fallback_runtimes() -> Vec<AgentRuntimeDto> {
    vec![
        AgentRuntimeDto {
            id: "bitfun".into(),
            display_name: "BitFun Native".into(),
            description: "BitFun built-in agent runtime".into(),
            available: true,
            error: None,
            supports_steer: false,
            supports_thinking: true,
            autonomous_tools: false,
        },
        AgentRuntimeDto {
            id: "omp".into(),
            display_name: "OMP (Oh My Pi)".into(),
            description: "OMP agent runtime via RPC subprocess".into(),
            available: false,
            error: Some("bundled binary not found".into()),
            supports_steer: true,
            supports_thinking: true,
            autonomous_tools: true,
        },
        AgentRuntimeDto {
            id: "claude".into(),
            display_name: "Claude Agent SDK".into(),
            description: "Claude Agent SDK via Node.js bridge".into(),
            available: false,
            error: Some("SDK not found".into()),
            supports_steer: false,
            supports_thinking: true,
            autonomous_tools: true,
        },
    ]
}

#[tauri::command]
pub async fn get_default_agent_runtime() -> Result<AgentRuntimeDto, String> {
    let runtimes = list_agent_runtimes().await?;
    let default = runtimes.iter().find(|r| r.available)
        .unwrap_or_else(|| runtimes.first().unwrap());
    Ok(default.clone())
}
