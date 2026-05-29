# Comet Design Handoff

- Change: multi-runtime-support
- Phase: design
- Mode: compact
- Context hash: 7b23cb66201e06e0d4be4e0ea6b44fc1fdc4216026fa8d35cfcb7fa7e6cea59b

Generated-by: comet-handoff.sh

OpenSpec remains the canonical capability spec. This handoff is a deterministic, source-traceable context pack, not an agent-authored summary.

## openspec/changes/multi-runtime-support/proposal.md

- Source: openspec/changes/multi-runtime-support/proposal.md
- Lines: 1-34
- SHA256: 42ad62075be40141b7dafdc12b6e235b325a72908104379a59d19182093bbe28

```md
## Why

BitFun（fork 为 MyBitFun）内置了一套完整的 agent 执行引擎（Rust 原生），但无法使用其他经过验证的 agent 运行时（如 OMP/pi、Claude Agent SDK、OpenCode、Codex CLI）。用户需要在不同场景下切换到最合适的运行时——例如用 OMP 处理需要自定义工具链的编码任务，用 Claude Agent SDK 处理需要 Claude 特有能力的工作流——而当前架构只支持 BitFun 自有运行时。

接入外部运行时的核心目的是使用其**完整成熟体系**（LLM 推理 + 工具选择 + 工具执行），而不是仅借用 LLM 调用能力。

## What Changes

- 在 `runtime-ports` crate 中新增 `AgentRuntime` 和 `AgentSession` async trait，定义统一的运行时会话接口
- 新增 `AgentEvent` 统一事件枚举，将各运行时的原生事件模型翻译为统一格式
- 采用**自治子进程模型**（Model C）：每个外部运行时作为自包含黑盒运行，自带完整工具链，BitFun 不参与工具执行
- 实现 `BitfunRuntime` adapter，包装现有 `ExecutionEngine` 为 `AgentRuntime` 的一个实现（fallback）
- 实现 `OmpRuntime` adapter，通过 `omp --mode rpc` 子进程（JSONL stdio）桥接，OMP 自治运行
- 实现 `ClaudeRuntime` adapter，通过 Node.js bridge 子进程桥接 Claude Agent SDK，Claude 自治运行
- 在 `RuntimeRegistry` 中注册和发现可用运行时，支持运行时健康检查
- 在前端 Web UI 中新增运行时选择器组件，允许用户在会话级别切换运行时
- 将现有会话创建路径改为通过 `AgentRuntime` trait 驱动

## Capabilities

### New Capabilities
- `agent-runtime-switching`: 定义 AgentRuntime trait 体系、RuntimeRegistry、AgentEvent 统一事件模型，以及 BitfunRuntime/OmpRuntime/ClaudeRuntime 三个适配器。采用自治子进程模型——外部运行时自带完整工具链，BitFun 仅负责 UI 渲染和事件转发
- `runtime-ui-selector`: 前端运行时选择器 UI 组件，显示可用运行时列表、健康状态、能力描述，允许会话级切换

### Modified Capabilities

## Impact

- **核心 crate**：`runtime-ports`（新增 trait 和 DTO）、`core`（会话创建路径改为 trait 驱动）
- **新增 adapter 代码**：`runtime-ports` 或新 crate 中实现 OmpRuntime/ClaudeRuntime
- **前端**：`web-ui` 中新增运行时选择器组件
- **新依赖**：子进程管理（已有 tokio 支持），Node.js bridge 脚本（~80 行，随项目分发）
- **会话持久化**：按运行时格式分别存储，切换运行时=创建新会话
- **文件同步**：依赖现有 `file_watch` 模块被动同步子进程的文件修改到 UI
```

## openspec/changes/multi-runtime-support/design.md

- Source: openspec/changes/multi-runtime-support/design.md
- Lines: 1-119
- SHA256: 260b98e30126d40af2fc12be82c6b1ff0267c13258e3683ed0e549d7cc8b18f3

[TRUNCATED]

