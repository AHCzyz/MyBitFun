//! Claude Agent SDK runtime adapter.
//!
//! Spawns a Node.js bridge process that wraps @anthropic-ai/claude-agent-sdk.
//! Communication is JSONL over stdio: commands go to stdin, events come from stdout.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use bitfun_runtime_ports::agent_runtime::{
    AgentEvent, AgentRuntime, AgentSession, AgentEventStream, RuntimeCapabilities, SessionConfig,
    StopReason,
};
use bitfun_runtime_ports::{PortError, PortErrorKind, PortResult};

// ── Runtime ──────────────────────────────────────────────────────────────────

pub struct ClaudeRuntime;

impl ClaudeRuntime {
    pub fn new() -> Self {
        Self
    }

    /// Resolve the path to `bridge.mjs` at runtime.
    fn bridge_path() -> PathBuf {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.join("../../..");
        let dev_path = workspace_root.join("resources/claude-bridge/bridge.mjs");
        if dev_path.exists() {
            return dev_path;
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                let bundled = parent.join("resources/claude-bridge/bridge.mjs");
                if bundled.exists() {
                    return bundled;
                }
                let macos = parent.join("../Resources/claude-bridge/bridge.mjs");
                if macos.exists() {
                    return macos;
                }
            }
        }

        dev_path
    }

    /// Resolve the Node.js binary path.
    /// Order: bundled (next to bridge.mjs) → system PATH.
    fn resolve_node_binary() -> Option<PathBuf> {
        let bridge = Self::bridge_path();
        if let Some(bridge_dir) = bridge.parent() {
            let bundled = bridge_dir.join("node");
            if bundled.exists() {
                return Some(bundled);
            }
            #[cfg(windows)]
            {
                let bundled_exe = bridge_dir.join("node.exe");
                if bundled_exe.exists() {
                    return Some(bundled_exe);
                }
            }
        }

        which::which("node").ok()
    }
}

#[async_trait]
impl AgentRuntime for ClaudeRuntime {
    fn id(&self) -> &str {
        "claude"
    }

    fn display_name(&self) -> &str {
        "Claude Agent SDK"
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        // Stored as a static to avoid allocating on every call
        static CAPS: std::sync::OnceLock<RuntimeCapabilities> = std::sync::OnceLock::new();
        CAPS.get_or_init(|| RuntimeCapabilities {
            description: "Claude Agent SDK via Node.js bridge".into(),
            supports_steer: false,
            supports_thinking: true,
            autonomous_tools: true,
            extras: HashMap::new(),
        })
    }

