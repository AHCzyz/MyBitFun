## Context

The multi-runtime path in `handle_user_input` follows this shape (verified by reading the post-baseline `0b4ff520` source):

```
take session from runtime_sessions[session_id]      ← under Mutex
if None: create_session()                           ← still under Mutex
active_counter.fetch_add(1)
spawn task {
  prompt(input) → stream
    Err  → emit DialogTurnFailed; dispose; RETURN  ← (P1 hole)
  while stream.next() { ... }
  put session back: *slot_guard = Some(rt_session)  ← (P5 hole: silent overwrite)
  active_counter.fetch_sub(1)
  reset_session_state_if_processing()
}
```

The success path tears down state correctly. The early-`return` branch and the put-back step are where the bugs live.

## Goals / Non-Goals

**Goals:**
- Make the runtime-error path leave the session state machine in the same shape as the success path.
- Make the concurrent-turn race produce *no* leaked child processes, even when two turns finish in unexpected order.
- Ensure any future code path that drops a `Box<dyn AgentSession>` for `ClaudeSession` does not orphan the bridge process.

**Non-Goals:**
- Apply the same fix to `omp_runtime.rs` / `bitfun_runtime.rs` — out of scope per review1.md flagging only ClaudeRuntime; tracked as follow-up.
- Eliminate concurrent session creation in the first place (would require holding the Mutex across the entire spawn task, or moving to an actor-per-session model). Hotfix accepts the brief duplication and disposes the loser.
- Adding integration tests that mock a runtime to drive these paths. The hotfix verifies via cargo check + targeted code review; runtime mocking infrastructure does not exist in the repo and standing it up is out of scope.

## Decisions

### D1. P1 fix: explicit cleanup before `return`

The minimal honest fix: mirror what the success path's tail already does. After `dispose()` and before `return`, call `active_counter.fetch_sub(1, Ordering::SeqCst)` and `session_manager.reset_session_state_if_processing(&session_id_clone, &turn_id_clone)`.

**Alternative considered**: an RAII guard like the existing `ActiveTurnRegistration` used in the bitfun branch (`disarm()` on success, auto-decrement on Drop). Cleaner but expands scope (touches more lines, requires moving the state-reset call into Drop too — which is `async`, awkward in Drop). Rejected for hotfix; could be a future refactor.

### D2. P5 fix part A: replace-and-dispose at put-back

Change the put-back from:
```rust
*slot_guard = Some(rt_session);
```
to:
```rust
let prev = slot_guard.replace(rt_session);
drop(slot_guard);                       // release lock before async dispose
if let Some(prev_session) = prev {
    let _ = prev_session.dispose().await;
}
```

`Mutex<Option<T>>::replace` swaps the inner Option and returns the previous value. If a concurrent turn already wrote a session into the slot, that session is now in `prev` and we dispose it cleanly. If the slot was empty (the common single-turn case), `prev` is `None` and we do nothing.

**Why drop the guard before `dispose().await`**: `dispose` is async and may block; holding the slot's mutex during it would serialize unrelated turns. Releasing first is safe because the slot now correctly holds `rt_session` — any third concurrent turn will find a Some and reuse it.

### D3. P5 fix part B: `kill_on_drop(true)` on the Child

In `claude_runtime.rs::create_session`, the `Command::new(&node_binary)` builder chain currently lacks `.kill_on_drop(true)`. Tokio's `Child` defaults to *false*, meaning Drop detaches the process. Add the call so any drop path — explicit, panic, future displacement, abort cancellation — kills the bridge.

This is **defense-in-depth on top of D2**, not a replacement for it. D2 disposes cleanly (cancel reader task + kill child via the abort_token + child.kill); D3 is the safety net for any path D2 doesn't catch.

**Alternative considered**: implement `Drop for ClaudeSession` that signals abort and kills the child. Rejected — tokio's `Mutex<Child>` and the abort_token aren't easily usable in a synchronous `Drop`, and `kill_on_drop` is the idiomatic mechanism that already does the right thing.

## Risks / Trade-offs

- **D2 still creates duplicate sessions on a race.** One of the two will eventually be disposed, but for the duration of the race both Node bridges run concurrently, doubling memory/CPU briefly. Accepted: races are rare (user must send two messages within one network round-trip), and the work is bounded.
- **`kill_on_drop` interacts with abort timing.** When `dispose()` is called, it already kills the child explicitly. After `dispose` returns, the `Box<Self>` is dropped and `kill_on_drop` runs again on the (already-killed) child — a no-op idempotently. Verified safe by inspection of `tokio::process::Child::drop`.
- **D1 doesn't deduplicate the cleanup logic.** Same two lines now appear in both error and success branches. Accepted as a hotfix; an RAII consolidation is a follow-up.

## Migration / rollback

Single-commit revert by reverting the hotfix commit. No data migration. No interface change.

## Open questions

None blocking. Tracked as follow-up:
- Apply same `kill_on_drop` + put-back replace pattern to `omp_runtime.rs` / `bitfun_runtime.rs`.
- Refactor concurrent-turn handling so duplicate session creation never happens (actor-per-session or per-turn semaphore).
