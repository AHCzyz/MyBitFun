## 1. Hotfix

- [x] 1.1 P1 — In `MyBitFun/src/crates/core/src/agentic/coordination/coordinator.rs`, in the spawn-task `Err(e) =>` branch (around the `prompt()` match in the runtime dispatch block, ~line 2640), add `active_counter.fetch_sub(1, Ordering::SeqCst)` and `session_manager.reset_session_state_if_processing(&session_id_clone, &turn_id_clone)` after `let _ = rt_session.dispose().await;` and before `return;`.
- [x] 1.2 P5 — Replace `*slot_guard = Some(rt_session); drop(slot_guard);` at session put-back with `let prev = slot_guard.replace(rt_session); drop(slot_guard); if let Some(prev_session) = prev { let _ = prev_session.dispose().await; }`. Then in `MyBitFun/src/crates/core/src/agentic/runtime_adapters/claude_runtime.rs::create_session`, add `.kill_on_drop(true)` to the `Command::new(&node_binary)` builder chain (after `.stderr(...)` and before `.spawn()`).
- [x] 1.3 Verify — Run `cd MyBitFun && cargo check -p bitfun-core` ; confirm clean build with no new warnings related to the touched code paths. **Result:** PASS, `Finished dev profile in 1m 05s`. One pre-existing `unused import: AgentRuntime` warning at coordinator.rs:45 — out of scope, tracked as follow-up.

## 2. Scope expansion (added during build phase)

- [x] 2.1 E0063 baseline fix — While running cargo check the first time, surfaced a pre-existing compile error in the same `Err(e)` branch as Task 1.1: `AgenticEvent::DialogTurnFailed { session_id, turn_id, error }` was missing required fields `error_category` and `error_detail`. The other 4 `DialogTurnFailed` initializers in the file all carry `error_category: None, error_detail: None` (or proper Some(_)). Pattern verified to be a single-point omission in the WIP committed at `0b4ff520`. Added the two `None` fields. Necessary for the build to pass; bundled with P1 commit since both edits live in the same error branch.

