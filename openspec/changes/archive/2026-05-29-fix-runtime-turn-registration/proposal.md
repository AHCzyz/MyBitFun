## Why

review3 P-3 🟠 — the runtime spawn task in `coordinator.rs` (claude / OMP / non-bitfun runtimes) is **not panic-safe**. It manually pairs `active_counter.fetch_add(1)` with `fetch_sub(1)` on three exit paths and calls `reset_session_state_if_processing` at the same three points. Any `await` panic between fetch_add and the matching fetch_sub leaks:

- `active_counter` permanently `+1` → `wait_session_drained` deadline-out forever → every subsequent `cancel_dialog_turn` / `delete_session` for this session burns its full timeout window;
- session state stuck in `Processing` → frontend spinner never stops.

Compare the bitfun path (`coordinator.rs:3036~3072`): it uses an inline `SessionExecutionGuard` whose `Drop` does both `fetch_sub` and `reset_session_state_if_processing`. **Any** exit path — early return, panic, normal completion — pays the same cleanup cost. The runtime path lacks this guard.

Reviewer's recommendation (review3.md §P-3, §6 must-fix): extract one shared RAII guard and use it on both paths.

## What Changes

- `coordinator.rs`:
  - Lift the bitfun path's inline `SessionExecutionGuard` to a module-level `TurnLifecycleGuard` struct (no behavioural change for bitfun).
  - Wrap the runtime spawn task body in `TurnLifecycleGuard` at task entry.
  - Delete the three manual `active_counter.fetch_sub(1) + reset_session_state_if_processing(...)` triples in the runtime spawn body (prompt() Err, RuntimeEvent::Error, success put-back). `dispose()` calls on the error paths stay — they are path-specific.
  - Keep `active_counter.fetch_add(1)` synchronous on the calling thread (it must happen before `tokio::spawn` returns so `wait_session_drained` cannot race the increment).

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None — internal refactor of an existing in-memory invariant._

## Impact

- **Code:** 1 file (`coordinator.rs`), ~40 lines net (lift + delete duplications).
- **Behaviour:**
  - Runtime turn panics no longer leak `active_counter` or leave session stuck in `Processing`.
  - Identical RAII contract on bitfun and runtime paths — easier to reason about, no future divergence.
  - No change to dispose semantics; no change to event ordering on the happy path.
- **Risk:** Low — the guard logic is already running in production on the bitfun path. We are duplicating its surface, not its content.
