## Context

Today's runtime spawn task (claude / OMP, `coordinator.rs:2704~2867`) lives outside every cancellation system bitfun has. The bitfun path registers a `CancellationToken` with `execution_engine` (`coordinator.rs:2916`); subagents register with `cancel_active_subagents_for_parent_turn`; tool execution registers with `tool_pipeline.cancel_dialog_turn_tools`. The runtime spawn task registers with **none** of these. `cancel_dialog_turn`'s steps run, but every signalling call is a no-op against runtime turns, and `wait_session_drained`'s 1500 ms loop just rides out the deadline.

Concrete symptoms (review3 §P-2 reproduction):
- User starts a long Claude turn (30 s+ thinking time), hits ESC mid-stream.
- Session state flips to Idle in ~3 ms; UI updates.
- Bridge child keeps streaming text deltas to a dropped receiver; SDK keeps consuming Anthropic API tokens; `wait_session_drained` deadlines at 1500 ms; `cancel_active_turn_for_session`'s 2 s polling window deadlines at 2 s; `delete_session` returns at ~3.5 s total — but the bridge survives until natural `TurnEnd` or the new 120 s `IDLE_TIMEOUT_MS` from review2 batch.

Existing safety nets (`kill_on_drop(true)` from `cb2832ae`, dispose-on-error from review3 batch1) only fire **after** the spawn task itself exits. They prevent the bridge from outliving the spawn task; they don't shorten the cancel→quiesce window.

## Goals / Non-Goals

**Goals:**
- ESC and "delete session" stop the bridge child in O(stream-poll quantum + dispose) — single-digit tens of ms, not seconds.
- Runtime-cancelled turns emit `DialogTurnCancelled` — **event-parity** with bitfun cancel (not persistence-parity; see D7 / review-finding F-3) — instead of silently completing as `TurnEnd { Completed }` because the API call won the race.
- A cancel that lands *before* the bridge call (e.g. during the cold-start `create_session` window) does **not** silently complete the turn, and does **not** burn an Anthropic API call (pre-prompt check, D8 / F-2).
- Zero changes to public traits (`AgentSession`, `AgentRuntime`, `ExecutionEngine`).
- Zero changes to runtime adapters (`claude_runtime.rs`, OMP) and `bridge.mjs`.
- Panic-safe and leak-safe — no leaked `runtime_turn_cancels` entry on any exit path: the spawn body, the early-error `?` paths *before* the spawn (the entry is now inserted before `create_session` per D4), or panic unwind. Enforced by the `RuntimeCancelGuard` RAII (D3).

**Non-Goals:**
- Unifying runtime turn lifecycle with `ExecutionEngine` (review3 architecture note §1, review3 §6 long-term item). Out of scope; needs its own RFC.
- Folding `runtime_sessions` + `active_turns_per_session` + `runtime_turn_cancels` into a single `RuntimeSessionEntry` struct (review3 §2). Out of scope.
- Closing the concurrent-insert race (review3 §P-6). Out of scope; orthogonal.
- **Persistence-parity with bitfun cancel.** The runtime spawn path does not persist turn records through `session_manager` on *any* exit (success, error, or cancel) — it only emits events. So the cancel branch emitting `DialogTurnCancelled` without calling `session_manager.cancel_dialog_turn` is consistent with the runtime path's existing behaviour, *not* a regression. The pre-existing "runtime turns are never persisted" gap (and the resulting loss of partial assistant text on session reload) is tracked separately (F-3); out of scope here.
- Making runtime adapters aware of cancellation. Adapters expose `dispose()` and `kill_on_drop`; we drive both from the coordinator.

## Decisions

### D1. Per-turn map keyed by `turn_id`, not per-session

```rust
runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>,  // key = turn_id
```

Reviewer's sketch in review3 §P-2 was per-session (`runtime_session_cancels: DashMap<String, CancellationToken>`). That has a sticky-cancel bug: `CancellationToken::cancel()` is permanent — once a session's token has been fired, every subsequent turn started under that session would see `is_cancelled() == true` and exit immediately on entering the spawn body. The user would think they cancelled the *current* turn but every future prompt() in the same session would silently never run.

