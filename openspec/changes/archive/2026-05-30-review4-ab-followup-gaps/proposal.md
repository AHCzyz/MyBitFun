## Why

本次代码审查对 commit `80391090`（review4 A+B follow-up）做了零容忍复盘，发现该 hotfix 虽完成了 finding A 的字面要求，但留下 4 个可见缺口：B 分支吞掉原始 `PortError`（诊断黑洞）、B 分支零测试覆盖（逻辑改错也能全绿）、T2/T3 未对称预置（guard 移除在 happy/Error 路径仍 no-op）、流内 `RuntimeEvent::Error` 与 `StopReason::_` 兜底缺 cancel 复检（与 B 论证不闭合）。作为 review3 P-2 / review4 A+B 的紧跟收尾，应在进入更大改动前把回归网补齐。

## What Changes

- **(1) 高危——保留错误上下文：** `run_runtime_event_loop` 的 `prompt()` Err 臂 cancel 复检命中时，把 `e`（`{:?}`）与 `e.kind`（`{:?}`）作为 `suppressed_err` / `suppressed_kind` 记入 `log::info!`，杜绝诊断信息丢失。
- **(2) 高危——补 B 测试：** 扩展 `FakeSession` 支持注入 `prompt()` 结果，新增 T5（pre-cancel + prompt Err → 仅 `DialogTurnCancelled`、disposed、cancels 移除）与 T6（不取消 + prompt Err → `DialogTurnFailed`）。
- **(3) 中危——对称预置：** T2（`runtime_event_loop_completes_cleanly`）、T3（`runtime_event_loop_disposes_on_error_event`）预置 `cancels.insert("tid", ...)` + 末尾 `assert!(cancels.get("tid").is_none())`。
- **(4) 中危——流内 cancel 复检：** select! loop 内 `RuntimeEvent::Error` 臂与 `TurnEnd { StopReason::_ }` 兜底臂在 emit Failed 前镜像 B 的 `is_cancelled()` 复检（命中 → emit Cancelled + dispose + return），并加 T7 覆盖 stream Error + 已取消 → Cancelled。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

（无 — 纯测试补充 + 防御性复检 + 日志增强，不改变任何 spec 级别行为。Cancelled vs Failed 的观感差异不构成 spec 变更，与 review4 A+B 一致。）

## Impact

- `src/crates/core/src/agentic/coordination/coordinator.rs`：
  - `run_runtime_event_loop` prompt() Err 臂（log 增强 + 已有复检保持）
  - `run_runtime_event_loop` select! loop 的 `RuntimeEvent::Error` 臂、`StopReason::_` 兜底臂（新增 cancel 复检）
  - mod tests：`FakeSession` 扩展可注入 prompt 结果；T2/T3 augment；新增 T5/T6/T7
- 无跨文件、无架构、无 schema、无新 public API。
