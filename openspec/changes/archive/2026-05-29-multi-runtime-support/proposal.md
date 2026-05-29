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
