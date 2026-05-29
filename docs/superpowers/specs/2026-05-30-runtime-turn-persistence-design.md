---
comet_change: runtime-turn-persistence
role: technical-design
canonical_spec: openspec
---

# Runtime Turn Persistence — 技术设计

> 需求事实源:`openspec/changes/runtime-turn-persistence/`(proposal + delta spec)。本文只做技术设计,不重定义需求。

## 问题

`run_runtime_event_loop`(`coordinator.rs` ~174-395)在三条终止路径(Completed / Cancelled / Failed)都不调 `session_manager`。runtime(claude/OMP)助手回复仅以 `TextChunk` 事件流式发给 UI,从不写回 session store。reload 从 `model_rounds[].text_items[].content` 重建,而 runtime turn 的 `model_rounds` 恒空 → 会话重载后助手内容全部消失。取消放大了此 gap(部分文本同样丢)。

## 方案:B(扩展 cancel/fail 接受 partial_text)

### 数据流

```
run_runtime_event_loop (claude/OMP spawn task)
  局部状态: acc_text: String
  ├─ TextDelta     → emit TextChunk(不变) + acc_text.push_str(&delta)
  ├─ ThinkingDelta → emit ThinkingChunk(不变),不累积(D-1: thinking 不持久化)
  │
  ├─ TurnEnd Completed  → complete_dialog_turn(sid, tid, acc_text, stats)
  ├─ Cancelled(cancel 臂 / D8 / prompt-err cancel)
  │                     → cancel_dialog_turn(sid, tid, Some(acc_text))
  └─ Error / StopReason::_ → fail_dialog_turn(sid, tid, err, Some(acc_text))

reload 读: model_rounds[].text_items[].content (已验证, session_manager.rs ~568)
```

### 集成点

`run_runtime_event_loop` 签名新增 `session_manager: Arc<SessionManager>`。spawn body(~2990)已为 `TurnLifecycleGuard` clone 一份 `session_manager`,再 clone 给 helper 是廉价的(Arc)。现有 bitfun 调用点不受影响。

### 共享注入 helper(D-4=A)

提取私有方法,complete/cancel/fail 三处复用:

```
fn inject_partial_text_if_absent(turn: &mut DialogTurnData, text: &str, ts: u64)
  if text.trim().is_empty() { return }                      // 空文本不注入
  let has_assistant_text = turn.model_rounds.iter().any(|r|
      r.text_items.iter().any(|i| !i.content.trim().is_empty()));
  if has_assistant_text { return }                          // 幂等守卫:已有内容不重复
  turn.model_rounds.push(ModelRoundData { text_items: [text], status: "completed", ... });
```

`complete_dialog_turn` 现有 ~40 行 fallback(3083-3124)改为调此 helper —— 行为零改变(原逻辑等价),同时缩小重复面。`cancel_dialog_turn` / `fail_dialog_turn` 在 set status 前调此 helper(仅当 `partial_text` 为 `Some` 且非空)。

### API 变更

| 函数 | 变更 |
|---|---|
| `complete_dialog_turn` | 无签名变更;内部 fallback 改调 helper(行为不变) |
| `cancel_dialog_turn` | 加 `partial_text: Option<String>` 参数 |
| `fail_dialog_turn` | 加 `partial_text: Option<String>` 参数 |
| `persist_cancelled_dialog_turn` | 加 `partial_text` 透传给 `cancel_dialog_turn` |
| `persist_failed_dialog_turn` | 加 `partial_text` 透传给 `fail_dialog_turn` |
| `run_runtime_event_loop` | 加 `session_manager: Arc<SessionManager>` |

bitfun 调用点(coordinator.rs 3329/3340)传 `partial_text = None` → helper 守卫使其为 no-op,行为不变。

## 关键决议(brainstorming D-1~D-4)

- **D-1 只持久化 text**(不含 thinking)。与 bitfun 现状一致(bitfun reload 也不恢复 thinking)。全保真持久化(thinking/工具/子代理)记为未来 feature。
- **D-2 ordering** 随 D-1 塌缩:单条 text 按到达顺序 `push_str`,天然有序。
- **D-3 `cancel_dialog_turn` 注释修正。** Spike 证实全仓零"增量写 model_rounds"生产路径,bitfun cancel/fail 也不传文本 → 原注释"已流式内容保留在 model_rounds"无支撑。改为陈述真相:"持久化 turn 当前 model_rounds;runtime 部分文本由调用方经 partial_text 注入"。
- **D-4 抽共享 helper**(见上)。complete 的现有 fallback 也复用,缩小既有重复面。

## 边界条件

- **空文本**(D8 pre-prompt / prompt-err cancel,acc_text 空)→ helper 非空检查跳过,只改 turn status。
- **合成 round status** = `"completed"`(文本生成完毕);turn 级 status 才是 Cancelled/Error,分层。UI badge 取 turn status。
- **transient session**(`should_persist_session_id`=false)→ 三个 persist 函数已 early-return,runtime 路径继承。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| 重构 `complete_dialog_turn` 引入回归 | helper 抽取保证行为零改变;V-5 跑现有 complete 测试做回归网 |
| bitfun cancel/fail 行为变化 | `partial_text=None` + `has_assistant_text` 守卫 → no-op;V-4 显式回归测试 |
| 取消窗口内 acc_text 与事件流不一致 | acc_text 在 helper 内同步累积,与 emit 同点,无并发 |

## 测试策略

| 测试 | 验证 |
|---|---|
| V-1 | runtime Completed → reload → 助手 text 存在 |
| V-2 | runtime Cancelled(有部分文本)→ reload → 部分 text 存在 |
| V-3 | runtime Failed → reload → 已生成 text 存在 |
| V-4 | bitfun cancel/fail 传 None → 行为不变(回归网) |
| V-5 | `complete_dialog_turn` 重构后现有测试全绿 |
| V-6 | 空文本取消 → 无空 round 注入(边界) |

## Future Work(超出本 change)

- 全保真持久化:thinking 推理、工具调用输出、子代理输出。可控性 agent 工具的关键。应 bitfun + runtime 一起做以保持一致。已记入项目记忆。
- bitfun 自身 cancel/fail 的部分文本持久化(本 change 传 None 保持现状,不处理)。
