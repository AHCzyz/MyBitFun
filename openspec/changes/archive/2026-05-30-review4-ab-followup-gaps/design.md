## 修复方案

全部改动位于 `src/crates/core/src/agentic/coordination/coordinator.rs`。按缺口编号给出方案。

### 缺口 1：B 分支保留 PortError 上下文（高危）

**位置：** `run_runtime_event_loop` prompt() Err 臂的 cancel 复检（约 211-225 行）

当前命中复检时 `log::info!` 只含 `runtime_id / session_id / turn_id`，`e` 随 `return` drop。改为在 log 中追加被抑制的错误：

```rust
if cancel_token.is_cancelled() {
    log::info!(
        "Runtime {} turn cancelled during prompt(): session_id={}, turn_id={}, suppressed_err={:?}, suppressed_kind={:?}",
        runtime_id_for_log, session_id, turn_id, e, e.kind,
    );
    let _ = event_queue.enqueue(AgenticEvent::DialogTurnCancelled { .. }, Some(EventPriority::High)).await;
    let _ = rt_session.dispose().await;
    return;
}
```

事件仍归类为 Cancelled（UX 不变），但工程师可从 log grep 出真实 PortError。`e.kind` 是枚举判别，不含 PII。

### 缺口 2：B 分支测试覆盖（高危）

**位置：** mod tests 的 `FakeSession`（约 5932-5965 行）

`FakeSession::prompt` 当前无条件 `Ok(...)`。加一个可注入的 prompt 结果字段：

```rust
struct FakeSession {
    session_id: String,
    event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<RuntimeEvent>>>,
    disposed: Arc<AtomicBool>,
    prompt_called: Arc<AtomicBool>,
    prompt_err: tokio::sync::Mutex<Option<PortError>>, // 新增：Some → prompt() 返回 Err
}
```

`prompt()` 先 set `prompt_called`，若 `prompt_err` 有值则 take 并返回 `Err`，否则走原 Ok 路径。
`fake_session` 构造函数增加 `prompt_err` 参数（现有 4 个调用点传 `None`），或新增 `fake_session_with_prompt_err` helper 避免改动现有调用点签名。**选后者**，最小化对现有测试的扰动。

注：`PortError` 已核实定义于 `crates/runtime-ports/src/lib.rs`：`PortError { kind: PortErrorKind, message: String }`，构造用 `PortError::new(kind, msg)`，derive `Debug`。`PortErrorKind` 变体为 `NotAvailable / NotFound / InvalidRequest / PermissionDenied / Cancelled / Timeout / Backend`（**无 `Internal`**）。测试用 `PortError::new(PortErrorKind::Backend, "boom")`。

**T5** `runtime_event_loop_classifies_prompt_err_as_cancelled_when_cancel_signaled`：
- `cancel.cancel()` 在 helper 运行前 —— 但 D8 会先短路。为命中 *prompt() Err 复检* 而非 D8，需让 cancel 在 D8 之后、prompt() 返回 Err 时才可见。
- **实现策略：** D8 检查 `is_cancelled()`，若此时未取消则进入 prompt()。要测 prompt() Err 复检，需 prompt() 内部触发 cancel 后再返回 Err。让 `FakeSession::prompt` 在返回 Err 前先 `cancel_token.cancel()`。但 FakeSession 无 cancel handle —— 故给注入式 fake 传入 `CancellationToken`，prompt() 内 `token.cancel()` 再返回 Err，精确复现"D8 通过后、prompt Err 之前 cancel 触发"。
- 断言：仅 `DialogTurnCancelled`、无 `DialogTurnFailed`、`disposed`、`cancels.get("tid").is_none()`、`prompt_called`。

**T6** `runtime_event_loop_prompt_err_emits_failed_when_not_cancelled`：
- 不取消，prompt() 直接返回 Err。
- 断言：`DialogTurnFailed`、无 `DialogTurnCancelled`、`disposed`、`cancels` 移除。

### 缺口 3：T2/T3 对称预置（中危）

**位置：** `runtime_event_loop_completes_cleanly`（约 6009 行）、`runtime_event_loop_disposes_on_error_event`（约 6043 行）

各加：
```rust
cancels.insert("tid".into(), cancel.clone()); // 验证 guard 移除（与 T1/T4 对称）
// ... 调用 helper ...
assert!(cancels.get("tid").is_none(), "guard did not remove entry on <happy|error> path");
```
注意 T2/T3 现在以值传 `cancel` 给 helper，需先 `cancel.clone()` 留一份给 insert。

### 缺口 4：流内 Error / StopReason 兜底 cancel 复检（中危）

**位置：** select! loop 内 `RuntimeEvent::Error` 臂（约 357 行）、`TurnEnd { StopReason::_ }` 兜底臂（约 334-349 行）

两处在构造/emit `DialogTurnFailed` 之前镜像 B 的复检：

```rust
// RuntimeEvent::Error { message, .. } 臂开头
if cancel_token.is_cancelled() {
    log::info!("Runtime {} turn cancelled during stream error: session_id={}, turn_id={}, suppressed_msg={:?}",
        runtime_id_for_log, session_id, turn_id, message);
    let _ = event_queue.enqueue(AgenticEvent::DialogTurnCancelled { .. }, Some(EventPriority::High)).await;
    let _ = rt_session.dispose().await;
    return;
}
```

`StopReason::_` 兜底臂同理。**已核实当前实现**：所有 `TurnEnd` 变体（含 `_` Failed）在 match outcome 后统一 `break`（约 355 行）→ 走 put-back 回收 session。复检命中（cancel）时应 `dispose + return`，与 cancel 臂 / prompt-err 臂 / Error 臂的 bad-state 处理一致（取消的 turn 不回收 session）。这是局部新增的 cancel 分支，不改动非 cancel 时的 `break` 行为。

**T7** `runtime_event_loop_classifies_stream_error_as_cancelled_when_cancelled`：
- prompt() 返回 Ok stream；先 `cancel.cancel()`，再向 stream 发 `RuntimeEvent::Error`。
- ⚠️ **已确认的时序约束**：`tokio::select!` 带 `biased`，cancel 臂优先。cancel 一旦 set，select 下一轮必走 cancel 臂——Error 臂的 `is_cancelled()` 复检在 *单测可控范围内不可稳定命中*（真实生产中"select 已选 stream.next()、arm 运行前另一线程 cancel"的并发窄窗口才会命中，单测无注入 hook 无法精确复现）。
- **决策（诚实标注）：** 复检代码作为 **defense-in-depth** 保留——它关闭的是"select 偏置失效 / 未来重构去掉 biased"后的回归面，几行且显然正确。T7 不假装测到复检本身，而是验证**不变量**："cancel 已 set + stream 后续 Error 事件 → 终态必为 `DialogTurnCancelled`，绝不为 `DialogTurnFailed`"。该断言对"biased 命中 cancel 臂"与"复检命中"两条路径都成立；若有人同时移除 biased *和* 复检，T7 失败。这是有效的回归守卫，而非对不可达代码的伪测试。

### 不改动的部分

- 不改 spec 级别行为
- 不涉及架构 / 新 public API
- D8 pre-prompt 臂与 loop cancel 臂保持现状（已正确）

### 升级评估

单文件、`run_runtime_event_loop` 单函数 + 同文件 test mod。无 3+ 文件、无架构、无新接口。**确认 hotfix 适用，不升级。**