```md
## Context

MyBitFun（fork 自 GCWing/BitFun）是一个 Rust + Tauri 桌面 Agent 运行时。当前架构中，所有 agent 交互都通过内置的 `ExecutionEngine` → `RoundExecutor` → `StreamProcessor` 链路完成。

项目已有：
- `runtime-ports` crate：纯 DTO 和 trait，不依赖具体实现
- `acp` crate：ACP（Agent Client Protocol）的外部协议适配层——跨进程协议，不适合做本地运行时抽象
- `ConversationCoordinator` / `DialogScheduler`：会话调度和消息队列
- `AgentRegistry`：内置/自定义 agent 管理
- `file_watch` 模块：文件系统变更监听

## Goals / Non-Goals

**Goals:**
- 定义 Rust-native `AgentRuntime` trait 体系，作为运行时切换的一等抽象
- 采用自治子进程模型（Model C）：外部运行时自带完整工具链，BitFun 不参与工具执行
- 实现 BitFun native adapter（包装现有 ExecutionEngine，作为 fallback）
- 实现 OMP RPC adapter（`omp --mode rpc` 子进程，自治运行）
- 实现 Claude Agent SDK adapter（Node.js bridge 子进程，自治运行）
- 前端运行时选择器，会话级切换
- 运行时健康检查和可用性发现

**Non-Goals:**
- 统一工具执行层（各运行时用自己的工具链，BitFun 不翻译工具）
- host_tools / customTools 桥接（可选增强，不在 Phase 1）
- 定时任务、顺序任务编排（Phase 2）
- 多 Agent 协作看板（Phase 3）
- SDK 直接嵌入（避免 Node.js FFI / V8 依赖爆炸）
- 跨运行时会话恢复（OMP 会话只能用 OMP 恢复，Claude 同理）

## Decisions

### D1: 自治子进程模型（Model C）

**选择**：每个外部运行时作为自包含黑盒运行，自带完整工具链（LLM + 工具选择 + 工具执行）。BitFun 只负责：prompt 传入、event stream 接收和 UI 渲染、文件 watcher 被动同步。

**理由**：
- 接入外部运行时的目的是使用其成熟体系，不是只借 LLM 调用
- 零工具翻译成本——OMP 用 OMP 的 Edit，Claude 用 Claude 的 Read/Write
- Adapter 实现简单——只需 JSONL 事件翻译，不需要理解工具内部实现

**替代方案（已否决）**：
- Model A（宿主工具层）：强迫 BitFun 重新实现 OMP/Claude 的全部工具，永远滞后，且违背"接入成熟体系"的初衷
- host_tools 桥接：覆盖度有限，部分工具无法通过 host_tools 暴露

### D2: AgentRuntime trait 在 runtime-ports crate

**选择**：在 `runtime-ports` 中新增 `agent_runtime` 模块。

**理由**：`runtime-ports` 已是纯 trait/DTO crate。新 trait 不引入 IO、网络、进程依赖，只定义接口。符合项目 core-decomposition-plan 中"新 crate 不依赖回 bitfun-core"的原则。

### D3: 子进程桥接而非 SDK 嵌入

**选择**：OMP 用 `omp --mode rpc` 子进程，Claude 用 Node.js bridge 子进程。Rust 侧通过 tokio Process + stdin/stdout JSONL 通信。

**理由**：
- 零额外编译依赖
- 进程隔离（OOM/panic 不影响主进程）
- OMP RPC 就是官方嵌入方式
- Claude SDK 底层 bundled Claude Code binary，bridge ~80 行

**替代方案（已否决）**：N-API FFI（复杂度爆炸）、嵌入 V8（包体积暴增）

### D4: 统一 AgentEvent 事件模型

**选择**：定义 `AgentEvent` 枚举，每个 adapter 翻译自己的原生事件。

**理由**：前端只需消费一套事件模型。各运行时的事件模型高度重叠（文本流、工具调用、回合边界），翻译损失最小。

**信息丢失处理**：保留 `metadata: HashMap<String, Value>` 扩展字段，运行时特有信息放这里。

### D5: RuntimeRegistry 全局单例

**选择**：`OnceLock + Arc`，启动时注册所有 adapter。

**理由**：与现有 `AgentRegistry`、`get_global_coordinator` 模式一致。

### D6: 运行时定位

**选择**：
```

Full source: openspec/changes/multi-runtime-support/design.md

## openspec/changes/multi-runtime-support/tasks.md

