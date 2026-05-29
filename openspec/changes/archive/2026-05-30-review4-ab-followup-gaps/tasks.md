## Tasks

- [x] T-1: 缺口 1 —— `run_runtime_event_loop` prompt() Err 臂 cancel 复检命中时，`log::info!` 追加 `suppressed_err={:?}` (`e`) 和 `suppressed_kind={:?}` (`e.kind`)，保留被抑制的 PortError 上下文
- [x] T-2: 缺口 4 —— select! loop 内 `RuntimeEvent::Error` 臂开头加 `is_cancelled()` 复检（命中 → log + emit `DialogTurnCancelled`(High) + dispose + return），镜像 B
- [x] T-3: 缺口 4 —— `TurnEnd { StopReason::_ }` 兜底臂在 emit Failed 前加 `is_cancelled()` 复检（命中 → dispose + return，非 break）
- [x] T-4: 缺口 2 —— `FakeSession` 扩展可注入 prompt 行为：新增 `prompt_err: Mutex<Option<PortError>>` + `cancel_on_prompt: Option<CancellationToken>`（prompt() 内先 cancel 再返回，复现 D8 后窗口；适配 Err 与 Ok 两路径）；新增 `fake_session_with_prompt_err` helper，未改现有 4 个 `fake_session` 调用点
- [x] T-5: 缺口 2 —— 新增 T5 `runtime_event_loop_classifies_prompt_err_as_cancelled_when_cancel_signaled`：D8 通过后 prompt() 内 cancel 再返回 Err → 断言仅 `DialogTurnCancelled`、无 `DialogTurnFailed`、`disposed`、`cancels.get("tid").is_none()`、`prompt_called`
- [x] T-6: 缺口 2 —— 新增 T6 `runtime_event_loop_prompt_err_emits_failed_when_not_cancelled`：不取消、prompt() 返回 Err → 断言 `DialogTurnFailed`、无 `DialogTurnCancelled`、`disposed`、`cancels` 移除
- [x] T-7: 缺口 3 —— T2(`runtime_event_loop_completes_cleanly`)、T3(`runtime_event_loop_disposes_on_error_event`) 预置 `cancels.insert("tid", cancel.clone())` + 末尾 `assert!(cancels.get("tid").is_none())`
- [x] T-8: 缺口 4 —— 新增 T7 `runtime_event_loop_classifies_stream_error_as_cancelled_when_cancelled`：cancel 在 prompt() 内触发 + stream 预置 Error → 断言终态 `DialogTurnCancelled` 而非 `DialogTurnFailed`（不变量守卫；biased select 下实走 loop cancel 臂，Error 臂复检为 defense-in-depth，见 design）
- [x] T-9: 编译 core crate（`cargo check -p bitfun-core --tests` 通过）并运行 coordinator mod tests（16 passed / 0 failed，含新 T5/T6/T7；runtime_event_loop 子集 7 passed）
