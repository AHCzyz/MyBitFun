//! OMP (Oh My Pi) runtime adapter.
//!
//! Communicates with the `omp` binary via JSONL over stdin/stdout in RPC mode.
//! Each `OmpSession` spawns a dedicated `omp --mode rpc --no-session` subprocess.
//!
//! Binary resolution order:
//! 1. Bundled: `<exe_dir>/../resources/omp/omp<.exe>` (Tauri resource layout)
//! 2. PATH: system-installed `omp`

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bitfun_runtime_ports::agent_runtime::{
    AgentEvent, AgentEventStream, AgentRuntime, AgentSession, RuntimeCapabilities,
    SessionConfig, StopReason,
};
use bitfun_runtime_ports::{AgentInputAttachment, PortError, PortErrorKind, PortResult};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Binary resolution
// ---------------------------------------------------------------------------

/// Resolve the omp binary path.
/// Order: bundled resource → PATH lookup.
fn resolve_omp_binary() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;

    // Tauri resource layout: <install_dir>/bin/bitfun-desktop.exe
    // Resources are at:        <install_dir>/resources/omp/omp.exe
    // In dev:                  target/debug/bitfun-desktop.exe
    // Resources at:            <project_root>/resources/omp/omp.exe
    let exe_dir = exe.parent()?;

    // Try 1: <exe_dir>/../resources/omp/omp<.exe>
    if let Some(parent) = exe_dir.parent() {
        let bundled = parent.join("resources").join("omp").join("omp");
        if bundled.exists() {
            return Some(bundled);
        }
        #[cfg(windows)]
        {
            let bundled_exe = parent.join("resources").join("omp").join("omp.exe");
            if bundled_exe.exists() {
                return Some(bundled_exe);
            }
        }
    }

    // Try 2: <exe_dir>/../../resources/omp/omp<.exe> (deeper nesting, some installers)
    if let Some(grandparent) = exe_dir.parent().and_then(|p| p.parent()) {
        let bundled = grandparent.join("resources").join("omp").join("omp");
        if bundled.exists() {
            return Some(bundled);
        }
        #[cfg(windows)]
        {
            let bundled_exe = grandparent.join("resources").join("omp").join("omp.exe");
            if bundled_exe.exists() {
                return Some(bundled_exe);
            }
        }
    }

    // Try 3: PATH
    which::which("omp").ok()
}

// ---------------------------------------------------------------------------
// OmpRuntime
// ---------------------------------------------------------------------------

/// OMP runtime provider. Resolves bundled or system omp binary.
pub struct OmpRuntime {
    capabilities: RuntimeCapabilities,
}

impl OmpRuntime {
    pub fn new() -> Self {
        Self {
            capabilities: RuntimeCapabilities {
                description: "OMP agent runtime via RPC subprocess".to_string(),
                supports_steer: true,
                supports_thinking: true,
                autonomous_tools: true,
                extras: HashMap::new(),
            },
        }
    }
}

impl Default for OmpRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRuntime for OmpRuntime {
    fn id(&self) -> &str {
        "omp"
    }