- Source: openspec/changes/multi-runtime-support/tasks.md
- Lines: 1-64
- SHA256: a1afe116b124a14b9bd22b72988fe2b99377798476205992156da7e70835172a

```md
## 1. Trait 体系定义

- [ ] 1.1 在 `runtime-ports` crate 中新增 `src/agent_runtime.rs`，定义 `AgentRuntime` trait（`id`, `display_name`, `capabilities`, `create_session`, `health_check`, `shutdown`）
- [ ] 1.2 定义 `AgentSession` trait（`session_id`, `prompt` → event stream, `steer`, `abort`, `dispose`）
- [ ] 1.3 定义 `AgentEvent` 枚举（`TextDelta`, `ThinkingDelta`, `ToolCallStart`, `ToolCallDelta`, `ToolResult`, `TurnStart`, `TurnEnd`, `Error`）和 `StopReason` 枚举，每个 variant 含 `metadata: HashMap<String, Value>` 扩展字段
- [ ] 1.4 定义 `RuntimeCapabilities` struct（运行时能力描述：支持 steer、支持 thinking、工具自治等）
- [ ] 1.5 定义 `SessionConfig` DTO（runtime_id, model_id, working_dir 等）
- [ ] 1.6 在 `runtime-ports/src/lib.rs` 中 pub mod agent_runtime 并 re-export 所有关键类型

## 2. RuntimeRegistry

- [ ] 2.1 实现 `RuntimeRegistry` struct（内部 `HashMap<String, Arc<dyn AgentRuntime>>`），提供 `register()`, `get()`, `list_all()` 方法
- [ ] 2.2 实现全局单例 `get_global_runtime_registry()`（OnceLock 模式，与 AgentRegistry 一致）
- [ ] 2.3 实现 `health_check_all()` 批量健康检查，返回每个运行时的可用状态
- [ ] 2.4 实现默认运行时选择逻辑：OMP 优先 → Claude → BitFun fallback

## 3. BitfunRuntime Adapter（fallback）

- [ ] 3.1 实现 `BitfunRuntime` struct，持有 `Arc<AgenticSystem>` 引用
- [ ] 3.2 实现 `BitfunRuntime::create_session()`，通过 `SessionManager` 创建会话，返回 `BitfunSession`
- [ ] 3.3 实现 `BitfunSession::prompt()`，将 `ExecutionEngine` 的流式输出翻译为 `AgentEvent` 流
- [ ] 3.4 实现 `BitfunSession::steer()` / `abort()` / `dispose()`
- [ ] 3.5 编写 BitfunRuntime 单元测试（验证 trait 实现编译通过、session 创建成功、health_check 返回 Ok）

## 4. OmpRuntime Adapter（自治子进程）

- [ ] 4.1 实现 `OmpProcess` struct，管理 `omp --mode rpc --no-session` 子进程生命周期（spawn, stdin/stdout JSONL 读写）
- [ ] 4.2 实现 JSONL 事件读取循环，将 OMP 的 `message_update` / `agent_start` / `agent_end` / `tool_execution_*` 翻译为 `AgentEvent`
- [ ] 4.3 实现 `OmpRuntime` struct 和 `AgentRuntime` trait
- [ ] 4.4 实现 `OmpSession` struct 和 `AgentSession` trait
- [ ] 4.5 实现 `health_check()`：检测 `omp` 是否在 PATH 中，执行 `omp --version` 验证
- [ ] 4.6 编写 OmpRuntime 集成测试（需要安装 omp 二进制）

## 5. ClaudeRuntime Adapter（自治子进程）

- [ ] 5.1 创建 `bridge.mjs` Node.js bridge 脚本（~80 行）：import SDK, 接收 JSONL 命令, 写出 JSONL 事件
- [ ] 5.2 实现 `ClaudeProcess` struct，管理 `node bridge.mjs` 子进程生命周期
- [ ] 5.3 实现 Claude SDK 事件流到 `AgentEvent` 的翻译
- [ ] 5.4 实现 `ClaudeRuntime` struct 和 `AgentRuntime` trait
- [ ] 5.5 实现 `ClaudeSession` struct 和 `AgentSession` trait
- [ ] 5.6 实现 `health_check()`：检测 `node` 在 PATH 中，检测 `ANTHROPIC_API_KEY` 环境变量
- [ ] 5.7 编写 ClaudeRuntime 集成测试（需要 API key）

## 6. Coordinator 集成

- [ ] 6.1 修改 `ConversationCoordinator` 的会话创建路径，接受 `runtime_id` 参数，通过 `RuntimeRegistry` 获取对应的 `AgentRuntime`
- [ ] 6.2 修改 `DialogScheduler` 的 `QueuedTurn` 结构，增加 `runtime_id: Option<String>` 字段
- [ ] 6.3 修改 `api-layer` 的 session 创建 DTO，增加 `runtimeId` 字段
- [ ] 6.4 修改 Tauri transport adapter 的 session 创建 command，传递 runtime_id
- [ ] 6.5 确保默认行为（runtime_id 为 None）使用 RuntimeRegistry 的默认选择逻辑

## 7. 前端运行时选择器

- [ ] 7.1 创建 `RuntimeSelector` React 组件：显示可用运行时列表，健康状态指示器
- [ ] 7.2 实现运行时健康状态 API（Tauri command → RuntimeRegistry.health_check_all()）
- [ ] 7.3 在会话创建流程中集成 runtime 选择
- [ ] 7.4 在现有会话的 header 区域显示当前 runtime，支持切换（确认后创建新会话）
- [ ] 7.5 不可用的 runtime 灰化并显示原因（如 "omp not found in PATH"）

## 8. 启动注册与打包

- [ ] 8.1 在 `AgenticSystem::init` 中注册所有 adapter 到 RuntimeRegistry
- [ ] 8.2 在桌面应用启动时触发运行时健康检查，缓存结果
- [ ] 8.3 将 `bridge.mjs` 打包到桌面应用资源中
```

