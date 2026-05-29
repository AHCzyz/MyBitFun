# Comet Design Handoff

- Change: fix-runtime-turn-cancellation
- Phase: design
- Mode: compact
- Context hash: a039912e1f92e6ca051dc81c4731cf4c1c4fa9884aabb122ea8472c4977449ca

Generated-by: comet-handoff.sh

OpenSpec remains the canonical capability spec. This handoff is a deterministic, source-traceable context pack, not an agent-authored summary.

## openspec/changes/fix-runtime-turn-cancellation/proposal.md

- Source: openspec/changes/fix-runtime-turn-cancellation/proposal.md
- Lines: 1-37
- SHA256: a5954f9e034283f16514f4ef2199edc380dea51bb7187a6a49b12275e5c56096

```md
## Why

review3 §P-2 identified that mid-turn cancellation **does not propagate to runtime (claude / OMP) spawn tasks**. `cancel_dialog_turn` and `delete_session` both walk through `cancel_active_turn_for_session`, which signals `execution_engine.cancel_dialog_turn`, `tool_pipeline.cancel_dialog_turn_tools`, and `cancel_active_subagents_for_parent_turn` — but the runtime spawn task never registers with any of those systems. ESC and "delete session" are *visible* (session state flips to Idle, UI updates) but not *actual* (the bridge keeps streaming from Anthropic until the SDK iterator naturally finishes or hits the 120 s `IDLE_TIMEOUT_MS`, billing user tokens during the gap).

`kill_on_drop(true)` (commit `cb2832ae`) and review3 batch1's dispose-on-error cleanup are the current safety nets — both only fire **after** the spawn task itself returns. They prevent leaks; they do not shorten the cancel-to-quiesce window.

## What Changes

- `coordinator.rs`:
  - **New field** on `ConversationCoordinator`: `runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>`, keyed by `turn_id` (one token per turn — mirrors the bitfun `execution_engine.register_cancel_token(&turn_id, …)` pattern; per-turn keys avoid the sticky-cancel staleness that a per-session token would have).
  - **Runtime spawn path** (`handle_user_input`): create a fresh `CancellationToken`, insert into `runtime_turn_cancels` before `tokio::spawn`, move a clone into the closure. Wrap the event-stream loop in `tokio::select!` listening on the token alongside `stream.next()`. On cancel: dispose the cached `rt_session` (which kills the bridge child via `kill()`), emit `DialogTurnCancelled` (matching bitfun's cancel semantics), and let `TurnLifecycleGuard` (introduced in batch 2) handle counter + state cleanup. A small RAII guard inside the spawn body removes the `turn_id` entry from `runtime_turn_cancels` on any exit path.
  - **`cancel_dialog_turn`** (line 3287): after the existing `execution_engine` / `tool_pipeline` / subagent cancel calls, look up `runtime_turn_cancels.get(dialog_turn_id)` and `cancel()`. No-op for bitfun turns (no entry); active for runtime turns. `wait_session_drained` downstream then sees the counter decrement quickly.
- **No changes** to: `delete_session` (already routes through `cancel_active_turn_for_session` → `cancel_dialog_turn`); `tool_pipeline`; `execution_engine`; runtime adapters (`claude_runtime.rs`, OMP); `bridge.mjs`.

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None — repairs an implicit contract on existing cancellation operations. No public API or trait surface changes._

## Impact

- **Code:** 1 file (`coordinator.rs`), ~50 lines net (new field + spawn loop `select!` + cancel hook + RAII removal).
- **Behaviour (user-visible):**
  - ESC and "delete session" stop the bridge child within ~50-100 ms (one stream-poll quantum + dispose) instead of up to 120 s.
  - Token / API-quota spend on cancelled or deleted runtime turns drops from "open-ended until idle timeout" to "bounded by dispose round-trip".
  - `DialogTurnCancelled` event now actually fires for runtime turns (previously masked by the natural `TurnEnd { stop_reason: Aborted }` or — more often — by `TurnEnd { stop_reason: Completed }` if the API call finished before the user noticed).
- **Behaviour (internal):**
  - `wait_session_drained` returns 0 within the drain budget on cancelled runtime turns instead of always hitting deadline.
  - `cancel_active_turn_for_session`'s 2 s polling window stops being decorative for runtime sessions.
- **Risk:** Low.
  - Per-turn keying eliminates the `CancellationToken` sticky-state hazard a per-session map would have.
  - The new `DashMap` is purely additive — no ownership shift for existing fields.
  - `tokio::select!` between a cancel token and a stream is already in use in `claude_runtime.rs`'s reader task and is well-trodden.
  - Race surface considered: cancel firing during `prompt()` (still in scope but acceptable — handled in design); cancel firing after stream loop exits naturally (no-op, RAII removal already happened or is racing with cancel — both safe). Detail in design.md.
```