    fn display_name(&self) -> &str {
        "OMP (Oh My Pi)"
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    async fn create_session(&self, _config: SessionConfig) -> PortResult<Box<dyn AgentSession>> {
        let omp_path = resolve_omp_binary().ok_or_else(|| {
            PortError::new(
                PortErrorKind::NotFound,
                "omp binary not found: not bundled and not in PATH".to_string(),
            )
        })?;

        let mut child = Command::new(&omp_path)
            .args(["--mode", "rpc", "--no-session"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                PortError::new(
                    PortErrorKind::Backend,
                    format!("failed to spawn omp subprocess ({}): {}", omp_path.display(), e),
                )
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PortError::new(PortErrorKind::Backend, "omp stdin not available"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PortError::new(PortErrorKind::Backend, "omp stdout not available"))?;

        let session_id = Uuid::new_v4().to_string();

        Ok(Box::new(OmpSession {
            session_id,
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            abort_token: CancellationToken::new(),
        }))
    }

    async fn health_check(&self) -> PortResult<()> {
        match resolve_omp_binary() {
            Some(path) => {
                log::info!("[OmpRuntime] found omp at: {}", path.display());
                Ok(())
            }
            None => Err(PortError::new(
                PortErrorKind::NotFound,
                "omp binary not found: not bundled and not in PATH",
            )),
        }
    }

    async fn shutdown(&self) -> PortResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// OmpSession
// ---------------------------------------------------------------------------

pub struct OmpSession {
    session_id: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    abort_token: CancellationToken,
}

impl OmpSession {
    /// Write a JSON command as a single JSONL line to stdin.
    async fn write_command(stdin: &Mutex<ChildStdin>, cmd: &Value) -> Result<(), PortError> {
        let mut line = serde_json::to_string(cmd).map_err(|e| {
            PortError::new(PortErrorKind::Backend, format!("serialize error: {}", e))
        })?;
        line.push('\n');

        let mut writer = stdin.lock().await;
        writer.write_all(line.as_bytes()).await.map_err(|e| {
            PortError::new(PortErrorKind::Backend, format!("stdin write error: {}", e))
        })
    }

    /// Spawn a background task reading OMP JSONL stdout, translating to AgentEvents.
    fn spawn_reader(
        stdout: Arc<Mutex<BufReader<ChildStdout>>>,
        tx: mpsc::UnboundedSender<AgentEvent>,
        abort_token: CancellationToken,
    ) {
        tokio::spawn(async move {
            let mut reader = stdout.lock().await;
            let mut line = String::new();

            loop {
                tokio::select! {
                    _ = abort_token.cancelled() => {
                        let _ = tx.send(AgentEvent::TurnEnd {
                            stop_reason: StopReason::Aborted,
                            metadata: HashMap::new(),
                        });
                        break;
                    }
                    result = reader.read_line(&mut line) => {
                        match result {
                            Ok(0) => break,
                            Ok(_) => {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    match serde_json::from_str::<Value>(trimmed) {
                                        Ok(json) => {
                                            let event = Self::translate_message(&json);
                                            if tx.send(event).is_err() {
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            let _ = tx.send(AgentEvent::Error {
                                                message: format!(
                                                    "JSON parse error from omp: {} (line: {})",
                                                    e, trimmed,
                                                ),
                                                metadata: HashMap::new(),
                                            });
                                        }
                                    }
                                }
                                line.clear();
                            }
                            Err(e) => {
                                if !abort_token.is_cancelled() {
                                    let _ = tx.send(AgentEvent::Error {
                                        message: format!("stdout read error: {}", e),
                                        metadata: HashMap::new(),
                                    });
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Translate a single OMP JSONL message to an AgentEvent.
    fn translate_message(msg: &Value) -> AgentEvent {
        let msg_type = msg["type"].as_str().unwrap_or("");

        match msg_type {
            "message_update" => {
                let data = &msg["data"];
                let data_type = data["type"].as_str().unwrap_or("");
                match data_type {
                    "text_delta" => AgentEvent::TextDelta {
                        delta: data["delta"].as_str().unwrap_or("").to_string(),
                        metadata: HashMap::new(),
                    },
                    "thinking_delta" => AgentEvent::ThinkingDelta {
                        delta: data["delta"].as_str().unwrap_or("").to_string(),
                        metadata: HashMap::new(),
                    },
                    "tool_call_start" => AgentEvent::ToolCallStart {
                        tool_call_id: data["tool_call_id"].as_str().unwrap_or("").to_string(),
                        tool_name: data["tool_name"].as_str().unwrap_or("").to_string(),
                        metadata: HashMap::new(),
                    },
                    "tool_call_delta" => AgentEvent::ToolCallDelta {
                        tool_call_id: data["tool_call_id"].as_str().unwrap_or("").to_string(),
                        delta: data["delta"].as_str().unwrap_or("").to_string(),
                        metadata: HashMap::new(),
                    },
                    "tool_result" => AgentEvent::ToolResult {
                        tool_call_id: data["tool_call_id"].as_str().unwrap_or("").to_string(),
                        result: data["result"].as_str().unwrap_or("").to_string(),
                        metadata: HashMap::new(),
                    },
                    _ => AgentEvent::Error {
                        message: format!("unknown message_update data type from omp: {}", data_type),
                        metadata: HashMap::new(),
                    },
                }
            }
            "agent_start" => AgentEvent::TurnStart {
                metadata: HashMap::new(),
            },
            "agent_end" => AgentEvent::TurnEnd {
                stop_reason: StopReason::Completed,
                metadata: HashMap::new(),
            },
            _ => AgentEvent::Error {
                message: format!("unknown message type from omp: {}", msg_type),
                metadata: HashMap::new(),
            },
        }
    }
}

#[async_trait]
impl AgentSession for OmpSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(
        &self,
        input: &str,
        _attachments: Vec<AgentInputAttachment>,
    ) -> PortResult<AgentEventStream> {
        let cmd = serde_json::json!({
            "command": "prompt",
            "text": input,
        });
        Self::write_command(&self.stdin, &cmd).await?;

        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();

        Self::spawn_reader(
            Arc::clone(&self.stdout),
            tx,
            self.abort_token.clone(),
        );

        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn steer(&self, message: &str) -> PortResult<()> {
        let cmd = serde_json::json!({
            "command": "steer",
            "text": message,
        });
        Self::write_command(&self.stdin, &cmd).await
    }

    async fn abort(&self) -> PortResult<()> {
        self.abort_token.cancel();
        let cmd = serde_json::json!({"command": "abort"});
        Self::write_command(&self.stdin, &cmd).await
    }

    async fn dispose(self: Box<Self>) -> PortResult<()> {
        self.abort_token.cancel();
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
        Ok(())
    }
}
