---
comet_change: multi-runtime-support
role: technical-design
canonical_spec: openspec
archived-with: 2026-05-29-multi-runtime-support
status: final
---

# Multi-Runtime Support — Technical Design

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│ BitFun Desktop Shell (Rust + Tauri)                  │
│                                                      │
│  ┌─────────┐  ┌──────────┐  ┌────────────────────┐  │
│  │ Web UI   │  │ IM 频道  │  │ 文件 Watcher       │  │
│  │ React    │  │ TG/飞书  │  │ 被动同步子进程修改  │  │
│  └────┬─────┘  └──────────┘  └────────────────────┘  │
│       │                                              │
│  ┌────▼──────────────────────────────────────────┐   │
│  │         DialogScheduler / EventRouter          │   │
│  └────┬──────────────────────────────────────────┘   │
│       │                                              │
│  ┌────▼──────────────────────────────────────────┐   │
│  │         RuntimeRegistry (singleton)             │   │
│  │  ┌──────────┐ ┌─────────┐ ┌──────────┐        │   │
│  │  │ BitFun   │ │ OMP     │ │ Claude   │        │   │
│  │  │ (in-proc)│ │ (sub)   │ │ (sub)    │        │   │
│  │  └──────────┘ └────┬────┘ └────┬─────┘        │   │
│  └────────────────────┼───────────┼───────────────┘   │
└───────────────────────┼───────────┼───────────────────┘
                        │           │
               stdio JSONL    stdio JSONL
                        │           │
                   ┌────▼─────┐ ┌───▼──────┐
                   │ omp rpc  │ │ bridge   │
                   │ 完整工具链│ │ .mjs     │
                   │ 自治运行  │ │ 完整工具链│
                   └──────────┘ │ 自治运行  │
                                └──────────┘
```

## Core Trait Definitions

### AgentRuntime

```rust
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn id(&self) -> &str;                                    // "bitfun" | "omp" | "claude"
    fn display_name(&self) -> &str;                          // "BitFun Native" | "OMP" | "Claude"
    fn capabilities(&self) -> RuntimeCapabilities;
    async fn create_session(&self, config: SessionConfig) -> PortResult<Box<dyn AgentSession>>;
    async fn health_check(&self) -> PortResult<()>;
    async fn shutdown(&self) -> PortResult<()>;
}
```

### AgentSession

```rust
#[async_trait]
pub trait AgentSession: Send + Sync {
    fn session_id(&self) -> &str;
    async fn prompt(
        &self,
        input: &str,
        attachments: Vec<AgentInputAttachment>,
    ) -> PortResult<Pin<Box<dyn Stream<Item = AgentEvent> + Send>>>;
    async fn steer(&self, message: &str) -> PortResult<()>;
    async fn abort(&self) -> PortResult<()>;
    async fn dispose(self: Box<Self>) -> PortResult<()>;
}
```

### AgentEvent

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    TextDelta { delta: String, #[serde(default)] metadata: HashMap<String, Value> },
    ThinkingDelta { delta: String, #[serde(default)] metadata: HashMap<String, Value> },
    ToolCallStart { tool_call_id: String, tool_name: String, #[serde(default)] metadata: HashMap<String, Value> },
    ToolCallDelta { tool_call_id: String, delta: String, #[serde(default)] metadata: HashMap<String, Value> },
    ToolResult { tool_call_id: String, result: String, #[serde(default)] metadata: HashMap<String, Value> },
    TurnStart { #[serde(default)] metadata: HashMap<String, Value> },
    TurnEnd { stop_reason: StopReason, #[serde(default)] metadata: HashMap<String, Value> },
    Error { message: String, #[serde(default)] metadata: HashMap<String, Value> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StopReason { Completed, Aborted, Error, ToolLimit }
```

## Adapter Details

### OmpRuntime

Subprocess: `omp --mode rpc --no-session`

Protocol mapping:

| OMP RPC event | AgentEvent |
|---|---|
| `{"type":"agent_start"}` | `TurnStart` |
| `{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"..."}}` | `TextDelta { delta }` |
| `{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"..."}}` | `ThinkingDelta { delta }` |
| `{"type":"message_update","assistantMessageEvent":{"type":"tool_call_start",...}}` | `ToolCallStart { tool_call_id, tool_name }` |
| `{"type":"message_update","assistantMessageEvent":{"type":"tool_result",...}}` | `ToolResult { tool_call_id, result }` |
| `{"type":"tool_execution_start","toolName":"..."}` | `ToolCallStart` (supplementary) |
| `{"type":"tool_execution_end","toolName":"..."}` | `ToolResult` (supplementary) |
| `{"type":"agent_end","stopReason":"end_turn"}` | `TurnEnd { Completed }` |
| `{"type":"agent_end","stopReason":"aborted"}` | `TurnEnd { Aborted }` |

OMP executes tools autonomously. BitFun observes tool events for UI rendering only — never intercepts execution.

### ClaudeRuntime

Subprocess: `node bridge.mjs`

bridge.mjs wraps `@anthropic-ai/claude-agent-sdk` `query()` into JSONL stdio:

```
stdin:  {"type":"prompt","message":"..."}    →  query({ prompt: "..." })
stdout: {"type":"assistant","subtype":"text_delta","delta":"..."}  →  AgentEvent::TextDelta
stdout: {"type":"result","result":"..."}     →  TurnEnd { Completed }
```

Claude SDK executes tools (Read/Write/Edit/Bash/Glob/Grep/WebSearch) autonomously inside the bridge process.

### BitfunRuntime

Wraps existing `ExecutionEngine` directly — zero IPC, in-process calls. Maps internal `AgenticEvent` to `AgentEvent`.

## Integration Points

### ConversationCoordinator change

Current path:
```
Coordinator::submit() → ExecutionEngine::execute_round()
```

New path:
```
Coordinator::submit() → RuntimeRegistry.get(runtime_id) → AgentSession::prompt()
```

The coordinator only changes at session creation — once a session exists, turns go through `AgentSession::prompt()` regardless of runtime.

### DialogScheduler

No change to scheduling logic. Only `QueuedTurn` gains `runtime_id: Option<String>` field, passed through to coordinator.

## File Synchronization

Autonomous runtimes modify files directly. BitFun's existing `file_watch` module (`services-integrations/src/file_watch`) detects changes and updates the file tree UI. No interception, no redirection.

Conflict principle: **no two agents should modify the same file concurrently**, regardless of whether they share a runtime. This is a task orchestration concern (Phase 2), not an architecture concern.

## Session Persistence

Each runtime stores transcripts in its native format:
- BitFun: internal session format (existing `SessionManager`)
- OMP: OMP JSONL session files (OMP manages its own, or `--no-session` for ephemeral)
- Claude: JSONL event log from bridge output

Switching runtime = creating a new session. Cross-runtime session resumption is out of scope for Phase 1.