Workarounds (per-session, fresh token each turn — but then concurrent turns share state, or replace-and-cancel-old which is racy) all get worse than just keying by `turn_id`. The bitfun path already keys cancel tokens by `turn_id` (`execution_engine.register_cancel_token(&turn_id, token)`); aligning with that is the natural choice.

**Alternative considered:** `parent.child_token()` — give each turn a child of a session-level parent token. Allows session-level "cancel everything" semantics for free. **Rejected** because no caller wants session-level group cancel today, and `delete_session` already iterates the only natural group (current turn) via `cancel_active_turn_for_session` → `cancel_dialog_turn`. Adds API surface for a phantom requirement.

### D2. `tokio::select!` wraps only the event-stream loop, not `prompt()`

> **Schematic only (F-4).** The block below shows the *control-flow shape*.
> The authoritative implementation — exact guard placement, the
> `run_runtime_event_loop` helper extraction, and the D8 pre-prompt check —
> lives in the technical design
> (`docs/superpowers/specs/2026-05-29-fix-runtime-turn-cancellation-design.md`).
> Where the two documents previously diverged on guard placement, the
> technical design wins; this banner exists to keep them reconciled.
> Note in particular: `TurnLifecycleGuard` stays in the spawn **closure** (it
> needs `session_manager` / `active_counter`, which the helper does not take);
> `RuntimeCancelGuard` is constructed **inside the helper** as a struct literal
> (no `::new`). A *second* `RuntimeCancelGuard` is also constructed on the
> **calling thread** before `tokio::spawn` — see D4.

```rust
// schematic — see technical design for the real shape
tokio::spawn(async move {
    let _guard = TurnLifecycleGuard::new(...);     // batch 2 — in the closure
    run_runtime_event_loop(rt_session, user_input, cancel_token,
                           runtime_turn_cancels, ...).await;
    // _guard drops here: counter -= 1, reset Processing → Idle
});

// inside run_runtime_event_loop(...):
let _cancel_guard = RuntimeCancelGuard::armed(runtime_turn_cancels, turn_id); // D3

// D8: a cancel may have fired during the cold-start create_session window,
// before we ever reached select!. Check before prompt() so we neither run a
// zombie turn nor burn an Anthropic call.
if cancel_token.is_cancelled() {
    let _ = event_queue.enqueue(AgenticEvent::DialogTurnCancelled { ... },
                                Some(EventPriority::High)).await;
    let _ = rt_session.dispose().await;
    return;
}

let mut stream = match rt_session.prompt(&user_input, vec![]).await {
    Ok(s) => s,
    Err(e) => { /* existing prompt() Err handling */ }
};

loop {
    tokio::select! {
        biased;  // check cancel first when both are ready
        _ = cancel_token.cancelled() => {
            let _ = event_queue.enqueue(
                AgenticEvent::DialogTurnCancelled { ... },
                Some(EventPriority::High),  // F-7: match runtime Aborted arm, not bitfun Critical
            ).await;
            let _ = rt_session.dispose().await;  // kills bridge child
            return;
        }
        event = stream.next() => {
            match event {
                Some(RuntimeEvent::TextDelta { ... }) => { ... }
                /* existing match arms */
                None => break,  // stream exhausted
            }
        }
    }
}

// existing put-back tail (slot.replace + displaced.dispose)
```

Why not wrap `prompt()` itself? Looking at `claude_runtime.rs:280-317`, `prompt()` does:
1. allocate an mpsc channel,
2. register tx in `self.event_tx`,
3. write a single JSON line to bridge stdin,
4. return the rx wrapped as a stream.

No long awaits, no network round-trip, no SDK call. Step 3 *could* in theory hang if the bridge isn't reading stdin, but the only ways that happens are bridge bugs (which `kill_on_drop` then catches) or system-level pipe failures. Wrapping `prompt()` in `select!` against a cancel token would buy ~milliseconds of latency in the pathological case at the cost of an additional state-machine. **Not worth it.**

`biased;` selects the cancel branch first when both branches are ready — guarantees that a cancel signalled *during* event processing wakes us before we hand the next event off to `event_queue`.

