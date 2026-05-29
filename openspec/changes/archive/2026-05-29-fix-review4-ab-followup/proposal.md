## Why

review4 对 commit `17a3667f`（runtime turn 取消）审核发现：新增的 `RuntimeCancelGuard` armed/disarmed 行为零测试覆盖（4 个现有测试均在空 DashMap 上运行，guard 的核心移除逻辑从未被断言）；同时 prompt() Err 分支不复检 `is_cancelled()`，导致极端窄窗口内取消被错报为 Failed。两者均为 review4 "强烈推荐"项，应作为 P-2 的紧跟收尾。

## What Changes

- 新增 2 个 sync `#[test]`：`runtime_cancel_guard_removes_when_armed` 和 `runtime_cancel_guard_keeps_when_disarmed`，直接验证 guard 的 RAII 移除 / disarm 保留行为
- T1、T4 预置 `cancels.insert("tid", cancel.clone())`，helper 返回后断言 `cancels.get("tid").is_none()`
- prompt() Err 臂开头加 `if cancel_token.is_cancelled()` 复检，将取消归类为 Cancelled 而非 Failed

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

（无 — 纯测试补充 + 3-5 行防御性复检，不改变任何 spec 级别行为）

## Impact

- `coordinator.rs` mod tests 区域（新增测试 + 2 个已有测试 augment）
- `coordinator.rs` `run_runtime_event_loop` prompt() Err 分支（加 3-5 行）
