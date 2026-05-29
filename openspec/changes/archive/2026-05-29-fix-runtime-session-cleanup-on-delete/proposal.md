## Why

`coordinator.runtime_sessions` and `coordinator.active_turns_per_session` are `DashMap`s keyed by `session_id`. Both are populated lazily via `entry().or_insert_with(...)` on the runtime-dispatch path. Neither is ever removed — `grep 'runtime_sessions.*remove\|active_turns_per_session.*remove'` returns 0 matches across the whole crate.

Effects on a long-running desktop process:

- Every BitFun session that ever ran on a non-bitfun runtime (Claude / OMP) leaves a `Mutex<Option<Box<dyn AgentSession>>>` entry behind in `runtime_sessions`. After the previous A-group hotfix added `kill_on_drop(true)`, the *child process* is killed when the entry is eventually dropped — but the entry is only dropped when the `coordinator` itself drops, i.e. on application exit. Until then, every deleted session keeps its bridge process alive and its slot occupied.
- Same shape for `active_turns_per_session`, smaller per-entry footprint (just an `Arc<AtomicUsize>`) but unbounded growth.

Code review 1 P4 flagged this; review 2 reaffirmed it as still unfixed. Beta-grade users running the desktop for days hit this directly.

## What Changes

In `coordinator.delete_session` (the canonical entry point — verified by call-graph trace; see design.md §Context):

1. Cancel any in-flight turn for the session via `cancel_active_turn_for_session(session_id, 2s)` (best-effort; matches the cascade path's existing pattern).
2. Remove `session_id` from `runtime_sessions`; if a session was cached, dispose it explicitly (this path runs the bridge's clean shutdown — `kill_on_drop` is the safety net underneath).
3. Remove `session_id` from `active_turns_per_session`.
4. Hand off to `session_manager.delete_session(...)` exactly as today.
5. Add a doc comment marking `coordinator.delete_session` as the canonical entry point and warning future code paths against bypassing it.

No public API change. No spec change.

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None — internal coordinator state machinery, not covered by any current spec._

## Impact

- **Code:** `MyBitFun/src/crates/core/src/agentic/coordination/coordinator.rs` only (~12 added lines + doc comment).
- **Behaviour on happy path (no in-flight turn):** identical UI outcome, plus the per-session memory and any cached bridge child process are now released at deletion time instead of at process exit.
- **Behaviour with in-flight turn:** the in-flight turn is cancelled (same as the cascade path's behaviour today), then the runtime session is disposed cleanly, then the rest of session deletion proceeds.
- **Out of scope (separate change later):** `active_subagent_executions` lifecycle (different keying — subagent-execution, not session — and uses an RAII disarm pattern, see field at coordinator.rs:303). TTL-based scavenger (process exit already cleans; live-process leak is the only target here).