## openspec/changes/fix-runtime-turn-cancellation/design.md

- Source: openspec/changes/fix-runtime-turn-cancellation/design.md
- Lines: 1-296
- SHA256: a82d74e77986c854cc76ce42d93ff39fa92e63d8565a233e4e52322b36a8752a

[TRUNCATED]

```md
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
```

Full source: openspec/changes/fix-runtime-turn-cancellation/design.md

## openspec/changes/fix-runtime-turn-cancellation/tasks.md

- Source: openspec/changes/fix-runtime-turn-cancellation/tasks.md
- Lines: 1-53
- SHA256: 9f65fb103952a086f935e1ae5142f1cbc6bb71733adcf0ace1d2017b3f09facd

```md
> Coordinates verified against `coordinator.rs` after review3 batch1/batch2.
> Build phase: treat line numbers as "around" — the structural anchors
> (function names, existing fields) are authoritative.

## 1. Coordinator state

- [ ] 1.1 Add field `runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>` to `ConversationCoordinator`, immediately after `runtime_sessions` (`coordinator.rs:556`). Key = `turn_id` (per OpenSpec design D1 — **not** session_id; sticky-cancel bug).
- [ ] 1.2 Initialize `runtime_turn_cancels: Arc::new(DashMap::new())` in `ConversationCoordinator::new`, after the `runtime_sessions` init (`coordinator.rs:1071`).

## 2. `RuntimeCancelGuard` (module scope, **armed**)

- [ ] 2.1 Define `struct RuntimeCancelGuard { map: Arc<DashMap<String, CancellationToken>>, turn_id: String, armed: bool }` at module scope, next to `TurnLifecycleGuard` (after `classify_runtime_error`, around `coordinator.rs:97~127`).
- [ ] 2.2 Impl `RuntimeCancelGuard::armed(map, turn_id) -> Self` (sets `armed: true`) and `fn disarm(&mut self) { self.armed = false; }`.
- [ ] 2.3 Impl `Drop`: `if self.armed { self.map.remove(&self.turn_id); }`. (`DashMap::remove` on a missing key is a no-op — idempotent.) Rationale for `armed`: entry-removal ownership crosses the `tokio::spawn` boundary because the entry is inserted on the calling thread before `create_session` (task 3.1 / OpenSpec D3+D4). Mirrors `ActiveTurnRegistration`'s `armed`/`disarm` at `coordinator.rs:2894~2910`.

## 3. `handle_user_input` runtime-branch wiring (early insert + calling-thread guard + disarm)

- [ ] 3.1 **Right after** `start_dialog_turn` returns in the runtime branch (`coordinator.rs:2651`), **before** the `DialogTurnStarted` emit / `registry.get` / `create_session`: construct `let cancel_token = CancellationToken::new();`, `self.runtime_turn_cancels.insert(turn_id.clone(), cancel_token.clone());`, and `let mut cancel_entry_guard = RuntimeCancelGuard::armed(self.runtime_turn_cancels.clone(), turn_id.clone());`. (F-1: the entry must be reachable the instant `current_turn_id` is visible, so a cancel during the cold-start `create_session` window is honoured, not dropped.)
- [ ] 3.2 The calling-thread `cancel_entry_guard` covers the `?` early-returns at `registry.get` (`:2668~2670`) and `create_session` (`:2682~2686`): if either bails, `Drop` removes the orphaned entry. No code change at those `?` sites — the guard does it.
- [ ] 3.3 Add `cancel_token` and a `runtime_turn_cancels` Arc clone to the spawn closure capture list (alongside the existing clones at `:2697~2702`).
- [ ] 3.4 Replace the inline spawn body (`:2717~2865`, from `let mut stream = match rt_session.prompt(...)` through the put-back tail) with a call to the extracted helper: keep `TurnLifecycleGuard::new(...)` in the **closure**, then `run_runtime_event_loop(rt_session, wrapped_user_input, cancel_token, runtime_turn_cancels, event_queue, session_slot_clone, session_id_clone, turn_id_clone, runtime_id_for_log).await;`.
- [ ] 3.5 **After** `tokio::spawn(...)` returns (before `return Ok(())` at `:2869`), call `cancel_entry_guard.disarm();` — spawn succeeded, the spawn-body's own `RuntimeCancelGuard` now owns entry removal. (If we did not disarm, both guards would try to remove — harmless via idempotency, but disarm makes the common path exactly one remove from the spawn body.)

## 4. `run_runtime_event_loop` helper (extracted, testable)

- [ ] 4.1 Define module-private `async fn run_runtime_event_loop(rt_session: Box<dyn AgentSession>, user_input: String, cancel_token: CancellationToken, runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>, event_queue: Arc<EventQueue>, session_slot: Arc<tokio::sync::Mutex<Option<Box<dyn AgentSession>>>>, session_id: String, turn_id: String, runtime_id_for_log: String)`. Add `#[allow(clippy::too_many_arguments)]` (9 params; wrapper struct is future cleanup, YAGNI now).
- [ ] 4.2 First line: `let _cancel_guard = RuntimeCancelGuard::armed(runtime_turn_cancels, turn_id.clone());` (spawn-body regime — always armed; every exit path must remove).
- [ ] 4.3 **Pre-prompt cancel check (F-2 / D8)**, before `prompt()`: `if cancel_token.is_cancelled() { emit DialogTurnCancelled{session_id, turn_id} @ High; let _ = rt_session.dispose().await; return; }`. Closes the cancelled-during-create_session path with **no Anthropic API call**.
- [ ] 4.4 Move the existing `prompt()` `Ok/Err` match in verbatim (the `Err` arm already emits `DialogTurnFailed` + disposes + returns — review3 batch1; unchanged).
- [ ] 4.5 Replace `while let Some(event) = stream.next().await { match event { … } }` with `loop { tokio::select! { biased; _ = cancel_token.cancelled() => { /* 4.6 */ } event = stream.next() => { match event { Some(ev) => match ev { /* existing arms verbatim */ }, None => break } } } }`. The existing `RuntimeEvent::Error` arm keeps its dispose+return (review3 batch1); `TurnEnd` keeps `break`.
- [ ] 4.6 Cancel branch body: `log::info!("Runtime {} turn cancelled by user: session_id={}, turn_id={}", runtime_id_for_log, session_id, turn_id);` then emit `DialogTurnCancelled{session_id, turn_id}` @ `EventPriority::High` (F-7: match the runtime `Aborted` arm at `:2805`, not bitfun `Critical`); `let _ = rt_session.dispose().await;`; `return;`.
- [ ] 4.7 Keep the put-back tail verbatim below the loop (`slot.replace(rt_session)` + dispose displaced). Reached only on the `None`/`TurnEnd→break` happy path.

