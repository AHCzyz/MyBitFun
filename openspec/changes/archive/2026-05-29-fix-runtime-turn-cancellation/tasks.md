> Coordinates verified against `coordinator.rs` after review3 batch1/batch2.
> Build phase: treat line numbers as "around" — the structural anchors
> (function names, existing fields) are authoritative.

## 1. Coordinator state

- [x] 1.1 Add field `runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>` to `ConversationCoordinator`, immediately after `runtime_sessions` (`coordinator.rs:556`). Key = `turn_id` (per OpenSpec design D1 — **not** session_id; sticky-cancel bug).
- [x] 1.2 Initialize `runtime_turn_cancels: Arc::new(DashMap::new())` in `ConversationCoordinator::new`, after the `runtime_sessions` init (`coordinator.rs:1071`).

## 2. `RuntimeCancelGuard` (module scope, **armed**)

- [x] 2.1 Define `struct RuntimeCancelGuard { map: Arc<DashMap<String, CancellationToken>>, turn_id: String, armed: bool }` at module scope, next to `TurnLifecycleGuard` (after `classify_runtime_error`, around `coordinator.rs:97~127`).
- [x] 2.2 Impl `RuntimeCancelGuard::armed(map, turn_id) -> Self` (sets `armed: true`) and `fn disarm(&mut self) { self.armed = false; }`.
- [x] 2.3 Impl `Drop`: `if self.armed { self.map.remove(&self.turn_id); }`. (`DashMap::remove` on a missing key is a no-op — idempotent.) Rationale for `armed`: entry-removal ownership crosses the `tokio::spawn` boundary because the entry is inserted on the calling thread before `create_session` (task 3.1 / OpenSpec D3+D4). Mirrors `ActiveTurnRegistration`'s `armed`/`disarm` at `coordinator.rs:2894~2910`.

## 3. `handle_user_input` runtime-branch wiring (early insert + calling-thread guard + disarm)

- [x] 3.1 **Right after** `start_dialog_turn` returns in the runtime branch (`coordinator.rs:2651`), **before** the `DialogTurnStarted` emit / `registry.get` / `create_session`: construct `let cancel_token = CancellationToken::new();`, `self.runtime_turn_cancels.insert(turn_id.clone(), cancel_token.clone());`, and `let mut cancel_entry_guard = RuntimeCancelGuard::armed(self.runtime_turn_cancels.clone(), turn_id.clone());`. (F-1: the entry must be reachable the instant `current_turn_id` is visible, so a cancel during the cold-start `create_session` window is honoured, not dropped.)
- [x] 3.2 The calling-thread `cancel_entry_guard` covers the `?` early-returns at `registry.get` (`:2668~2670`) and `create_session` (`:2682~2686`): if either bails, `Drop` removes the orphaned entry. No code change at those `?` sites — the guard does it.
- [x] 3.3 Add `cancel_token` and a `runtime_turn_cancels` Arc clone to the spawn closure capture list (alongside the existing clones at `:2697~2702`).
- [x] 3.4 Replace the inline spawn body (`:2717~2865`, from `let mut stream = match rt_session.prompt(...)` through the put-back tail) with a call to the extracted helper: keep `TurnLifecycleGuard::new(...)` in the **closure**, then `run_runtime_event_loop(rt_session, wrapped_user_input, cancel_token, runtime_turn_cancels, event_queue, session_slot_clone, session_id_clone, turn_id_clone, runtime_id_for_log).await;`.
- [x] 3.5 **After** `tokio::spawn(...)` returns (before `return Ok(())` at `:2869`), call `cancel_entry_guard.disarm();` — spawn succeeded, the spawn-body's own `RuntimeCancelGuard` now owns entry removal. (If we did not disarm, both guards would try to remove — harmless via idempotency, but disarm makes the common path exactly one remove from the spawn body.)

## 4. `run_runtime_event_loop` helper (extracted, testable)

- [x] 4.1 Define module-private `async fn run_runtime_event_loop(rt_session: Box<dyn AgentSession>, user_input: String, cancel_token: CancellationToken, runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>, event_queue: Arc<EventQueue>, session_slot: Arc<tokio::sync::Mutex<Option<Box<dyn AgentSession>>>>, session_id: String, turn_id: String, runtime_id_for_log: String)`. Add `#[allow(clippy::too_many_arguments)]` (9 params; wrapper struct is future cleanup, YAGNI now).
- [x] 4.2 First line: `let _cancel_guard = RuntimeCancelGuard::armed(runtime_turn_cancels, turn_id.clone());` (spawn-body regime — always armed; every exit path must remove).
- [x] 4.3 **Pre-prompt cancel check (F-2 / D8)**, before `prompt()`: `if cancel_token.is_cancelled() { emit DialogTurnCancelled{session_id, turn_id} @ High; let _ = rt_session.dispose().await; return; }`. Closes the cancelled-during-create_session path with **no Anthropic API call**.
- [x] 4.4 Move the existing `prompt()` `Ok/Err` match in verbatim (the `Err` arm already emits `DialogTurnFailed` + disposes + returns — review3 batch1; unchanged).
- [x] 4.5 Replace `while let Some(event) = stream.next().await { match event { … } }` with `loop { tokio::select! { biased; _ = cancel_token.cancelled() => { /* 4.6 */ } event = stream.next() => { match event { Some(ev) => match ev { /* existing arms verbatim */ }, None => break } } } }`. The existing `RuntimeEvent::Error` arm keeps its dispose+return (review3 batch1); `TurnEnd` keeps `break`.
- [x] 4.6 Cancel branch body: `log::info!("Runtime {} turn cancelled by user: session_id={}, turn_id={}", runtime_id_for_log, session_id, turn_id);` then emit `DialogTurnCancelled{session_id, turn_id}` @ `EventPriority::High` (F-7: match the runtime `Aborted` arm at `:2805`, not bitfun `Critical`); `let _ = rt_session.dispose().await;`; `return;`.
- [x] 4.7 Keep the put-back tail verbatim below the loop (`slot.replace(rt_session)` + dispose displaced). Reached only on the `None`/`TurnEnd→break` happy path.

