//! BitFun native runtime adapter.
//!
//! Wraps the in-process AgenticSystem so it can present the same
//! AgentRuntime / AgentSession interface that external runtimes (OMP, Claude)
//! expose through the runtime-ports abstraction.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use bitfun_runtime_ports::agent_runtime::{
    AgentEvent, AgentEventStream, AgentRuntime, AgentSession, RuntimeCapabilities, SessionConfig,
    StopReason,
};
use bitfun_runtime_ports::{
    AgentInputAttachment, AgentSubmissionPort, AgentSubmissionRequest, AgentSubmissionSource,
    AgentTurnCancellationPort, AgentTurnCancellationRequest, PortError, PortErrorKind, PortResult,
};

use crate::agentic::core::SessionConfig as CoreSessionConfig;
use crate::agentic::events::AgenticEvent;
use crate::agentic::system::AgenticSystem;

// ---------------------------------------------------------------------------
// BitfunRuntime
// ---------------------------------------------------------------------------

/// Wraps the in-process AgenticSystem behind the AgentRuntime trait.
pub struct BitfunRuntime {
    system: Arc<AgenticSystem>,
    capabilities: RuntimeCapabilities,
}

impl BitfunRuntime {
    pub fn new(system: Arc<AgenticSystem>) -> Self {
        Self {
            system,
            capabilities: RuntimeCapabilities {
                description: "BitFun's built-in agent runtime".to_string(),
                supports_steer: false,
                supports_thinking: true,
                autonomous_tools: false,
                extras: HashMap::new(),
            },
        }
    }
}

fn to_port_error(e: crate::util::errors::BitFunError) -> PortError {
    PortError::new(PortErrorKind::Backend, e.to_string())
}

#[async_trait]
impl AgentRuntime for BitfunRuntime {
    fn id(&self) -> &str {
        "bitfun"
    }

