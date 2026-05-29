## 修复方案

### A. RuntimeCancelGuard 单测 + T1/T4 augment

**位置：** `coordinator.rs` mod tests（~5950 行附近）

**新增 2 个 sync `#[test]`**（不带 tokio runtime）：

1. `runtime_cancel_guard_removes_when_armed` — pre-insert entry 到 DashMap，构造 armed guard，drop，断言 entry removed
2. `runtime_cancel_guard_keeps_when_disarmed` — pre-insert entry，构造 armed guard，调用 `disarm()`，drop，断言 entry kept

**Augment T1 和 T4：**
- 在调用 helper 前预置 `cancels.insert("tid", cancel.clone())`
- helper 返回后 `assert!(cancels.get("tid").is_none())`
- 这验证了 helper 内 guard 的移除行为，而非空 map 上的 no-op

### B. prompt() Err 臂 is_cancelled 复检

**位置：** `coordinator.rs` `run_runtime_event_loop` prompt() Err 分支

在 Err 臂开头加：
```rust
if cancel_token.is_cancelled() {
    log::info!("Runtime {} turn cancelled during prompt(): ...");
    let _ = event_queue.enqueue(AgenticEvent::DialogTurnCancelled { ... }, Some(EventPriority::High)).await;
    let _ = rt_session.dispose().await;
    return;
}
```

D8 `is_cancelled()` 通过后、prompt() 返回 Err 之前，cancel 可能已触发。复检让事件归类为 Cancelled 而非 Failed。

### 不改动的部分

- 不改动 spec 级别行为（Cancelled vs Failed 的观感差异不构成 spec 变更）
- 不涉及架构调整
- 不新增 public API