## 5. `cancel_dialog_turn` integration (clone-then-cancel)

- [x] 5.1 In `cancel_dialog_turn`, between the existing Step 3 block and the `wait_session_drained(1500ms)` call (`coordinator.rs:3368`), add Step 3.5: `let runtime_cancel = self.runtime_turn_cancels.get(dialog_turn_id).map(|e| e.value().clone()); if let Some(token) = runtime_cancel { token.cancel(); }`. Clone-then-drop-Ref **before** `cancel()` (F-5/D5 footgun avoidance — don't hold the DashMap shard read-guard across `cancel()`). Comment: no-op for bitfun turns (no entry); `get` not `remove` so `RuntimeCancelGuard::drop` owns removal.

## 6. Tests (in existing `mod tests`, `coordinator.rs:5574`)

- [x] 6.1 Add `#[cfg(test)] struct FakeSession { session_id, event_rx: Mutex<Option<Receiver<AgentEvent>>>, prompt_err: Option<PortError>, disposed: Arc<AtomicBool> }` implementing `AgentSession` (`session_id`/`prompt`/`abort`/`dispose`; `steer` uses the trait default). `dispose` sets `disposed=true`.
- [x] 6.2 **T1 `runtime_event_loop_cancels_promptly`**: pre-stock an empty rx **and keep the `Sender` alive in a local binding** (F-6: if the Sender drops, `stream.next()` yields `None` immediately and the loop exits via the put-back/completion path — testing the wrong branch). Spawn helper, `sleep(10ms)`, `cancel.cancel()`, `timeout(200ms, task)` must complete. Assert `disposed==true`, `dequeue_batch` saw `DialogTurnCancelled`, `slot.is_none()`.
- [x] 6.3 **T2 `runtime_event_loop_completes_cleanly`**: pre-stock `[TurnEnd{Completed}]`, drop the Sender after. Run to completion, never fire cancel. Assert `disposed==false`, `slot.is_some()`, event queue saw `DialogTurnCompleted`.
- [x] 6.4 **T3 `runtime_event_loop_disposes_on_error_event`**: pre-stock `[Error{message:"rate limit"}]`. Run. Assert `disposed==true`, `slot.is_none()`, event queue saw `DialogTurnFailed`. (Regression guard for review3 batch1 P-5.)
- [x] 6.5 Add a **T4 `runtime_event_loop_skips_prompt_when_precancelled`** (F-2/D8 coverage): construct `FakeSession` whose `prompt` would `panic!`/set a "prompt_called" flag; pre-cancel the token before spawning the helper. Assert the helper emitted `DialogTurnCancelled`, `disposed==true`, and `prompt` was **never** called. (This is the one genuinely-new behaviour the change adds; worth its own case rather than folding into T1.)

## 7. Verification

- [x] 7.1 `cd MyBitFun && cargo check -p bitfun-core --message-format=short` exits 0 (watch for the `clippy::too_many_arguments` allow being needed; not a `check` failure but confirm under `cargo clippy`).
- [x] 7.2 `cd MyBitFun && cargo test -p bitfun-core --lib reset_session_state_if_processing` — both pre-existing turn-scoped tests still pass (the `TurnLifecycleGuard` drop relies on turn-scoped reset).
- [x] 7.3 `cd MyBitFun && cargo test -p bitfun-core --lib runtime_event_loop` — T1–T4 pass.
- [x] 7.4 Reachability greps: `cancel_token.cancelled()` (1 hit, in helper) and `cancel_token.is_cancelled()` (1 hit, pre-prompt check).
- [x] 7.5 RAII wiring greps: `RuntimeCancelGuard` = exactly **5** hits (struct def + `impl` block + `impl Drop` + `::armed(` calling-thread instance + `::armed(` helper instance); `runtime_turn_cancels.remove` = **1** hit (only inside the guard's `Drop`); `cancel_entry_guard.disarm()` = **1** hit.
- [x] 7.6 **[F-3, non-blocking]** grep for any `EventSubscriber` that consumes `DialogTurnCompleted`/`DialogTurnCancelled` and writes a persisted turn record. If none, file a separate change for "runtime turns are never persisted" (partial assistant text lost on reload). Does **not** block this change.
