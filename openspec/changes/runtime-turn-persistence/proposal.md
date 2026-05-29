## Why

claude/OMP 运行时(非 bitfun)的对话轮在 `run_runtime_event_loop`(`coordinator.rs` 约 174-395 行)的任何终止路径——成功(`TurnEnd Completed`)、取消(`Cancelled`)、失败(`Error`/`StopReason::_`)——都**不调用 `session_manager`**。助手回复仅以 `TextChunk`/`ThinkingChunk` 事件流式发给 UI,从不写回 session store。后果:**会话重载后 claude/OMP 的助手内容全部消失**(reload 从 `model_rounds[].text_items[].content` 重建,而 runtime turn 的 `model_rounds` 恒空)。取消功能放大了这个 gap——取消时已生成的部分文本同样丢失。

实证(review4):grep 全仓,`DialogTurnCompleted`/`Cancelled` 订阅者只有 `cron/subscriber.rs` 和 `bitfun_runtime.rs`;`run_runtime_event_loop` 体内 `session_manager.` 命中数 = 0。

这是 review3/review4 路线图优先级 3、最高用户影响项(用户直接可感的数据丢失)。

## What Changes

- `run_runtime_event_loop` 累积流式的 text / thinking deltas(保留 ordering),在三条终止路径把累积内容写回 session store:
  - **Completed** → 复用现有 `session_manager.complete_dialog_turn(sid, tid, final_response, stats)`。其内建 `has_assistant_text` fallback(`model_rounds` 无文本 + `final_response` 非空 → 注入合成 round)正好命中 runtime turn(`model_rounds` 恒空),零 API 改动。
  - **Cancelled** → 扩展 `cancel_dialog_turn` 接受 `partial_text: Option<String>`,以 `has_assistant_text` 守卫注入(空才注入,对 bitfun 是 no-op)。
  - **Failed** → 扩展 `fail_dialog_turn` 接受 `partial_text: Option<String>`,同守卫。
- `run_runtime_event_loop` 签名新增 `session_manager: Arc<SessionManager>`(spawn body 已为 `TurnLifecycleGuard` clone 一份,再 clone 廉价)。
- 修正 `cancel_dialog_turn` 的误导性注释(详见 design 的矛盾注释处理 + spike)。

## Capabilities

### New Capabilities

- `runtime-turn-persistence`: 定义 runtime(claude/OMP)对话轮在成功/取消/失败终止时,助手 text+thinking 内容必须持久化到 session store 并在重载后可见的行为契约。

### Modified Capabilities

(无现有 spec 覆盖此行为;以新 capability 表达。)

## Impact

- `src/crates/core/src/agentic/coordination/coordinator.rs`:`run_runtime_event_loop`(累积 + 三路径持久化 + 加参数)、调用点(spawn body 加 `session_manager.clone()`)。
- `src/crates/core/src/agentic/session/session_manager.rs`:`cancel_dialog_turn`、`fail_dialog_turn` 加 `partial_text` 参数 + 注入逻辑 + 注释修正。
- 测试:runtime turn 三路径 → reload → 助手回复(含 thinking)存在;bitfun 路径 partial_text=None 不回归。
- 无 schema 变更(复用现有 `ModelRoundData`/`TextItemData` 结构)。
