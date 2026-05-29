## 1. Hotfix

- [x] 1.1 Lift `SessionExecutionGuard` from inline-in-bitfun-spawn to module-level `TurnLifecycleGuard` in `coordinator.rs` (placed near the top of the file, after `classify_runtime_error`).
- [x] 1.2 Update bitfun spawn closure to reference the lifted `TurnLifecycleGuard` (delete the inline `struct`/`impl Drop` block, keep the `_guard = TurnLifecycleGuard::new(...)` line).
- [x] 1.3 Add `let _guard = TurnLifecycleGuard::new(...)` at the top of the runtime spawn task body, owning `session_manager`, `session_id_clone`, `turn_id_clone`, `active_counter`.
- [x] 1.4 Delete the manual `active_counter.fetch_sub(1, Ordering::SeqCst); session_manager.reset_session_state_if_processing(...)` pair in the prompt() Err branch (≈ lines 2679~2683).
- [x] 1.5 Delete the same pair in the `RuntimeEvent::Error` branch (≈ lines 2798~2802) — keep `dispose()` and `return`.
- [x] 1.6 Delete the same pair in the success put-back tail (≈ lines 2821~2825) — keep the `slot_guard.replace(rt_session)` and `displaced.dispose()` logic.
- [x] 1.7 Verify: `cd MyBitFun && cargo check -p bitfun-core --message-format=short` exits 0.
- [x] 1.8 Confirm root cause is removed: grep the runtime spawn block (lines ~2658~2826) for `fetch_sub` and `reset_session_state_if_processing` — both must return zero hits inside that range.
