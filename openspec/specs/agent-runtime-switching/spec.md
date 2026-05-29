## ADDED Requirements

### Requirement: AgentRuntime trait definition
The system SHALL define an async `AgentRuntime` trait in `runtime-ports` crate with methods: `id()`, `display_name()`, `capabilities()`, `create_session()`, `health_check()`, `shutdown()`.

#### Scenario: Trait compiles without concrete dependencies
- **WHEN** `runtime-ports` crate is compiled
- **THEN** `AgentRuntime` trait is available without depending on `bitfun-core`, `tokio-process`, or any concrete implementation

### Requirement: AgentSession trait definition
The system SHALL define an async `AgentSession` trait with methods: `session_id()`, `prompt()`, `steer()`, `abort()`, `dispose()`.

#### Scenario: prompt returns event stream
- **WHEN** `prompt()` is called with a text input
- **THEN** it returns a `Pin<Box<dyn Stream<Item = AgentEvent> + Send>>`

#### Scenario: abort cancels running turn
- **WHEN** `abort()` is called during an active prompt
- **THEN** the event stream emits `AgentEvent::TurnEnd { stop_reason: Aborted }` and terminates

### Requirement: AgentEvent unified event model
The system SHALL define an `AgentEvent` enum with variants: `TextDelta`, `ThinkingDelta`, `ToolCallStart`, `ToolCallDelta`, `ToolResult`, `TurnStart`, `TurnEnd`, `Error`. Each variant SHALL include a `metadata: HashMap<String, Value>` field for runtime-specific information.

#### Scenario: TextDelta carries incremental text
- **WHEN** a runtime emits partial text output
- **THEN** `AgentEvent::TextDelta { delta, metadata }` is emitted with the incremental content

#### Scenario: TurnEnd signals completion
- **WHEN** an agent turn completes
- **THEN** `AgentEvent::TurnEnd { stop_reason, metadata }` is emitted with one of: `Completed`, `Aborted`, `Error`, `ToolLimit`

#### Scenario: Runtime-specific information preserved
- **WHEN** OMP emits a tool execution event with fields not mapped to standard AgentEvent variants
- **THEN** the unmapped fields are preserved in the `metadata` HashMap

### Requirement: BitfunRuntime adapter (fallback)
The system SHALL implement `AgentRuntime` for the existing BitFun execution engine, wrapping `ExecutionEngine` + `AgenticSystem` as an in-process adapter. This serves as the always-available fallback when no external runtime is installed.

#### Scenario: BitfunRuntime creates session
- **WHEN** `BitfunRuntime::create_session()` is called
- **THEN** a session is created through `SessionManager`, and the returned `AgentSession` wraps `ExecutionEngine`

#### Scenario: BitfunRuntime health check always passes
- **WHEN** `health_check()` is called
- **THEN** it returns `Ok(())` immediately (no external dependency, always available)

### Requirement: OmpRuntime adapter (autonomous subprocess)
The system SHALL implement `AgentRuntime` for OMP via `omp --mode rpc` subprocess with JSONL stdio communication. OMP runs autonomously with its own complete toolchain — BitFun does NOT participate in tool execution.

#### Scenario: OmpRuntime spawns subprocess
- **WHEN** `create_session()` is called
- **THEN** the adapter spawns `omp --mode rpc --no-session` as a child process and waits for `{"type":"ready"}` on stdout

#### Scenario: OmpRuntime translates prompt to RPC command
- **WHEN** `prompt()` is called with input text
- **THEN** the adapter writes `{"id":"...","type":"prompt","message":"<input>"}` to subprocess stdin and returns an event stream

#### Scenario: OmpRuntime translates RPC events to AgentEvent
- **WHEN** subprocess emits `{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"..."}}`
- **THEN** adapter emits `AgentEvent::TextDelta { delta, metadata }` into the event stream

#### Scenario: OmpRuntime tool execution is autonomous
- **WHEN** OMP subprocess emits `tool_execution_start` / `tool_execution_end` events
- **THEN** the adapter translates them to `AgentEvent::ToolCallStart` / `AgentEvent::ToolResult` without intercepting or re-executing the tool

#### Scenario: OmpRuntime health check detects missing binary
- **WHEN** `health_check()` is called and `omp` binary is not found in PATH
- **THEN** it returns an error describing the missing binary

### Requirement: ClaudeRuntime adapter (autonomous subprocess)
The system SHALL implement `AgentRuntime` for Claude Agent SDK via a Node.js bridge subprocess. Claude runs autonomously with its own complete toolchain — BitFun does NOT participate in tool execution.

#### Scenario: ClaudeRuntime spawns bridge
- **WHEN** `create_session()` is called
- **THEN** the adapter spawns `node bridge.mjs` (bundled with the app) as a child process

#### Scenario: ClaudeRuntime translates SDK events to AgentEvent
- **WHEN** bridge emits `{"type":"assistant","content":[{"type":"text","text":"..."}]}`
- **THEN** adapter emits `AgentEvent::TextDelta { delta, metadata }` into the event stream

#### Scenario: ClaudeRuntime tool execution is autonomous
- **WHEN** Claude SDK executes tools (Read, Write, Edit, Bash, etc.) inside the bridge process
- **THEN** the adapter only observes tool events from the stream and translates to AgentEvent, without intercepting execution

#### Scenario: ClaudeRuntime health check validates API key
- **WHEN** `health_check()` is called and `ANTHROPIC_API_KEY` is not set
- **THEN** it returns an error describing the missing API key

### Requirement: RuntimeRegistry
The system SHALL provide a global `RuntimeRegistry` singleton that registers all available `AgentRuntime` implementations at startup.

#### Scenario: Registry lists available runtimes
- **WHEN** `list_runtimes()` is called
- **THEN** it returns a list including `BitfunRuntime` (always), `OmpRuntime` (if omp binary found), and `ClaudeRuntime` (if API key configured)

#### Scenario: Registry retrieves runtime by id
- **WHEN** `get_runtime("omp")` is called
- **THEN** it returns the `OmpRuntime` instance or an error if not registered

### Requirement: ConversationCoordinator runtime integration
The system SHALL modify `ConversationCoordinator` to use `AgentRuntime` for session creation based on user-selected runtime.

#### Scenario: Session created with default runtime
- **WHEN** a new session is created without explicit runtime selection
- **THEN** the default runtime (OMP if available, otherwise BitfunRuntime) is used

#### Scenario: Session created with specific runtime
- **WHEN** a new session is created with `runtime_id: "omp"`
- **THEN** `OmpRuntime::create_session()` is called and subsequent turns use the OMP adapter

### Requirement: File system passive sync
The system SHALL rely on the existing `file_watch` module to passively sync file changes made by autonomous runtimes to the UI. BitFun does NOT intercept or redirect file operations.

#### Scenario: OMP modifies a file
- **WHEN** OMP subprocess writes to a file via its own Edit tool
- **THEN** BitFun's file watcher detects the change and updates the file tree UI

### Requirement: Session persistence per runtime format
The system SHALL store session transcripts in each runtime's native format. Switching runtimes creates a new session — cross-runtime session resumption is not supported in Phase 1.

#### Scenario: OMP session stored in OMP format
- **WHEN** an OMP session is active
- **THEN** the transcript is stored in the format OMP natively produces

#### Scenario: Switching runtime creates new session
- **WHEN** user changes from OMP to Claude in an active session
- **THEN** the system creates a new Claude session; the OMP session transcript is preserved in OMP format