**Alternative considered:** poll the cancel token at the top of each loop iteration, no select. **Rejected** — only checks cancellation between events; if the stream is silent for 30 s while the SDK retries, cancel doesn't fire until the next event.

### D3. `RuntimeCancelGuard` — module-scope struct **with `armed` flag**, not extension of `TurnLifecycleGuard`

`TurnLifecycleGuard` (batch 2) is shared between bitfun and runtime spawn paths. The bitfun path has no `runtime_turn_cancels` entry, so making `TurnLifecycleGuard` clean up the cancel map would force a `Option<Arc<DashMap<String, CancellationToken>>>` field carried through bitfun for nothing. So a separate guard is correct.

```rust
struct RuntimeCancelGuard {
    map: Arc<DashMap<String, CancellationToken>>,
    turn_id: String,
    armed: bool,
}
impl RuntimeCancelGuard {
    fn armed(map: Arc<DashMap<String, CancellationToken>>, turn_id: String) -> Self {
        Self { map, turn_id, armed: true }
    }
    fn disarm(&mut self) { self.armed = false; }
}
impl Drop for RuntimeCancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.map.remove(&self.turn_id);
        }
    }
}
```

**Why `armed` (this reverses the earlier draft, which had no flag).** D4 now inserts the entry on the calling thread *before* `create_session` (to close the cold-start cancel window — see D4 / F-1). That means entry-removal ownership has to cover **two** regimes:

1. **Calling thread, pre-spawn.** Between the insert and `tokio::spawn`, `registry.get(...)` or `create_session(...)` can return `Err` via `?`. On those early returns the entry would leak. A calling-thread `RuntimeCancelGuard` (armed) cleans it up on the `?` unwind.
2. **Spawn body.** Once `tokio::spawn` succeeds, the spawn task owns the lifecycle. The calling thread calls `.disarm()` on its guard immediately after `spawn` returns, transferring ownership; a *second* `RuntimeCancelGuard::armed(...)` constructed inside the helper now owns removal across every spawn-body exit path (D8 pre-prompt return, cancel branch, prompt() Err, stream-exhausted, put-back fall-through, panic unwind).

This is exactly the `ActiveTurnRegistration { armed } → TurnLifecycleGuard` ownership-transfer pattern the bitfun path already uses across its own `tokio::spawn` boundary (`coordinator.rs:2894~2910` + `3071`). The earlier draft claimed "no `armed` needed because the guard is constructed inside the spawn task" — that was true only while the insert lived *inside* the spawn body. Moving the insert earlier (mandatory to fix F-1) is what brings the flag back.

Both guards share the same `Arc<DashMap>` (cheap clone). `DashMap::remove` on a missing key is a no-op, so even if the disarm were ever skipped the double-remove is harmless — but `disarm()` makes the common path do exactly one remove, from the spawn body.

**Alternative considered:** explicit `runtime_turn_cancels.remove(&turn_id)` at every exit path. **Rejected** — exactly the duplication that batch 2 just removed for `fetch_sub`/`reset_state`. RAII keeps one source of truth.

**Alternative considered:** keep the insert *after* `create_session` (no calling-thread guard, no `armed`). **Rejected** — that is the F-1 bug: a cold-start cancel during the 100–500 ms Node-spawn window finds no entry and is silently dropped while the turn runs to completion.

### D4. Insert into `runtime_turn_cancels` **immediately after `start_dialog_turn` returns** — before `create_session`, before `tokio::spawn`

This is the F-1 correction. The original draft inserted the token after `create_session` / `active_counter.fetch_add`. But `session.state.current_turn_id` — which `cancel_dialog_turn` reads to find what to cancel — becomes visible the moment `start_dialog_turn` returns. Everything between that point and the insert is an unguarded cancel window. And on a **cold start that window is large**: `create_session` spawns a Node process and loads the SDK (100–500 ms). A user who hits ESC during it would have `cancel_dialog_turn` flip the state to Idle, find **no entry** in `runtime_turn_cancels`, fire nothing — then we insert a fresh (un-cancelled) token and the turn runs to completion, re-rendering itself as completed. The user's cancel is silently lost.

Fix: insert as the very first thing after `start_dialog_turn`, before the `DialogTurnStarted` emit, before `registry.get`, before `create_session`:

