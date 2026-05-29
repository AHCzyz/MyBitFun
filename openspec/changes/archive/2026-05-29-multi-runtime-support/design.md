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
- **OMP**：首选默认（成熟工具链，日常编码）
- **Claude**：高质量备选（需要 Claude 特有能力时）
- **BitFun 原生**：内置 fallback（没装 omp、没配 Claude key 时的保底）

### D7: 会话持久化策略

**选择**：按运行时格式分别存储。切换运行时 = 创建新会话。跨运行时会话恢复不在 Phase 1 范围。

**理由**：
- 各运行时的 transcript 格式本质不同（OMP JSONL vs Claude JSONL vs BitFun 内部格式）
- 统一 transcript 格式需要理解每个运行时的上下文管理语义，成本高且容易出错
- "换运行时=新会话"是最诚实的语义——不同运行时的上下文窗口、工具能力、消息格式都不一样

### D8: 文件同步——被动 watcher

**选择**：依赖现有 `file_watch` 模块被动同步子进程的文件修改。

**分析**：
- 文件同时修改冲突不是架构问题——同运行时的两个 agent 同时改同一文件也会冲突。这是任务编排层的职责（Phase 2）
- BitFun 的 file_watch 延迟 ~50-100ms，编码场景可接受
- 原则：不应该同时修改同一个文件，不管是否同运行时

## Risks / Trade-offs

| Risk | 影响 | 缓解 |
|---|---|---|
| 子进程启动延迟 | 首次使用 ~500ms | lazy spawn + UI loading 状态 |
| 子进程崩溃 | 当前回合中断 | 自动重启 + 注入 Error 事件；会话历史在 BitFun 侧有 transcript 可回溯 |
| AgentEvent 翻译信息丢失 | 运行时特有功能无法在 UI 展示 | metadata 扩展字段兜底；逐步完善事件映射 |
| OMP/Claude 二进制未安装 | 运行时不可用 | health_check 检测，UI 灰化 |
| Bridge 进程安全 | 子进程有完整用户权限 | 个人桌面场景，不过度工程。多租户场景需额外沙箱 |
| 会话格式分裂 | 切换运行时丢失上下文 | 明确接受；Phase 2 任务编排可跨运行时传递输出（文本，不是 transcript） |
| 工具执行结果 UI 延迟 | ~50-100ms | 编码场景可接受；非实时编辑 |

## Open Questions

- Claude Agent SDK bridge 是打包进安装包还是首次使用时下载 Node.js 依赖？
- OpenCode / Codex CLI adapter 放在 Phase 1 还是后续 phase？
- OMP 作为默认运行时时，BitFun 原生 fallback 的触发条件是"omp not found"还是用户可以显式选择？
