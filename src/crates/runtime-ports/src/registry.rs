//! Runtime Registry
//!
//! Global singleton that holds all registered AgentRuntime implementations.
//! Runtimes are registered at startup; the registry is read-only during operation.

use std::sync::{Arc, OnceLock};

use crate::agent_runtime::{AgentRuntime, RuntimeHealthStatus};

/// Registry of all available agent runtimes.
pub struct RuntimeRegistry {
    runtimes: Vec<Arc<dyn AgentRuntime>>,
    default_id: String,
}

impl std::fmt::Debug for RuntimeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.runtimes.iter().map(|r| r.id()).collect();
        f.debug_struct("RuntimeRegistry")
            .field("runtime_ids", &ids)
            .field("default_id", &self.default_id)
            .finish()
    }
}

impl RuntimeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            runtimes: Vec::new(),
            default_id: "bitfun".to_string(),
        }
    }

    /// Register a runtime. Order determines priority for default selection.
    pub fn register(&mut self, runtime: Arc<dyn AgentRuntime>) {
        self.runtimes.push(runtime);
    }

    /// List all registered runtimes.
    pub fn list_all(&self) -> &[Arc<dyn AgentRuntime>] {
        &self.runtimes
    }

    /// Get a runtime by id.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn AgentRuntime>> {
        self.runtimes.iter().find(|r| r.id() == id)
    }

    /// Check health of all runtimes.
    pub async fn health_check_all(&self) -> Vec<RuntimeHealthStatus> {
        let mut results = Vec::with_capacity(self.runtimes.len());
        for runtime in &self.runtimes {
            let status = match runtime.health_check().await {
                Ok(()) => RuntimeHealthStatus {
                    runtime_id: runtime.id().to_string(),
                    available: true,
                    error: None,
                },
                Err(e) => RuntimeHealthStatus {
                    runtime_id: runtime.id().to_string(),
                    available: false,
                    error: Some(e.message.clone()),
                },
            };
            results.push(status);
        }
        results
    }

    /// Select the default runtime based on priority order and health.
    /// Priority: OMP -> Claude -> BitFun (first healthy wins).
    pub fn select_default(&self, health_statuses: &[RuntimeHealthStatus]) -> &Arc<dyn AgentRuntime> {
        let healthy_ids: Vec<&str> = health_statuses
            .iter()
            .filter(|s| s.available)
            .map(|s| s.runtime_id.as_str())
            .collect();

        // Priority order
        for preferred in &["omp", "claude", "bitfun"] {
            if healthy_ids.contains(preferred) {
                if let Some(rt) = self.get(preferred) {
                    return rt;
                }
            }
        }

        // Fallback: first registered runtime
        self.runtimes
            .first()
            .expect("RuntimeRegistry must have at least one runtime registered")
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_RUNTIME_REGISTRY: OnceLock<RuntimeRegistry> = OnceLock::new();

/// Get the global runtime registry.
pub fn get_global_runtime_registry() -> &'static RuntimeRegistry {
    GLOBAL_RUNTIME_REGISTRY.get_or_init(RuntimeRegistry::new)
}

/// Set the global runtime registry. Call once during startup.
/// Panics if called more than once.
pub fn init_global_runtime_registry(registry: RuntimeRegistry) {
    GLOBAL_RUNTIME_REGISTRY
        .set(registry)
        .expect("RuntimeRegistry already initialized");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::agent_runtime::{AgentSession, RuntimeCapabilities, SessionConfig};
    use crate::{PortError, PortErrorKind, PortResult};
    use std::collections::HashMap;
    use std::sync::LazyLock;

    struct MockRuntime {
        id: &'static str,
        healthy: bool,
    }

    #[async_trait]
    impl AgentRuntime for MockRuntime {
        fn id(&self) -> &str { self.id }
        fn display_name(&self) -> &str { self.id }
        fn capabilities(&self) -> &RuntimeCapabilities {
            static CAPS: LazyLock<RuntimeCapabilities> = LazyLock::new(|| RuntimeCapabilities {
                description: String::new(),
                supports_steer: false,
                supports_thinking: false,
                autonomous_tools: false,
                extras: HashMap::new(),
            });
            &CAPS
        }
        async fn create_session(&self, _: SessionConfig) -> PortResult<Box<dyn AgentSession>> {
            Err(PortError::new(PortErrorKind::NotAvailable, "mock"))
        }
        async fn health_check(&self) -> PortResult<()> {
            if self.healthy { Ok(()) } else { Err(PortError::new(PortErrorKind::NotAvailable, "unhealthy")) }
        }
    }

    #[tokio::test]
    async fn test_registry_get() {
        let mut reg = RuntimeRegistry::new();
        reg.register(Arc::new(MockRuntime { id: "omp", healthy: true }));
        reg.register(Arc::new(MockRuntime { id: "claude", healthy: true }));

        assert_eq!(reg.get("omp").unwrap().id(), "omp");
        assert_eq!(reg.get("claude").unwrap().id(), "claude");
        assert!(reg.get("bitfun").is_none());
    }

    #[tokio::test]
    async fn test_select_default_prefers_omp() {
        let mut reg = RuntimeRegistry::new();
        reg.register(Arc::new(MockRuntime { id: "bitfun", healthy: true }));
        reg.register(Arc::new(MockRuntime { id: "omp", healthy: true }));

        let health = reg.health_check_all().await;
        let default = reg.select_default(&health);
        assert_eq!(default.id(), "omp");
    }

    #[tokio::test]
    async fn test_select_default_falls_back_when_omp_unhealthy() {
        let mut reg = RuntimeRegistry::new();
        reg.register(Arc::new(MockRuntime { id: "bitfun", healthy: true }));
        reg.register(Arc::new(MockRuntime { id: "omp", healthy: false }));

        let health = reg.health_check_all().await;
        let default = reg.select_default(&health);
        assert_eq!(default.id(), "bitfun");
    }
}
