---
comet_change: runtime-turn-persistence
role: technical-design
canonical_spec: openspec
---

# Runtime Turn Persistence — 技术设计

> 需求事实源:`openspec/changes/runtime-turn-persistence/`(proposal + delta spec)。本文只做技术设计,不重定义需求。

## 问题

`run_runtime_event_loop`(`coordinator.rs` ~174-395)在三条终止路径(Completed / Cancelled / Failed)都不调 `session_manager`。runtime(claude/OMP)助手回复仅以 `TextChunk` 事件流式发给 UI,从不写回 session store。reload 从 `model_rounds[].text_items[].content` 重建,而 runtime turn 的 `model_rounds` 恒空 → 会话重载后助手内容全部消失。取消放大了此 gap(部分文本同样丢)。

## 方案:B-extended(text + thinking;扩展 complete/cancel/fail)

### 数据流

```
run_runtime_event_loop (claude/OMP spawn task)
  局部状态: acc_text: String, acc_thinking: String
  ├─ TextDelta     → emit TextChunk(不变) + acc_text.push_str(&delta)
  ├─ ThinkingDelta → emit ThinkingChunk(不变) + acc_thinking.push_str(&delta)
  │
  ├─ TurnEnd Completed  → complete_dialog_turn(sid, tid, acc_text, Some(acc_thinking), stats)
  ├─ Cancelled(cancel 臂 / D8 / prompt-err cancel)
  │                     → cancel_dialog_turn(sid, tid, Some(acc_text), Some(acc_thinking))
  └─ Error / StopReason::_ → fail_dialog_turn(sid, tid, err, Some(acc_text), Some(acc_thinking))

reload 读: model_rounds[].text_items[].content + model_rounds[].thinking_items[].content
           (已验证, session_manager.rs ~568/580)
```

### 集成点

`run_runtime_event_loop` 签名新增 `session_manager: Arc<SessionManager>`。spawn body(~2990)已为 `TurnLifecycleGuard` clone 一份 `session_manager`,再 clone 给 helper 是廉价的(Arc)。现有 bitfun 调用点不受影响。

### 共享注入 helper(D-4=A,thinking-aware)

提取私有 free fn,complete/cancel/fail 三处复用:

```
fn inject_partial_content_if_absent(turn: &mut DialogTurnData, text: &str, thinking: &str, ts: u64)
  if text.trim().is_empty() && thinking.trim().is_empty() { return }   // 都空不注入
  let has_assistant_text = turn.model_rounds.iter().any(|r|
      r.text_items.iter().any(|i| !i.content.trim().is_empty()));
  if has_assistant_text { return }                          // 幂等守卫:已有内容不重复
  let mut round = ModelRoundData { ..., status: "completed" };
  if !text.trim().is_empty()     { round.text_items.push(TextItemData{ content:text, ... }); }
  if !thinking.trim().is_empty() { round.thinking_items.push(ThinkingItemData{ content:thinking, is_collapsed:true, ... }); }
  turn.model_rounds.push(round);
```

`complete_dialog_turn` 现有 ~40 行 fallback(3083-3124)改为调此 helper(text 部分行为零改变,thinking 为新增能力)。`cancel_dialog_turn` / `fail_dialog_turn` 在 set status 前调此 helper(仅当 partial 非空)。

注:thinking 与 text 分别进 `thinking_items` / `text_items` 两个独立数组(非交错),与现有 fallback 结构 + UI"思考在答复之上"渲染一致。`ThinkingItemData.is_collapsed` 是必填 `bool`,历史思考默认折叠(`true`)。

### API 变更

| 函数 | 变更 |
|---|---|
| `complete_dialog_turn` | 加 `thinking: Option<String>` 参数;内部 fallback 改调 thinking-aware helper |
| `cancel_dialog_turn` | 加 `partial_text: Option<String>` + `partial_thinking: Option<String>` |
| `fail_dialog_turn` | 加 `partial_text: Option<String>` + `partial_thinking: Option<String>` |
| `persist_cancelled_dialog_turn` | 透传 `None, None`(bitfun 无 partial) |
| `persist_failed_dialog_turn` | 透传 `None, None` |
| `run_runtime_event_loop` | 加 `session_manager: Arc<SessionManager>` |