## openspec/changes/multi-runtime-support/specs/agent-runtime-switching/spec.md

- Source: openspec/changes/multi-runtime-support/specs/agent-runtime-switching/spec.md
- Lines: 1-127
- SHA256: 870c0d853ee3b4eed5e11b27478030bb1baca234617aebd0002d1f53534bc56a

[TRUNCATED]

```md
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

```

Full source: openspec/changes/multi-runtime-support/specs/agent-runtime-switching/spec.md

## openspec/changes/multi-runtime-support/specs/runtime-ui-selector/spec.md

- Source: openspec/changes/multi-runtime-support/specs/runtime-ui-selector/spec.md
- Lines: 1-38
- SHA256: a1be84fc4da180f9a9fe2c60fea3264451021f62ac3d05c1cc11c3867908da2e

```md
## ADDED Requirements

### Requirement: Runtime selector component
The system SHALL provide a UI component that displays available runtimes and allows the user to select one for the current or new session.

#### Scenario: Displays registered runtimes
- **WHEN** the runtime selector is rendered
- **THEN** it lists all runtimes from `RuntimeRegistry` with display name, description, and health status

#### Scenario: Healthy runtime shown as available
- **WHEN** a runtime's `health_check()` returns Ok
- **THEN** the runtime is shown as selectable (not grayed out)

#### Scenario: Unhealthy runtime shown as unavailable
- **WHEN** a runtime's `health_check()` returns an error
- **THEN** the runtime is grayed out with the error reason displayed (e.g. "omp not found in PATH", "ANTHROPIC_API_KEY not set")

### Requirement: Default runtime selection
The system SHALL select the default runtime automatically based on availability: OMP if installed, otherwise BitFun native.

#### Scenario: OMP installed becomes default
- **WHEN** OMP health check passes
- **THEN** new sessions default to OMP runtime

#### Scenario: No external runtime falls back to BitFun
- **WHEN** neither OMP nor Claude health check passes
- **THEN** new sessions default to BitFun native runtime

### Requirement: Runtime selection persists per session
The system SHALL store the selected runtime ID as part of session configuration.

#### Scenario: Resume session uses stored runtime
- **WHEN** a session created with `runtime_id: "omp"` is resumed
- **THEN** the OMP adapter is used for continued interaction

#### Scenario: Switching runtime creates new session with confirmation
- **WHEN** user changes the runtime selector while in an active session
- **THEN** the system prompts for confirmation, then creates a new session with the selected runtime
```

