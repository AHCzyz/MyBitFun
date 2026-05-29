## 1. Trait 体系定义

- [x] 1.1 在 `runtime-ports` crate 中新增 `src/agent_runtime.rs`，定义 `AgentRuntime` trait（`id`, `display_name`, `capabilities`, `create_session`, `health_check`, `shutdown`）
- [x] 1.2 定义 `AgentSession` trait（`session_id`, `prompt` → event stream, `steer`, `abort`, `dispose`）
- [x] 1.3 定义 `AgentEvent` 枚举（`TextDelta`, `ThinkingDelta`, `ToolCallStart`, `ToolCallDelta`, `ToolResult`, `TurnStart`, `TurnEnd`, `Error`）和 `StopReason` 枚举，每个 variant 含 `metadata: HashMap<String, Value>` 扩展字段
- [x] 1.4 定义 `RuntimeCapabilities` struct（运行时能力描述：支持 steer、支持 thinking、工具自治等）
- [x] 1.5 定义 `SessionConfig` DTO（runtime_id, model_id, working_dir 等）
- [x] 1.6 在 `runtime-ports/src/lib.rs` 中 pub mod agent_runtime 并 re-export 所有关键类型

## 2. RuntimeRegistry

- [x] 2.1 实现 `RuntimeRegistry` struct（内部 `HashMap<String, Arc<dyn AgentRuntime>>`），提供 `register()`, `get()`, `list_all()` 方法
- [x] 2.2 实现全局单例 `get_global_runtime_registry()`（OnceLock 模式，与 AgentRegistry 一致）
- [x] 2.3 实现 `health_check_all()` 批量健康检查，返回每个运行时的可用状态
- [x] 2.4 实现默认运行时选择逻辑：OMP 优先 → Claude → BitFun fallback

## 3. BitfunRuntime Adapter（fallback）

- [x] 3.1 实现 `BitfunRuntime` struct，持有 `Arc<AgenticSystem>` 引用
- [x] 3.2 实现 `BitfunRuntime::create_session()`，通过 `SessionManager` 创建会话，返回 `BitfunSession`
- [x] 3.3 实现 `BitfunSession::prompt()`，将 `ExecutionEngine` 的流式输出翻译为 `AgentEvent` 流
- [x] 3.4 实现 `BitfunSession::steer()` / `abort()` / `dispose()`
- [x] 3.5 编写 BitfunRuntime 单元测试（验证 trait 实现编译通过、session 创建成功、health_check 返回 Ok）

## 4. OmpRuntime Adapter（自治子进程）

- [x] 4.1 实现 `OmpProcess` struct，管理 `omp --mode rpc --no-session` 子进程生命周期（spawn, stdin/stdout JSONL 读写）
- [x] 4.2 实现 JSONL 事件读取循环，将 OMP 的 `message_update` / `agent_start` / `agent_end` / `tool_execution_*` 翻译为 `AgentEvent`
- [x] 4.3 实现 `OmpRuntime` struct 和 `AgentRuntime` trait
- [x] 4.4 实现 `OmpSession` struct 和 `AgentSession` trait
- [x] 4.5 实现 `health_check()`：检测 `omp` 是否在 PATH 中，执行 `omp --version` 验证
- [x] 4.6 编写 OmpRuntime 集成测试（需要安装 omp 二进制）

## 5. ClaudeRuntime Adapter（自治子进程）

- [x] 5.1 创建 `bridge.mjs` Node.js bridge 脚本（~80 行）：import SDK, 接收 JSONL 命令, 写出 JSONL 事件
- [x] 5.2 实现 `ClaudeProcess` struct，管理 `node bridge.mjs` 子进程生命周期
- [x] 5.3 实现 Claude SDK 事件流到 `AgentEvent` 的翻译
- [x] 5.4 实现 `ClaudeRuntime` struct 和 `AgentRuntime` trait
- [x] 5.5 实现 `ClaudeSession` struct 和 `AgentSession` trait
- [x] 5.6 实现 `health_check()`：检测 `node` 在 PATH 中，检测 `ANTHROPIC_API_KEY` 环境变量
- [x] 5.7 编写 ClaudeRuntime 集成测试（需要 API key）

## 6. Coordinator 集成

- [x] 6.1 修改 `ConversationCoordinator` 的会话创建路径，接受 `runtime_id` 参数，通过 `RuntimeRegistry` 获取对应的 `AgentRuntime`
- [x] 6.2 修改 `DialogScheduler` 的 `QueuedTurn` 结构，增加 `runtime_id: Option<String>` 字段
- [x] 6.3 修改 `api-layer` 的 session 创建 DTO，增加 `runtimeId` 字段
- [x] 6.4 修改 Tauri transport adapter 的 session 创建 command，传递 runtime_id
- [x] 6.5 确保默认行为（runtime_id 为 None）使用 RuntimeRegistry 的默认选择逻辑

## 7. 前端运行时选择器

- [x] 7.1 创建 `RuntimeSelector` React 组件：显示可用运行时列表，健康状态指示器
- [x] 7.2 实现运行时健康状态 API（Tauri command → RuntimeRegistry.health_check_all()）
- [x] 7.3 在会话创建流程中集成 runtime 选择
- [x] 7.4 在现有会话的 header 区域显示当前 runtime，支持切换（确认后创建新会话）
- [x] 7.5 不可用的 runtime 灰化并显示原因（如 "omp not found in PATH"）

## 8. 启动注册与打包

- [x] 8.1 在 `AgenticSystem::init` 中注册所有 adapter 到 RuntimeRegistry
- [x] 8.2 在桌面应用启动时触发运行时健康检查，缓存结果
- [x] 8.3 将 `bridge.mjs` 打包到桌面应用资源中