```rust
let turn_id = self.session_manager.start_dialog_turn(...).await?;

// F-1: make the cancel token reachable the instant current_turn_id is visible,
// i.e. before the create_session await chain (cold-start Node spawn).
let cancel_token = CancellationToken::new();
self.runtime_turn_cancels.insert(turn_id.clone(), cancel_token.clone());
// Calling-thread guard: removes the entry if any `?` below (registry.get,
// create_session) bails before we hand ownership to the spawn task.
let mut cancel_entry_guard =
    RuntimeCancelGuard::armed(self.runtime_turn_cancels.clone(), turn_id.clone());

self.emit_event(AgenticEvent::DialogTurnStarted { ... }).await;
let runtime = registry.get(runtime_id).cloned().ok_or_else(...)?;  // ? → guard removes entry
let rt_session = { /* slot take-or-create */
    runtime.create_session(...).await.map_err(...)?               // ? → guard removes entry
};
let active_counter = ...; active_counter.fetch_add(1, Ordering::SeqCst);

// ... clones, including runtime_turn_cancels.clone() and cancel_token.clone() ...
tokio::spawn(async move {
    let _guard = TurnLifecycleGuard::new(...);          // owns counter + state on spawn exit
    run_runtime_event_loop(rt_session, ..., cancel_token, runtime_turn_cancels, ...).await;
    // run_runtime_event_loop builds its own armed RuntimeCancelGuard internally (D3 regime 2)
});
cancel_entry_guard.disarm();  // spawn succeeded → spawn body now owns entry removal
return Ok(());
```

The entry must be visible to `cancel_dialog_turn` *before* `handle_user_input` returns `Ok(())` — same reasoning as `active_counter.fetch_add`. Moving it earlier strictly widens that visibility to cover `create_session` too.

Cancelling a token *before* the spawn body runs is safe and is the whole point: the spawn body's **D8 pre-prompt check** sees `is_cancelled()` and returns before calling `prompt()` — no bridge call, no API spend. (If the cancel instead lands after `prompt()`, the `select!` cancel branch handles it.) Net: a cold-start cancel is now honoured at worst one cheap `prompt()`-skip later, never silently dropped.

**On `wait_session_drained` during this window:** if the cancel fires before `fetch_add`, the counter is still 0, so `wait_session_drained` returns "drained" immediately. That is fine — the spawn task will self-abort at the D8 check and never start a bridge call, so there is nothing to drain. The user-visible outcome (state Idle, `DialogTurnCancelled` emitted, no API spend) is correct.

### D5. Hook `cancel_dialog_turn` after existing cancel calls, before `wait_session_drained`

`cancel_dialog_turn` (`coordinator.rs:3287~3383`) currently does:
1. update session state to Idle (conditional)
2. emit SessionStateChanged
3. cancel via `execution_engine` + `tool_pipeline` + subagents (Steps 3a/b/c — all no-op for runtime turns today)
4. `wait_session_drained(1500ms)`

Insert the runtime cancel between step 3 and step 4:

```rust
// Step 3.5: signal runtime spawn task (if this turn is on the runtime path).
// No-op for bitfun turns (no entry); for runtime turns this is the only
// signal that actually reaches the spawn loop.
//
// Clone the token out and drop the DashMap Ref *before* calling cancel():
// holding a shard read-guard across cancel() is safe today (cancel only
// touches atomics + wakers), but cloning-then-dropping removes any chance a
// future cancel() side-effect that re-enters the map deadlocks on the same
// thread. The token is an Arc internally; the clone is cheap.
let runtime_cancel = self
    .runtime_turn_cancels
    .get(dialog_turn_id)
    .map(|entry| entry.value().clone());
if let Some(token) = runtime_cancel {
    token.cancel();
}
```

`get` + clone (not `remove`) — let `RuntimeCancelGuard::drop` own removal so we don't race with the spawn task removing it twice. `cancel()` on an already-cancelled token is a no-op; on a not-yet-cancelled token it fires and the spawn task picks it up next poll (or at the D8 pre-prompt check if it hasn't reached `select!` yet).

`wait_session_drained` then sees the counter actually decrement quickly (instead of always deadline-out) because the spawn task disposes + returns, drops `TurnLifecycleGuard`, decrements counter.

