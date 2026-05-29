# 极致代码审查报告 — runtime turn 取消（commit 17a3667f）

审查对象：实际落地代码（非设计文档）。逐行核对了 `RuntimeCancelGuard`、`run_runtime_event_loop`、`handle_user_input` runtime 分支 wiring、`cancel_dialog_turn` Step 3.5、4 个测试，并模拟了并发/冷启动/删除交错路径。

## 1. 总体评估

- **代码质量评分：85 / 100**
- **最关键风险：**
  1. 🔴（预存，本次实证确认，非本 commit 引入）**runtime turn 全程不落库。** `run_runtime_event_loop` 在成功/错误/取消任何路径都不调 `session_manager`；`complete_dialog_turn`(1690) 只在 bitfun 路径。助手回复仅以 `TextChunk` 事件流出，会话重载后 claude/OMP turn 的助手内容全部消失。这是设计文档 F-3 标的预存 gap，本 commit 明确 out-of-scope——但用 grep + 读码坐实了它真实存在，且取消功能会放大它（取消的部分文本同样丢失）。**必须单开 change。**
  2. 🟡 **本 commit 新增的核心机制几乎没有测试覆盖。** 4 个测试只在空 `cancels` map 上调用 helper，从未 insert 过 entry，因此 `RuntimeCancelGuard` 的"移除存在的 entry"行为零断言；calling-thread guard + `disarm()` + 早返回清理（F-1 最微妙的部分）和 `cancel_dialog_turn` Step 3.5 完全没测。
- **审查摘要：** 实现干净、与设计高度一致，核心取消逻辑正确——`biased` select 让取消优先、D8 闭合冷启动窗口、RAII 双 guard 所有权转移正确、Step 3.5 clone-then-cancel 无死锁、状态机经 `reset_session_state_if_processing` 的 turn-scoped 校验不会误清新 turn。`delete_session` 在活跃 turn 时现在能经 cancel token 快速 dispose bridge（取代旧的最坏 120s）。扣分集中在：新机制缺直接测试、两处极端竞态、一处理论性 shutdown 泄漏，以及笼罩在功能之上的持久化 gap。

## 2. 发现的问题（按严重程度排序）

### F-3 🔴 严重（预存 / out-of-scope，实证确认）— runtime turn 不持久化

- **类别：** 正确性-持久化缺失
- **位置：** `coordinator.rs:174~376`（helper 全程无 `session_manager`）；对比 `1690 complete_dialog_turn`（bitfun-only）
- **证明：** grep 全仓：消费 `DialogTurnCompleted`/`Cancelled` 的只有 `cron/subscriber.rs`（更新调度任务状态）与 `bitfun_runtime.rs`（事件→事件翻译），无任何订阅者把助手消息写回 session store。helper 体内 `session_manager.` 命中数 = 0。
- **后果：** runtime turn 的用户消息经 `start_dialog_turn` 入库，但助手回复永不入库。重载会话只见提问、不见回答。取消时已流式输出的部分文本同样丢失。
- **与本 commit 关系：** 非本 commit 引入（runtime 路径从来不落库），设计已列 Non-Goal。本 commit 无需修，但必须作为最高优先级独立 change 立项——否则取消功能做得再完美，用户重载后仍是空白。
- **参考：** 设计文档 Open Questions / F-3。

### A 🟡 中危 — 新增取消机制无直接测试覆盖

- **类别：** 测试-盲点
- **位置：** 测试 `coordinator.rs:5926~6068`；未覆盖 `2916 cancel_entry_guard`、`3001 disarm`、`3503~3509 Step 3.5`、`RuntimeCancelGuard::drop` 的实际移除
- **详细描述：** 4 个测试全部以 `Arc::new(DashMap::new())`（空 map）调用 helper，从不 insert turn_id 对应 entry。于是：
  - in-helper `RuntimeCancelGuard` drop 时 `map.remove("tid")` 命中空 map → no-op，测试也从不断言"entry 曾被移除"→ guard 的核心职责（移除存在的 entry）零断言。
  - calling-thread guard 的"早返回时移除孤儿 entry / spawn 成功后 disarm"——即 F-1 这次最微妙的新逻辑——完全在 `handle_user_input` 里，测试够不到。
  - `cancel_dialog_turn` Step 3.5（token 触发）零覆盖。
- **后果：** 本次最复杂的所有权转移逻辑，正确性只靠 cargo check + 人工推导背书。未来重构（如有人误删 `disarm()` 或改插入点）不会被测试拦住。
- **修复建议：** `RuntimeCancelGuard` 可独立单测，~12 行：