    fn display_name(&self) -> &str {
        "BitFun Native"
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    async fn create_session(&self, config: SessionConfig) -> PortResult<Box<dyn AgentSession>> {
        let session_id = Uuid::new_v4().to_string();

        let workspace_path = config.working_dir.clone().ok_or_else(|| {
            PortError::new(
                PortErrorKind::InvalidRequest,
                "working_dir is required to create a BitFun session",
            )
        })?;

        let core_config = CoreSessionConfig {
            workspace_path: Some(workspace_path),
            ..CoreSessionConfig::default()
        };

        self.system
            .coordinator
            .create_session(
                format!("BitFun adapter session {}", &session_id[..8]),
                "bitfun".to_string(),
                core_config,
            )
            .await
            .map_err(to_port_error)?;

        Ok(Box::new(BitfunSession {
            session_id,
            system: self.system.clone(),
            working_dir: config.working_dir,
        }))
    }

    async fn health_check(&self) -> PortResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// BitfunSession
// ---------------------------------------------------------------------------

pub struct BitfunSession {
    session_id: String,
    system: Arc<AgenticSystem>,
    #[allow(dead_code)]
    working_dir: Option<String>,
}

impl BitfunSession {
    /// Extract the turn_id (turn_id) from an AgenticEvent, if present.
    fn get_turn_id(event: &AgenticEvent) -> Option<&str> {
        match event {
            AgenticEvent::DialogTurnStarted { turn_id, .. }
            | AgenticEvent::DialogTurnCompleted { turn_id, .. }
            | AgenticEvent::DialogTurnCancelled { turn_id, .. }
            | AgenticEvent::DialogTurnFailed { turn_id, .. }
            | AgenticEvent::ModelRoundStarted { turn_id, .. }
            | AgenticEvent::ModelRoundCompleted { turn_id, .. }
            | AgenticEvent::TextChunk { turn_id, .. }
            | AgenticEvent::ThinkingChunk { turn_id, .. }
            | AgenticEvent::ToolEvent { turn_id, .. }
            | AgenticEvent::UserSteeringInjected { turn_id, .. } => {
                Some(turn_id.as_str())
            }
            _ => None,
        }
    }

    /// Map an AgenticEvent to zero or more AgentEvent variants.
    fn map_event(
        event: &AgenticEvent,
        seen_tool_starts: &mut HashSet<String>,
    ) -> Vec<AgentEvent> {
        match event {
            AgenticEvent::DialogTurnStarted { .. } => {
                vec![AgentEvent::TurnStart {
                    metadata: HashMap::new(),
                }]
            }

            AgenticEvent::TextChunk { text, .. } => {
                vec![AgentEvent::TextDelta {
                    delta: text.clone(),
                    metadata: HashMap::new(),
                }]
            }

            AgenticEvent::ThinkingChunk { content, .. } => {
                vec![AgentEvent::ThinkingDelta {
                    delta: content.clone(),
                    metadata: HashMap::new(),
                }]
            }

            AgenticEvent::ToolEvent { tool_event, .. } => {
                Self::map_tool_event(tool_event, seen_tool_starts)
            }

            AgenticEvent::DialogTurnCompleted { .. } => {
                vec![AgentEvent::TurnEnd {
                    stop_reason: StopReason::Completed,
                    metadata: HashMap::new(),
                }]
            }

            AgenticEvent::DialogTurnCancelled { .. } => {
                vec![AgentEvent::TurnEnd {
                    stop_reason: StopReason::Aborted,
                    metadata: HashMap::new(),
                }]
            }

            AgenticEvent::DialogTurnFailed { error, .. } => {
                vec![AgentEvent::Error {
                    message: error.clone(),
                    metadata: HashMap::new(),
                }]
            }

            AgenticEvent::SystemError { error, .. } => {
                vec![AgentEvent::Error {
                    message: error.clone(),
                    metadata: HashMap::new(),
                }]
            }

            // Non-user-visible events — silently drop.
            _ => vec![],
        }
    }

    fn map_tool_event(
        tool_event: &bitfun_events::agentic::ToolEventData,
        seen_tool_starts: &mut HashSet<String>,
    ) -> Vec<AgentEvent> {
        use bitfun_events::agentic::ToolEventData::*;

        match tool_event {
            EarlyDetected { tool_id, tool_name, .. }
            | Started { tool_id, tool_name, .. } => {
                if seen_tool_starts.insert(tool_id.clone()) {
                    vec![AgentEvent::ToolCallStart {
                        tool_call_id: tool_id.clone(),
                        tool_name: tool_name.clone(),
                        metadata: HashMap::new(),
                    }]
                } else {
                    vec![]
                }
            }

            ParamsPartial { tool_id, params, .. } => {
                vec![AgentEvent::ToolCallDelta {
                    tool_call_id: tool_id.clone(),
                    delta: params.clone(),
                    metadata: HashMap::new(),
                }]
            }

            Progress { tool_id, message, .. } => {
                vec![AgentEvent::ToolCallDelta {
                    tool_call_id: tool_id.clone(),
                    delta: message.clone(),
                    metadata: HashMap::new(),
                }]
            }

            StreamChunk { tool_id, data, .. } => {
                vec![AgentEvent::ToolCallDelta {
                    tool_call_id: tool_id.clone(),
                    delta: serde_json::to_string(data).unwrap_or_default(),
                    metadata: HashMap::new(),
                }]
            }

            Completed { tool_id, result, .. } => {
                vec![AgentEvent::ToolResult {
                    tool_call_id: tool_id.clone(),
                    result: serde_json::to_string(result).unwrap_or_default(),
                    metadata: HashMap::new(),
                }]
            }

            Failed { tool_id, error, .. } => {
                vec![AgentEvent::ToolResult {
                    tool_call_id: tool_id.clone(),
                    result: format!("Error: {}", error),
                    metadata: HashMap::new(),
                }]
            }

            Cancelled { tool_id, reason, .. } => {
                vec![AgentEvent::ToolResult {
                    tool_call_id: tool_id.clone(),
                    result: format!("Cancelled: {}", reason),
                    metadata: HashMap::new(),
                }]
            }

            Queued { .. }
            | Waiting { .. }
            | Streaming { .. }
            | ConfirmationNeeded { .. }
            | Confirmed { .. }
            | Rejected { .. } => vec![],
        }
    }
}

#[async_trait]
impl AgentSession for BitfunSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(
        &self,
        input: &str,
        attachments: Vec<AgentInputAttachment>,
    ) -> PortResult<AgentEventStream> {
        // 1. Subscribe to the event queue BEFORE submitting.
        let mut broadcast_rx = self.system.event_queue.subscribe();

        // 2. Submit the message through the coordinator.
        let submission_result = self
            .system
            .coordinator
            .submit_message(AgentSubmissionRequest {
                session_id: self.session_id.clone(),
                message: input.to_string(),
                turn_id: None,
                source: Some(AgentSubmissionSource::AgentSession),
                attachments,
                metadata: serde_json::Map::new(),
            })
            .await
            ?;

        let turn_id = submission_result.turn_id;

        // 3. Bridge broadcast events into a mpsc stream.
        let (tx, rx) = mpsc::channel::<AgentEvent>(256);
        let session_id = self.session_id.clone();

        tokio::spawn(async move {
            let mut seen_tool_starts: HashSet<String> = HashSet::new();
            let mut turn_ended = false;

            loop {
                match broadcast_rx.recv().await {
                    Ok(envelope) => {
                        let event = &envelope.event;

                        // Filter: only events for our session.
                        let event_session_id = event.session_id().unwrap_or("");
                        if event_session_id != session_id {
                            continue;
                        }

                        // Filter: only events for our turn (if we have a turn_id).
                        if !turn_id.is_empty() {
                            if let Some(event_tid) = Self::get_turn_id(event) {
                                if event_tid != turn_id {
                                    continue;
                                }
                            }
                        }

                        let mapped = Self::map_event(event, &mut seen_tool_starts);

                        for agent_event in mapped {
                            let is_terminal = matches!(
                                &agent_event,
                                AgentEvent::TurnEnd { .. } | AgentEvent::Error { .. }
                            );

                            if tx.send(agent_event).await.is_err() {
                                return; // Receiver dropped
                            }

                            if is_terminal {
                                turn_ended = true;
                            }
                        }

                        if turn_ended {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("BitfunSession broadcast receiver lagged by {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return;
                    }
                }
            }
        });

        // 4. Return the stream.
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn abort(&self) -> PortResult<()> {
        self.system
            .coordinator
            .cancel_turn(AgentTurnCancellationRequest {
                session_id: self.session_id.clone(),
                turn_id: None,
                source: Some(AgentSubmissionSource::AgentSession),
                reason: Some("user abort".to_string()),
                wait_timeout_ms: None,
            })
            .await
            ?;
        Ok(())
    }

    async fn dispose(self: Box<Self>) -> PortResult<()> {
        Ok(())
    }
}