    async fn create_session(
        &self,
        config: SessionConfig,
    ) -> PortResult<Box<dyn AgentSession>> {
        let bridge = Self::bridge_path();
        if !bridge.exists() {
            return Err(PortError::new(
                PortErrorKind::NotFound,
                format!(
                    "Claude bridge script not found at {}",
                    bridge.display()
                ),
            ));
        }
        let node_binary = Self::resolve_node_binary().ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotAvailable,
                "Node.js not found: not bundled and not in PATH".to_string(),
            )
        })?;
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            return Err(PortError::new(
                PortErrorKind::PermissionDenied,
                "ANTHROPIC_API_KEY environment variable not set. Set it to use the Claude Agent SDK runtime.".to_string(),
            ));
        }

        let working_dir = config
            .working_dir
            .as_deref()
            .unwrap_or(".")
            .to_string();

        let mut child = Command::new(&node_binary)
            .arg(bridge.to_str().ok_or_else(|| {
                PortError::new(
                    PortErrorKind::Backend,
                    "bridge path contains invalid UTF-8",
                )
            })?)
            .current_dir(&working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| {
                PortError::new(
                    PortErrorKind::Backend,
                    format!("Failed to spawn Node.js bridge process: {e}"),
                )
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            PortError::new(PortErrorKind::Backend, "bridge did not provide stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            PortError::new(PortErrorKind::Backend, "bridge did not provide stdout")
        })?;

        let session_id = uuid::Uuid::new_v4().to_string();
        let abort_token = CancellationToken::new();
        let abort_clone = abort_token.clone();

        // Shared channel sender — the background reader task routes events here
        let event_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<AgentEvent>>>> =
            Arc::new(Mutex::new(None));
        let event_tx_bg = event_tx.clone();

        // Spawn long-lived stdout reader task
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                tokio::select! {
                    _ = abort_clone.cancelled() => break,
                    result = reader.read_line(&mut line) => {
                        match result {
                            Ok(0) => {
                                // EOF — child process exited
                                break;
                            }
                            Ok(_) => {
                                let trimmed = line.trim().to_string();
                                line.clear();
                                if trimmed.is_empty() {
                                    continue;
                                }
                                if let Ok(event) = serde_json::from_str::<Value>(&trimmed) {
                                    if let Some(agent_event) = translate_bridge_event(&event) {
                                        let guard = event_tx_bg.lock().await;
                                        if let Some(ref tx) = *guard {
                                            let _ = tx.send(agent_event).await;
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                // Read error — child likely exited
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Box::new(ClaudeSession {
            session_id,
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            abort_token,
            event_tx,
            model_id: config.model_id,
            working_dir,
        }))
    }

    async fn health_check(&self) -> PortResult<()> {
        let bridge = Self::bridge_path();
        if !bridge.exists() {
            return Err(PortError::new(
                PortErrorKind::NotFound,
                format!(
                    "Claude bridge script not found at {}",
                    bridge.display()
                ),
            ));
        }
        match Self::resolve_node_binary() {
            Some(path) => {
                log::info!("[ClaudeRuntime] found node at: {}", path.display());
            }
            None => {
                return Err(PortError::new(
                    PortErrorKind::NotAvailable,
                    "Node.js not found: not bundled and not in PATH".to_string(),
                ));
            }
        }
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            return Err(PortError::new(
                PortErrorKind::PermissionDenied,
                "ANTHROPIC_API_KEY environment variable not set.".to_string(),
            ));
        }
        Ok(())
    }

    async fn shutdown(&self) -> PortResult<()> {
        Ok(())
    }
}

// ── Session ──────────────────────────────────────────────────────────────────

pub struct ClaudeSession {
    session_id: String,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    abort_token: CancellationToken,
    /// Current turn's event sender. Set by `prompt()`, consumed by the background reader task.
    event_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<AgentEvent>>>>,
    /// Model override for this session.
    model_id: Option<String>,
    /// Working directory for this session.
    working_dir: String,
}

#[async_trait]
impl AgentSession for ClaudeSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(
        &self,
        input: &str,
        _attachments: Vec<bitfun_runtime_ports::AgentInputAttachment>,
    ) -> PortResult<AgentEventStream> {
        // Create a fresh channel for this turn
        let (tx, rx) = tokio::sync::mpsc::channel(64);

        // Register as the active event target
        {
            let mut guard = self.event_tx.lock().await;
            *guard = Some(tx);
        }

        // Build the command
        let mut cmd = serde_json::json!({
            "command": "prompt",
            "text": input,
            "workingDir": self.working_dir,
        });
        if let Some(ref model) = self.model_id {
            cmd["model"] = Value::String(model.clone());
        }
        let cmd_line = format!("{}\n", cmd);

        // Write to bridge stdin
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(cmd_line.as_bytes())
                .await
                .map_err(|e| PortError::new(PortErrorKind::Backend, format!("stdin write failed: {e}")))?;
        }

        Ok(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
    }

    async fn abort(&self) -> PortResult<()> {
        self.abort_token.cancel();

        let mut child = self.child.lock().await;
        child
            .kill()
            .await
            .map_err(|e| PortError::new(PortErrorKind::Backend, format!("failed to kill child: {e}")))?;

        Ok(())
    }

    async fn dispose(self: Box<Self>) -> PortResult<()> {
        self.abort_token.cancel();

        let mut child = self.child.lock().await;
        child
            .kill()
            .await
            .map_err(|e| PortError::new(PortErrorKind::Backend, format!("failed to kill child: {e}")))?;

        Ok(())
    }
}

// ── Event translation ────────────────────────────────────────────────────────

/// Translate a single bridge JSONL event into an `AgentEvent`.
///
/// Returns `None` for bridge events that have no corresponding `AgentEvent`
/// (e.g. unknown types or internal events).
fn translate_bridge_event(val: &Value) -> Option<AgentEvent> {
    let event_type = val.get("type")?.as_str()?;

    match event_type {
        "text_delta" => {
            let delta = val
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AgentEvent::TextDelta {
                delta,
                metadata: HashMap::new(),
            })
        }
        "thinking_delta" => {
            let delta = val
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AgentEvent::ThinkingDelta {
                delta,
                metadata: HashMap::new(),
            })
        }
        "tool_call_start" => {
            let tool_call_id = val
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tool_name = val
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AgentEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                metadata: HashMap::new(),
            })
        }
        "tool_call_delta" => {
            let tool_call_id = val
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let delta = val
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AgentEvent::ToolCallDelta {
                tool_call_id,
                delta,
                metadata: HashMap::new(),
            })
        }
        "tool_result" => {
            let tool_call_id = val
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let result = val
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AgentEvent::ToolResult {
                tool_call_id,
                result,
                metadata: HashMap::new(),
            })
        }
        "turn_end" => {
            let stop_reason = match val.get("stopReason").and_then(|v| v.as_str()) {
                Some("completed") => StopReason::Completed,
                Some("aborted") => StopReason::Aborted,
                Some("error") => StopReason::Error,
                _ => StopReason::Completed,
            };
            Some(AgentEvent::TurnEnd {
                stop_reason,
                metadata: HashMap::new(),
            })
        }
        "error" => {
            let message = val
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown bridge error")
                .to_string();
            Some(AgentEvent::Error {
                message,
                metadata: HashMap::new(),
            })
        }
        _ => {
            // Unknown event type — skip
            None
        }
    }
}