```rust
#[test]
fn runtime_cancel_guard_removes_when_armed() {
    let m: Arc<DashMap<String, CancellationToken>> = Arc::new(DashMap::new());
    m.insert("t".into(), CancellationToken::new());
    { let _g = RuntimeCancelGuard::armed(m.clone(), "t".into()); }
    assert!(m.get("t").is_none(), "armed guard must remove entry on drop");
}

#[test]
fn runtime_cancel_guard_keeps_when_disarmed() {
    let m: Arc<DashMap<String, CancellationToken>> = Arc::new(DashMap::new());
    m.insert("t".into(), CancellationToken::new());
    { let mut g = RuntimeCancelGuard::armed(m.clone(), "t".into()); g.disarm(); }
    assert!(m.get("t").is_some(), "disarmed guard must NOT remove (ownership transferred)");
}
```

  另建议 T1/T4 在传入 map 时预置 `cancels.insert("tid", cancel.clone())`，并在 helper 返回后断言 `cancels.get("tid").is_none()`——这样才真正验证 helper guard 的移除行为。

### B 🔵 低危 — D8 之后、prompt() 之前取消，错报 Failed 而非 Cancelled

- **类别：** 逻辑-竞态-事件归类
- **位置：** `coordinator.rs:206~226`（prompt() Err 臂）
- **详细描述：** D8 `is_cancelled()` 检查通过后、prompt() 返回 Err 之前，若取消恰好触发，prompt() Err 臂不复检 `is_cancelled()`，用户会看到 `DialogTurnFailed` 而不是 `DialogTurnCancelled`。
- **后果：** 纯观感，极端窄窗口（取消与 prompt 失败同时发生）。turn 终归结束，无资源泄漏。
- **修复建议（可选）：** prompt() Err 臂开头加 `if cancel_token.is_cancelled() { /* emit Cancelled + dispose + return */ }`。优先级低。

### C 🔵 低危（预存）— 队列满时 DialogTurnCancelled 会被丢弃

- **类别：** 可靠性-事件丢失
- **位置：** `events/queue.rs:88~91`
- **详细描述：** `enqueue` 在 `len() >= max_queue_size`(10000) 时不分优先级一律丢弃并返回 `Ok`。helper cancel 分支的 `EventPriority::High` 不享有豁免，极端负载下 `DialogTurnCancelled` 可能被丢。
- **缓解：** 用户 ESC 路径里 `cancel_dialog_turn` 已先 emit `SessionStateChanged{idle}`，UI 仍会翻 Idle；丢的只是 transcript 的取消标记。预存队列设计问题，非本 commit。
- **修复建议（长期）：** 队列对 Critical/High 做保留水位或独立小队列。

### D 🔵 低危（理论）— spawn 后未被 poll 即 shutdown 会泄漏 entry

- **类别：** 资源-生命周期边角
- **位置：** `coordinator.rs:2973~3001`
- **详细描述：** `cancel_entry_guard.disarm()`(3001) 在 `tokio::spawn` 返回后同步执行，不等任务被 poll。窗口：disarm 后、in-helper guard 构造前，若该 spawned future 从未被 poll 就被 drop（仅发生在 runtime 关停），两个 guard 都不会移除该 entry → 泄漏。
- **后果：** 仅进程退出/runtime 关停时发生，此时整个 DashMap 一并销毁。可忽略。但"任何退出路径都不泄漏 entry"的措辞略微过强。
- **备注：** `active_turns_per_session` 计数器有同型（更严重：卡在 +1）的预存模式。非本 commit 引入。

### E 🔵 低危（预存）— 早返回 ? 清理了 cancel entry，但 Processing 状态成孤儿

- **类别：** 逻辑-异常状态残留
- **位置：** `coordinator.rs:2936 / 2954` 的 `?`
- **详细描述：** `registry.get` 或 `create_session` 失败时，calling-thread guard 正确移除 cancel entry，但 `start_dialog_turn` 已设的 `Processing{turn_id}` 无人复位（此时 `TurnLifecycleGuard` 尚未构造，counter 也未 fetch_add）。session 卡 Processing，且 `DialogTurnStarted` 已发但无对应终态事件。
- **后果：** 用户看到"转圈"无法结束（直到下次操作或重启 `restore_session` 归一）。预存问题（本 commit 前同样存在），本 commit 未加重（cancel entry 已被清理）。
- **修复建议（可选，既然已动这块）：** 在这两个 `?` 处补 `reset_session_state_if_processing` + emit `DialogTurnFailed`，或用一个覆盖 Processing 状态的 guard。

### ⚪ 建议

- `FakeSession::prompt`(5901) `take().expect(...)`：二次调用会 panic。当前每 turn 一次 prompt，不可达，但留个隐式假设。
- helper 9 参数已加 `#[allow(clippy::too_many_arguments)]`，可接受；若再加第 10 个参数应改 context struct。

## 3. 可疑模式与潜在风险

