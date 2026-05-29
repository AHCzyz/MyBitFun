## Context

review3 §P-3 identifies that the runtime spawn task (added in commit `82970140`) was structurally weaker than the bitfun spawn task it sits next to. Both spawn tasks own a per-session `Arc<AtomicUsize>` counter that gates `wait_session_drained`, and both are responsible for resetting `Session.state` from `Processing` back to `Idle` if the task exits without going through `persist_completed_dialog_turn` / `persist_cancelled_dialog_turn` / `persist_failed_dialog_turn`.

The bitfun path uses an inline RAII guard (`SessionExecutionGuard`, declared inside the spawn closure at `coordinator.rs:3036~3072`) so panics, early returns, and normal completion all hit the same cleanup. The runtime path was written with manual `fetch_sub + reset_state` triples on each of three exit paths (prompt() Err at 2679~2683, RuntimeEvent::Error at 2798~2802, success put-back at 2821~2825). A panic on any `await` inside the loop body — `event_queue.enqueue`, `stream.next`, an internal `unwrap` added in the future — bypasses every triple.

This is a hotfix, not a refactor: the goal is to **mirror the bitfun pattern in the runtime path**, not to redesign turn lifecycle ownership. The reviewer also flagged §2 ("RuntimeSessionEntry struct") as a long-term direction; that is explicitly out of scope here.

## Decisions

### D1. Lift `SessionExecutionGuard` to module level as `TurnLifecycleGuard`

The bitfun path's inline definition becomes:

```rust
struct TurnLifecycleGuard {
    session_manager: Arc<SessionManager>,
    session_id: String,
    turn_id: String,
    active_counter: Arc<AtomicUsize>,
}

impl TurnLifecycleGuard {
    fn new(...) -> Self { ... }
}

impl Drop for TurnLifecycleGuard {
    fn drop(&mut self) {
        self.active_counter.fetch_sub(1, Ordering::SeqCst);
        self.session_manager
            .reset_session_state_if_processing(&self.session_id, &self.turn_id);
    }
}
```

The bitfun spawn closure references this lifted type — no behavioural change.

The reviewer's sketch in review3.md uses an `armed` flag with a `disarm()` method (matching the outer `ActiveTurnRegistration` pattern at `coordinator.rs:2853~2872`). We **omit** `armed`/`disarm()` here because:

1. The runtime spawn task has no scenario where we want to suppress the cleanup — every exit path needs the decrement and state reset.
2. The bitfun outer `ActiveTurnRegistration` uses `armed` because cleanup ownership transfers from the calling thread to the spawned task at `tokio::spawn`. The inner `SessionExecutionGuard` (lifted here) lives entirely inside the spawn task and never transfers ownership, so it has no use for `disarm()`.

This keeps the lifted struct minimal and faithful to the inline original.

### D2. The outer `ActiveTurnRegistration` (bitfun-only) stays as-is

`ActiveTurnRegistration` at `coordinator.rs:2853~2872` exists to plug a different gap: the window between `fetch_add` (line 2852) and `tokio::spawn` (line 3027) on the bitfun path includes async work (`emit_event`, `get_context_messages`, title generation spawn), so a panic there must also decrement the counter. Its sole responsibility is the synchronous fetch-add window, distinct from the inner guard's responsibility for the spawn body. Folding them is more invasive than this hotfix warrants.

The runtime path has only trivial Arc clones between `fetch_add` (line 2649) and `tokio::spawn` (line 2658), so it does not need an outer `ActiveTurnRegistration`. We keep `fetch_add` on the calling thread (so `wait_session_drained` cannot observe a zero-counter window after `handle_user_input` returns Ok) and let `TurnLifecycleGuard` take ownership at the top of the spawn closure.

### D3. `dispose()` calls stay path-specific

The new guard handles only counter + session-state cleanup. `rt_session.dispose()` is **path-specific**:

- prompt() Err → `dispose()` (session in bad state, don't cache).
- `RuntimeEvent::Error` → `dispose()` (review3 batch1 P-5 made this so).
- success / TurnEnd → put-back into `session_slot` for reuse by the next turn (`replace()` returns any concurrent displaced session, which we dispose).

`dispose()` ordering relative to the guard drop is: `dispose().await` first, then the closure ends and `_guard` drops (decrement + reset). This matches the existing prompt() Err and RuntimeEvent::Error sequencing exactly — we are removing the manual decrement/reset pair, not the dispose call.

## Non-Decisions

- **Do not** unify the runtime path's `dispose()` into the guard. The guard's `Drop` is synchronous; `dispose()` is `async`. Forcing it through `tokio::task::spawn` from within `Drop` would orphan the dispose work from the spawn task's lifetime and lose error context.
- **Do not** introduce a `runtime_session_cancels: DashMap<String, CancellationToken>` here (review3 §P-2). That fix needs full Comet flow — open → design → build → verify — because it changes the cancel semantics, not just panic safety.
- **Do not** rewrite the bitfun path beyond hoisting the guard struct. It already works.