## 5. `cancel_dialog_turn` integration (clone-then-cancel)

- [ ] 5.1 In `cancel_dialog_turn`, between the existing Step 3 block and the `wait_session_drained(1500ms)` call (`coordinator.rs:3368`), add Step 3.5: `let runtime_cancel = self.runtime_turn_cancels.get(dialog_turn_id).map(|e| e.value().clone()); if let Some(token) = runtime_cancel { token.cancel(); }`. Clone-then-drop-Ref **before** `cancel()` (F-5/D5 footgun avoidance — don't hold the DashMap shard read-guard across `cancel()`). Comment: no-op for bitfun turns (no entry); `get` not `remove` so `RuntimeCancelGuard::drop` owns removal.

## 6. Tests (in existing `mod tests`, `coordinator.rs:5574`)

- [ ] 6.1 Add `#[cfg(test)] struct FakeSession { session_id, event_rx: Mutex<Option<Receiver<AgentEvent>>>, prompt_err: Option<PortError>, disposed: Arc<AtomicBool> }` implementing `AgentSession` (`session_id`/`prompt`/`abort`/`dispose`; `steer` uses the trait default). `dispose` sets `disposed=true`.
- [ ] 6.2 **T1 `runtime_event_loop_cancels_promptly`**: pre-stock an empty rx **and keep the `Sender` alive in a local binding** (F-6: if the Sender drops, `stream.next()` yields `None` immediately and the loop exits via the put-back/completion path — testing the wrong branch). Spawn helper, `sleep(10ms)`, `cancel.cancel()`, `timeout(200ms, task)` must complete. Assert `disposed==true`, `dequeue_batch` saw `DialogTurnCancelled`, `slot.is_none()`.
- [ ] 6.3 **T2 `runtime_event_loop_completes_cleanly`**: pre-stock `[TurnEnd{Completed}]`, drop the Sender after. Run to completion, never fire cancel. Assert `disposed==false`, `slot.is_some()`, event queue saw `DialogTurnCompleted`.
- [ ] 6.4 **T3 `runtime_event_loop_disposes_on_error_event`**: pre-stock `[Error{message:"rate limit"}]`. Run. Assert `disposed==true`, `slot.is_none()`, event queue saw `DialogTurnFailed`. (Regression guard for review3 batch1 P-5.)
- [ ] 6.5 Add a **T4 `runtime_event_loop_skips_prompt_when_precancelled`** (F-2/D8 coverage): construct `FakeSession` whose `prompt` would `panic!`/set a "prompt_called" flag; pre-cancel the token before spawning the helper. Assert the helper emitted `DialogTurnCancelled`, `disposed==true`, and `prompt` was **never** called. (This is the one genuinely-new behaviour the change adds; worth its own case rather than folding into T1.)

## 7. Verification

- [ ] 7.1 `cd MyBitFun && cargo check -p bitfun-core --message-format=short` exits 0 (watch for the `clippy::too_many_arguments` allow being needed; not a `check` failure but confirm under `cargo clippy`).
- [ ] 7.2 `cd MyBitFun && cargo test -p bitfun-core --lib reset_session_state_if_processing` — both pre-existing turn-scoped tests still pass (the `TurnLifecycleGuard` drop relies on turn-scoped reset).
- [ ] 7.3 `cd MyBitFun && cargo test -p bitfun-core --lib runtime_event_loop` — T1–T4 pass.
- [ ] 7.4 Reachability greps: `cancel_token.cancelled()` (1 hit, in helper) and `cancel_token.is_cancelled()` (1 hit, pre-prompt check).
- [ ] 7.5 RAII wiring greps: `RuntimeCancelGuard` = exactly **5** hits (struct def + `impl` block + `impl Drop` + `::armed(` calling-thread instance + `::armed(` helper instance); `runtime_turn_cancels.remove` = **1** hit (only inside the guard's `Drop`); `cancel_entry_guard.disarm()` = **1** hit.
- [ ] 7.6 **[F-3, non-blocking]** grep for any `EventSubscriber` that consumes `DialogTurnCompleted`/`DialogTurnCancelled` and writes a persisted turn record. If none, file a separate change for "runtime turns are never persisted" (partial assistant text lost on reload). Does **not** block this change.
```