1. **helper 与 TurnLifecycleGuard 的职责拆分：** counter/state 归属在闭包的 `TurnLifecycleGuard`，entry 归属在 helper 内的 `RuntimeCancelGuard`。设计上正确（避免把 SM 状态耦合进 helper），但导致直接调 helper 的测试永远测不到 counter/state 复位与 helper 退出的集成。这是 Finding A 的根因之一，属可接受的可测性取舍，但需在心里标注"helper 测试 ≠ 集成测试"。
2. **runtime 路径仍是 bitfun 路径的平行宇宙：** 取消、清理、计数、状态各自一套，本次又加了 `runtime_turn_cancels` 第三张 per-turn/session DashMap。设计已把"合并 RuntimeSessionEntry"列为 out-of-scope，但每加一张表，`delete_session`/并发清理的同步面就更大。长期应收敛（review3 §架构观察）。
3. **`biased;` 不会饿死 stream：** 每轮先 poll cancel（未取消即 Pending，开销极小）再 poll stream——取消反而更灵敏，stream 不被饿死。此处写法正确，非风险，仅澄清。

## 4. 逻辑流追踪

**用户 ESC（mid-stream）：**
```
cancel_dialog_turn(sid, tid)
 ├ update_state_for_turn_if_processing → Idle  ✓ (turn-scoped)
 ├ emit SessionStateChanged{idle}
 ├ execution_engine/tool_pipeline/subagents cancel → 对 runtime 全 no-op
 ├ Step 3.5: get(tid).clone() → token.cancel()   ← 唯一真正抵达 helper 的信号
 └ wait_session_drained(1500ms) → 现在真能等到 0(旧实现常态超时)
[helper] select! biased → cancel 分支：emit DialogTurnCancelled@High, dispose(杀bridge), return
 └ _cancel_guard drop → remove(tid)；闭包 _guard drop → counter-=1, reset(no-op, 已Idle)
单次 DialogTurnCancelled，bridge 数十 ms 内死亡。✓
```

**冷启动窗口取消（ESC during create_session）：**
```
insert(tid,token)@2915 → [create_session await 100-500ms 中] cancel_dialog_turn 触发 token.cancel()
 → wait_session_drained 此刻 counter 未 fetch_add → 立即返回 0(此窗口"假 drained"，但无害)
[create_session 返回] fetch_add → spawn → helper D8 is_cancelled()=true
 → emit DialogTurnCancelled, dispose(刚建的session), return → 无 Anthropic 调用 ✓
```

**delete_session during 活跃 turn：**
```
cancel_active_turn_for_session → cancel_dialog_turn → token.cancel()
delete_session.remove(sid) → slot.take()=None(turn 持有session中)→ 不 double-dispose ✓
[helper] cancel 分支 dispose 杀 bridge ✓(取代旧 120s 残留)
```

## 5. 安全性专项审查（OWASP）

本 commit 不触及认证/授权/加密/注入面。相关项：

- **A04 Insecure Design：** 取消机制现已对齐 bitfun 语义（event-parity），但 persistence-parity 缺失（F-3）仍是设计层缺口。
- **A09 Logging & Monitoring：** cancel 日志为 `log::info!`，仅含 `session_id`/`turn_id`/`runtime_id`，无敏感数据 ✓；但 F-3 导致取消的部分输出既不落库也可能在队列满时丢事件（C），可观测性有缝。
- **信任边界：** helper 处理的 `RuntimeEvent` 来自 bridge stdout（已在 batch1/batch2 审过），本 commit 未改变该边界。`DialogTurnFailed.error` 仍直传错误文本到前端（review3 P-12 预存），非本 commit。
- 无新增注入/越权/泄漏面。

## 6. 最终建议

### 必须修复

- **无 commit 级阻断项。** 独立立项 F-3（runtime turn 持久化）——这是当前最高用户影响，且取消功能让它更显眼。

### 强烈推荐（本 commit 范围内）

- **A：** 补 `RuntimeCancelGuard` 的 armed/disarmed 两个单测（~12 行，见上）；T1/T4 预置 entry 并断言移除。这是把 F-1 最微妙逻辑纳入回归网的最低成本。
- **B：** prompt() Err 臂加 `is_cancelled()` 复检，使取消在所有窄窗口都归类为 Cancelled。

### 可选 / 长期

- **E：** 早返回 `?` 处补 Processing 复位 + 终态事件（既然已动这块）。
- **C：** EventQueue 对 High/Critical 做保留水位。
- **架构：** 收敛 runtime 三张 DashMap 为 `RuntimeSessionEntry`，并让 runtime turn 接入统一的 turn lifecycle（review3 长期项）。

---

**结论：** 本次实现质量高、与设计一致、核心取消逻辑经多路径推演正确，**可以合入**。唯一真正该在合入前补的是 Finding A 的 guard 单测（廉价且正中新逻辑盲点）；F-3 必须立刻另开 change，否则整个 runtime 体验在"重载丢回复"上是破的。
