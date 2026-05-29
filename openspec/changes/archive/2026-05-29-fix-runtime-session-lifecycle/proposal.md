## Why

`review1.md` group A flagged two severe defects in the multi-runtime dispatch path that ships in commit `0b4ff520` (multi-runtime feature WIP, on top of upstream `42432b0e`). Both are runtime-state bugs that can compound over normal use.

- **P1 — spawn-task error branch leaks session state.** In `coordinator.rs::handle_user_input`, the `runtime` branch's `tokio::spawn` task takes a path on `prompt()` error that disposes the runtime session and `return`s, **bypassing** the success-path cleanup at the bottom of the spawn body (`active_counter.fetch_sub` + `session_manager.reset_session_state_if_processing`). The session state machine stays in `Processing` and `wait_session_drained` blocks forever, so subsequent turns on the same session cannot start.

- **P5 — concurrent turns orphan Node bridge processes.** The `runtime_sessions` slot uses a `Mutex<Option<Box<dyn AgentSession>>>` "take on entry, put back at exit" pattern. When two turns race on the same session, both can `take` `None` (because the first already took), each creates its own `AgentSession`, and at exit each tries to `*slot_guard = Some(rt_session)` — the second write **overwrites** the first without disposing it. The displaced `ClaudeSession` is then dropped, but `tokio::process::Child` defaults to `kill_on_drop = false` and `ClaudeSession` has no `Drop` impl, so the underlying Node bridge process is **detached and leaks**. Ditto for any future path that drops a session without going through `dispose()`.

## What Changes

- `coordinator.rs`: in the runtime spawn task's error branch, decrement `active_counter` and call `reset_session_state_if_processing` before `return` — same cleanup the success path performs.
- `coordinator.rs`: at session put-back time, use `slot_guard.replace(rt_session)` and `dispose()` the displaced session if any (closes the concurrent-turn race).
- `claude_runtime.rs`: set `.kill_on_drop(true)` on the Node bridge `Command` builder so any drop path (panic, abort, future code that bypasses `dispose`) still tears down the child process.

No public API change. No spec change (no existing capability covers these internals).

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None — no current `openspec/specs/` capability covers the runtime dispatch internals._

## Impact

- **Code:** `MyBitFun/src/crates/core/src/agentic/coordination/coordinator.rs` (+~12 lines), `MyBitFun/src/crates/core/src/agentic/runtime_adapters/claude_runtime.rs` (+1 line).
- **Behaviour on happy path:** no change.
- **Behaviour on error:** session correctly returns to idle state (was: stuck in Processing).
- **Behaviour under concurrency:** no orphan Node bridge processes (was: each race-orphan persists until OS reaps).
- **Out of scope (separate change later):** the same `kill_on_drop` / displacement risks likely apply to `omp_runtime.rs` and `bitfun_runtime.rs`. Review only flagged ClaudeRuntime; broader sweep tracked as follow-up. Also a pre-existing `unused import: AgentRuntime` warning on coordinator.rs:45 — left intact, tracked as follow-up.

## Scope expansion noted during build phase

The first `cargo check` after applying P1+P5 surfaced **E0063** at coordinator.rs:2639 — a pre-existing compile error in the WIP baseline (`0b4ff520`) where the runtime-error branch's `AgenticEvent::DialogTurnFailed` initializer was missing the required `error_category` and `error_detail` fields. The four other `DialogTurnFailed` sites in the file all carry these. Single-point omission, fixed by adding `error_category: None, error_detail: None,` — bundled with the P1 commit because the edit lives in the same error branch.
