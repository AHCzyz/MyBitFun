## 高层架构决策

### 方案选型:B(扩展 cancel/fail 接受 partial_text)

探索阶段比较了 3 个方案:

| | Completed | Cancelled/Failed | API 改动 | F-3 闭合度 |
|---|---|---|---|---|
| A 最小复用 | `complete_dialog_turn` | 调现有 cancel/fail,部分文本仍丢 | 仅 helper + 加参数 | 部分 |
| **B 扩展 cancel/fail(选定)** | `complete_dialog_turn` | cancel/fail 加 `partial_text: Option<String>`,`has_assistant_text` 守卫注入 | 改 2 API 签名 + 调用点 | **完全** |
| C 新建 runtime 专用 API | `persist_runtime_turn` | 同函数内分派 | 加 1 新 API | 完全但冗余 |

**选 B 的理由:** review 明确点名"取消功能放大了这个 gap",取消的部分文本丢失是用户痛点核心。A 留尾巴(取消 gap 未闭合);C 与 `complete_dialog_turn` 的 fallback 逻辑冗余。B 用 `has_assistant_text` 守卫(空才注入)对 bitfun(model_rounds 有内容)是 no-op、对 runtime(空)才注入,两路径兼容不重复写。

### 数据流

```
run_runtime_event_loop (claude/OMP spawn task)
  │  累积: acc_text:String, acc_thinking:String (按事件到达顺序)
  ├─ TextDelta    → emit TextChunk(不变) + acc_text.push_str(delta)
  ├─ ThinkingDelta→ emit ThinkingChunk(不变) + acc_thinking.push_str(delta)
  │
  ├─ TurnEnd Completed → complete_dialog_turn(sid,tid,acc_text,stats)
  │                       └ has_assistant_text fallback 注入 acc_text ✓
  ├─ cancel / D8 / prompt-err cancel → cancel_dialog_turn(sid,tid, Some(acc_text+thinking))
  ├─ Error / StopReason::_ → fail_dialog_turn(sid,tid,err, Some(acc_text+thinking))
  │
  └ reload 读: model_rounds[].text_items[].content + thinking_items[].content
```

### 集成点

- `run_runtime_event_loop` 签名加 `session_manager: Arc<SessionManager>`(spawn body 已 clone 给 `TurnLifecycleGuard`,再 clone 廉价)。
- `cancel_dialog_turn` / `fail_dialog_turn` 加 `partial_text: Option<String>` 参数;现有 bitfun 调用点传 `None`(no-op,不回归)。

### 待 brainstorming 深化的开放项

1. **thinking 内容的持久化形态** — `complete_dialog_turn` 的 fallback 只注入 `text_items`,不含 `thinking_items`。runtime 的 thinking 要不要持久化?若要,Completed 路径的 fallback 也需扩展(或 runtime 走不同注入)。这是方案 B 边界,需 design 定。
2. **ordering 保留** — text 与 thinking 交错到达时,reload 渲染顺序。当前 reload 把 text_items 和 thinking_items 分别 join,可能丢失交错顺序。
3. **矛盾注释 spike** — `cancel_dialog_turn` 注释"已流式内容保留在 model_rounds"无证据支撑(全仓无增量写 model_rounds 的生产路径)。design 阶段定向确认 bitfun 持久化机制(~15min spike),据此敲定注释措辞。判定不阻塞 B(`has_assistant_text` 守卫对两种真相都安全)。
4. **partial_text 注入逻辑落点** — 注入逻辑放 `cancel_dialog_turn`/`fail_dialog_turn` 内,还是抽共享 helper(与 complete 的 fallback 共用)。

### 不改动

- 不改 bitfun 执行引擎路径
- 不改 `ModelRoundData`/`TextItemData` 结构(复用)
- 不改事件流(TextChunk/ThinkingChunk emit 保持,只是额外累积)

### 升级/规模评估

跨 2 文件(coordinator.rs + session_manager.rs)、新 capability、需 delta spec、5+ 测试。完整 workflow(full),非 preset。