`delete_session` reaches this hook automatically: it calls `cancel_active_turn_for_session(session_id, 2s)` (line 3444), which reads `session.state.current_turn_id` and calls `cancel_dialog_turn(session_id, &current_turn_id)`. No changes needed in `delete_session` itself.

### D6. Concurrent-turn safety (P-6 race) is **explicitly out of scope**

If two `handle_user_input` calls land in the same session at the same time (review3 §P-6), both insert into `runtime_turn_cancels` under different `turn_id` keys. `cancel_dialog_turn` only cancels the one matching `session.state.current_turn_id`. The other races on. This is no worse than today, and addressing it requires the `RuntimeSessionEntry` refactor we're explicitly deferring.

### D7. `dispose()` in cancel branch, no put-back; emit-only (event-parity, not persistence-parity)

Cancel branch consumes `rt_session` via `dispose().await` and returns. The put-back code below the loop never runs. Slot stays `None` (or holds a session displaced concurrently — handled by the existing put-back's `replace()` logic on the *next* turn). Next turn for this session will create a fresh `ClaudeSession` via `or_insert_with`. Matches the existing prompt() Err and `RuntimeEvent::Error` semantics from review3 batch1: any non-happy-path exit disposes; only the success path puts back.

Alternative — abort+keep-session — would require `rt_session.abort()` (which exists on the trait, line 112 of `agent_runtime.rs`) instead of `dispose()`. **Rejected** because abort still kills the bridge child (`claude_runtime.rs:319-329`: same `child.kill()` as dispose), so the cached session would be unusable anyway.

**Event-parity, not persistence-parity (F-3).** The cancel branch emits `DialogTurnCancelled` and disposes — it does **not** call `session_manager.cancel_dialog_turn` the way bitfun's `persist_cancelled_dialog_turn` (`coordinator.rs:1492`) does. This is deliberate and consistent: the runtime spawn path never touches `session_manager` persistence on *any* exit — on success it emits `DialogTurnCompleted` but does not call `complete_dialog_turn`; on error it emits `DialogTurnFailed` but does not persist. Adding persistence only to the cancel branch would be the odd one out. The whole "runtime turns are not persisted through `session_manager`" gap (which also means partial assistant text streamed before a cancel/complete is lost on session reload) is a pre-existing, separately-tracked issue — see Non-Goals and F-3. Confirm during build whether an event subscriber persists these turns out-of-band; if not, file the persistence gap as its own change.

### D8. Pre-`prompt()` cancellation check inside the helper (F-2)

A cancel can fire during the cold-start `create_session` window (D4) — i.e. *before* the spawn body ever reaches `select!`. The token is already cancelled when the helper starts. Without an explicit check, the helper would still call `rt_session.prompt(...)`, which writes the prompt command to the bridge stdin and **starts an Anthropic API call** (billed) before the first `select!` poll fires the cancel branch and disposes.

So the helper, right after constructing its `RuntimeCancelGuard` and before `prompt()`, checks:

```rust
if cancel_token.is_cancelled() {
    let _ = event_queue.enqueue(
        AgenticEvent::DialogTurnCancelled { session_id, turn_id },
        Some(EventPriority::High),
    ).await;
    let _ = rt_session.dispose().await;
    return; // RuntimeCancelGuard + (closure's) TurnLifecycleGuard clean up
}
```

This closes the "cancelled-before-prompt" path with no API spend. The post-`prompt()` cancel path is still handled by the `select!` cancel branch (D2). Together D4 (entry reachable early) + D8 (skip prompt if already cancelled) make a cold-start cancel cost at most one cheap `prompt()`-skip, never a silent completion and never a wasted API call.

**Why not wrap `prompt()` in `select!` instead (one mechanism)?** Per D2, `prompt()` has no long await and the borrow/state-machine cost of `select!`-wrapping it isn't worth it. A single `is_cancelled()` boolean check is far cheaper and covers the only case that matters (already-cancelled-on-entry).

## Risks / Trade-offs

- **[Risk] `dispose().await` in cancel branch could itself hang.** → `dispose()` only does `abort_token.cancel()` + `child.kill()`. Both are bounded operations (cancel is a flag set, kill is `TerminateProcess` on Windows / `SIGKILL` on Unix). No await chains. Safe.

- **[Risk] `event_queue.enqueue(DialogTurnCancelled)` could block under load.** → The cancel signal has already been delivered upstream; the enqueue is for UI notification. Worst case: UI sees the cancellation a few ms late. The bridge is already getting killed.

- **[Trade-off] Per-turn map grows linearly with active turns.** → Active turn count is bounded by user concurrency (single-digit per user); `DashMap` handles this trivially. Each entry is ~40 bytes (turn_id String + CancellationToken Arc). Negligible.

- **[Trade-off] We're adding *yet another* per-session/per-turn DashMap.** → Acknowledged. The design.md for the future `RuntimeSessionEntry` refactor will collapse `runtime_sessions` + `active_turns_per_session` + `runtime_turn_cancels` into one entry. Until then, the marginal cost of one more map is small compared to the cost of a half-finished entry refactor.

- **[Trade-off] `biased;` in `select!` means a flooded event stream could starve cancel checks.** → False — `biased;` *prefers* cancel when both are ready. It does not prevent the event branch from being polled when only the event branch is ready. Tokio's `select!` round-robins fairly when neither is `biased`; `biased` says "if cancel is ready, take cancel". Exactly what we want.

- **[Note] Cross-scope guard drop order (F-5).** Two RAII guards clean up on the spawn path, in different scopes. `RuntimeCancelGuard` (armed) lives **inside** `run_runtime_event_loop`; `TurnLifecycleGuard` lives in the spawn **closure** that calls the helper. On helper return the order is: `RuntimeCancelGuard::drop` first (removes the `runtime_turn_cancels` entry), then control returns to the closure and `TurnLifecycleGuard::drop` runs (counter `-= 1`, reset Processing→Idle). They touch disjoint state, so order is not load-bearing for correctness — but it is documented so the build phase does **not** "simplify" by passing `session_manager`/`active_counter` into the helper and folding both guards together (which would couple bitfun's `TurnLifecycleGuard` to runtime-only state). Keep them split.

- **[Note] Cancel-branch event priority (F-7).** Emit `DialogTurnCancelled` with `EventPriority::High`, matching the runtime path's own `TurnEnd { Aborted }` arm (`coordinator.rs:2805`). Bitfun's `persist_cancelled_dialog_turn` uses `EventPriority::Critical` (`coordinator.rs:1513`); we intentionally align with the *runtime* sibling arm, not the bitfun helper, for within-path consistency.

- **[Trade-off] `armed` flag re-introduces a small amount of guard ceremony (D3).** → Acknowledged. It is the cost of moving the insert before `create_session` (F-1): entry-removal ownership now spans the `tokio::spawn` boundary, exactly like `ActiveTurnRegistration`/`TurnLifecycleGuard` on the bitfun path. The alternative (insert after `create_session`, no flag) leaves the cold-start cancel window open, which is the bug we are fixing.

## Migration Plan

Single commit on `main`. No feature flag — the new code path only activates for runtime sessions (`runtime_id != "bitfun"`), which today means only the claude SDK runtime in production. Bitfun spawn task is unaffected.

Rollback: `git revert`. The `runtime_turn_cancels` field becomes orphaned but the rest of the system continues to function — no cascading cleanup needed.

## Open Questions

- **[Build-time confirm, F-3] Does any event subscriber persist runtime turn records?** The runtime spawn path emits `DialogTurnCompleted` / `DialogTurnFailed` / `DialogTurnCancelled` but never calls `session_manager` persistence. A grep during design did not find a subscriber that turns these events into persisted turn records. If none exists, partial assistant text from a cancelled (or completed) runtime turn is lost on session reload. This change does **not** fix that (Non-Goals); the build phase must confirm the behaviour and, if the gap is real, file it as a separate change. Not a blocker for this cancellation fix.

Otherwise none. The design maps to review3's recommendations — with the per-turn keying correction (D1), the early-insert + `armed`-guard correction (D3/D4, finding F-1), and the pre-prompt check (D8, finding F-2) — and aligns with patterns already in `coordinator.rs`.