bitfun 调用点(coordinator.rs 3329/3340 + complete 路径 1755)传 `None` → helper 守卫使其为 no-op / text-only,行为不变。

## 关键决议(brainstorming D-1~D-4 + scope 修订)

- **D-1(已修订为 B-extended):持久化 text + thinking。** 原选 A(只 text)的理由是"与 bitfun 一致(bitfun 也不恢复 thinking)"。实测推翻该前提:release 版**助理模式(=runtime 路径)持久化 thinking 且重启仍在**,而**专业模式(=bitfun)从不产生 thinking**。故为 runtime 持久化 thinking 不构成不一致,反而补全 runtime 路径匹配 release 已有行为。bitfun 无 thinking 流,传 None 即可。
- **D-2 ordering:** text 与 thinking 进两个独立数组(text_items / thinking_items),各自按到达顺序 `push_str` 累积成单条,无交错排序问题。
- **D-3 `cancel_dialog_turn` 注释修正。** Spike 证实全仓零"增量写 model_rounds"生产路径,bitfun cancel/fail 也不传内容 → 原注释"已流式内容保留在 model_rounds"无支撑。改为陈述真相:"持久化 turn 当前 model_rounds;runtime 部分内容由调用方经 partial_text/partial_thinking 注入"。
- **D-4 抽共享 helper**(thinking-aware,见上)。complete 的现有 fallback 也复用。

## 边界条件

- **全空**(D8 pre-prompt / prompt-err cancel,acc_text 与 acc_thinking 都空)→ helper 双空检查跳过,只改 turn status。
- **仅 thinking 无 text**(模型只输出思考就被取消)→ 注入 round 含 thinking_items、空 text_items。
- **合成 round status** = `"completed"`(内容生成完毕);turn 级 status 才是 Cancelled/Error,分层。UI badge 取 turn status。
- **历史 thinking 折叠** = `is_collapsed: true`(重载的思考默认折叠,与实时展开区分)。
- **transient session**(`should_persist_session_id`=false)→ 三个 persist 函数已 early-return,runtime 路径继承。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| 重构 `complete_dialog_turn` 引入回归 | helper 抽取保证 text 行为零改变;V-5 跑现有 complete 测试 + 新增 characterization 做回归网 |
| `complete_dialog_turn` 加 thinking 参数破坏 bitfun complete 调用点 | bitfun 调用点(1755)传 `None` → helper text-only 分支,行为不变;V-4 覆盖 |
| bitfun cancel/fail 行为变化 | `partial_text=None, partial_thinking=None` + `has_assistant_text` 守卫 → no-op;V-4 显式回归测试 |
| 取消窗口内 acc 与事件流不一致 | acc_text/acc_thinking 在 loop 内同步累积,与 emit 同点,无并发 |
| release 的 thinking 持久化机制可能与本实现不同 | 无法访问 release 代码;本实现走 model_rounds[].thinking_items(reload 已读此路径)。若未来 release 代码合入冲突,以届时实际代码为准调和 |

## 测试策略

| 测试 | 验证 |
|---|---|
| V-1 | runtime Completed → reload → 助手 text + thinking 都存在 |
| V-2 | runtime Cancelled(有部分 text+thinking)→ reload → 都存在 |
| V-3 | runtime Failed → reload → 已生成 text+thinking 存在 |
| V-4 | bitfun complete/cancel/fail 传 None → 行为不变(回归网) |
| V-5 | `complete_dialog_turn` 重构后现有测试全绿 + text characterization |
| V-6 | 全空取消 → 无空 round 注入(边界) |
| V-7 | 仅 thinking 无 text 的 Completed → round 含 thinking_items、空 text_items |

## Future Work(超出本 change)

- 工具调用输出、子代理输出的全保真持久化(本 change 覆盖 text + thinking,未含 tool/subagent)。可控性 agent 工具的关键。已记入项目记忆。
- bitfun 自身 cancel/fail 的部分内容持久化(本 change 传 None 保持现状,不处理)。
