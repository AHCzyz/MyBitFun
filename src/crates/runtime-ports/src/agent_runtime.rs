//! Agent Runtime Abstraction
//!
//! Defines the trait hierarchy for switching between different agent runtimes.
//! Each runtime (BitFun native, OMP, Claude Agent SDK) implements these traits
//! to provide a unified interface for session management and event streaming.
//!
//! Design principle: autonomous subprocess model (Model C).
//! External runtimes are self-contained black boxes with their own toolchains.
//! BitFun only passes prompts in and receives event streams out.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;

use crate::AgentInputAttachment;
use crate::{PortError, PortResult};

// ---------------------------------------------------------------------------
// AgentRuntime — lifecycle management for a runtime provider
// ---------------------------------------------------------------------------

/// Capabilities a runtime advertises. Used by UI to show feature differences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapabilities {
    /// Human-readable description
    pub description: String,
    /// Runtime supports mid-turn steering
    pub supports_steer: bool,
    /// Runtime emits thinking/reasoning deltas
    pub supports_thinking: bool,
    /// Runtime manages its own tools autonomously
    pub autonomous_tools: bool,
    /// Arbitrary capability flags for runtime-specific features
    #[serde(default)]
    pub extras: HashMap<String, serde_json::Value>,
}

/// Configuration for creating a new agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    /// Optional model override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Working directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Runtime-specific configuration
    #[serde(default)]
    pub runtime_options: HashMap<String, serde_json::Value>,
}

/// A provider of agent sessions. Each runtime (BitFun, OMP, Claude) implements this.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Stable identifier: "bitfun" | "omp" | "claude"
    fn id(&self) -> &str;

    /// Human-readable name for UI display
    fn display_name(&self) -> &str;

    /// What this runtime can do
    fn capabilities(&self) -> &RuntimeCapabilities;

    /// Create a new agent session
    async fn create_session(
        &self,
        config: SessionConfig,
    ) -> PortResult<Box<dyn AgentSession>>;

    /// Check if this runtime is usable (binary found, API key configured, etc.)
    async fn health_check(&self) -> PortResult<()>;

    /// Release runtime-level resources
    async fn shutdown(&self) -> PortResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AgentSession — interaction with a single agent session
// ---------------------------------------------------------------------------

/// A boxed, pinned, Send stream of agent events.
pub type AgentEventStream = Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>>;

/// An active agent session. Created by an AgentRuntime, consumed by the coordinator.
#[async_trait]
pub trait AgentSession: Send + Sync {
    /// Opaque session identifier
    fn session_id(&self) -> &str;

    /// Send a user message and receive streaming events.
    /// The stream ends when the agent turn completes (TurnEnd) or errors.
    async fn prompt(
        &self,
        input: &str,
        attachments: Vec<AgentInputAttachment>,
    ) -> PortResult<AgentEventStream>;

    /// Inject a steering message into the running turn (if supported)
    async fn steer(&self, _message: &str) -> PortResult<()> {
        Err(PortError::new(
            crate::PortErrorKind::NotAvailable,
            "steer not supported by this runtime",
        ))
    }

    /// Abort the current turn
    async fn abort(&self) -> PortResult<()>;

    /// Release session resources
    async fn dispose(self: Box<Self>) -> PortResult<()>;
}

// ---------------------------------------------------------------------------
// AgentEvent — unified event model across all runtimes
// ---------------------------------------------------------------------------

/// Why a turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    Aborted,
    Error,
    ToolLimit,
}

/// Unified event type. Each runtime translates its native events into these.
/// Unmapped runtime-specific data goes into `metadata`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TextDelta {
        delta: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    ThinkingDelta {
        delta: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    ToolCallStart {
        tool_call_id: String,
        tool_name: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    ToolCallDelta {
        tool_call_id: String,
        delta: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    ToolResult {
        tool_call_id: String,
        result: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    TurnStart {
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    TurnEnd {
        stop_reason: StopReason,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
    Error {
        message: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
}

/// Health status of a single runtime at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeHealthStatus {
    pub runtime_id: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
