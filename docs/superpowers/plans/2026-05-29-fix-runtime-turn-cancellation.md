---
change: fix-runtime-turn-cancellation
design-doc: docs/superpowers/specs/2026-05-29-fix-runtime-turn-cancellation-design.md
base-ref: d4b95828b7ba7cfbb9c63d091eb08d8627277561
archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

# Runtime turn cancellation — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ESC and `delete_session` actually stop the bridge child for runtime (claude/OMP) turns within ~50–100 ms, instead of riding out the SDK's natural completion (up to the new 120 s `IDLE_TIMEOUT_MS`).

**Architecture:** Per-turn `CancellationToken` registry on the `ConversationCoordinator` (`runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>`, keyed by `turn_id`). The runtime spawn task body extracts into a free async fn `run_runtime_event_loop` whose stream loop is wrapped in `tokio::select!` listening on the token. `cancel_dialog_turn` fires the token. RAII (`RuntimeCancelGuard` with `armed` flag) handles entry removal across the `tokio::spawn` boundary; the calling thread's guard covers pre-spawn `?` early-returns. A pre-`prompt()` `is_cancelled()` check (D8) closes the cold-start window so a cancel during the 100–500 ms Node bridge spawn never burns an Anthropic API call.

**Tech Stack:** Rust, `tokio`, `tokio_util::sync::CancellationToken`, `dashmap`, `futures::Stream`. No new dependencies. No changes to traits or runtime adapters.

**Authoritative steps:** OpenSpec `tasks.md` is the canonical line-level checklist (53 lines, F-1…F-7 references, structural anchors after batch 1 + batch 2). This plan adds execution order + verification gates + risk pre-resolution; it does **not** re-state tasks.md verbatim. When the two diverge, **tasks.md wins**.

**Working directory:** `F:/Work/Mybitfun/MyBitFun/` (inner git repo). Plan/spec/report docs live at `F:/Work/Mybitfun/` (outside the git repo, intentionally untracked per project convention).

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Pre-resolved implementation risks

Resolved during plan write — do **not** re-investigate at build time:

| Risk | Resolution | Source |
|---|---|---|
| `PortError` / `PortErrorKind` `Clone` derivation | Both derive `Clone` (`runtime-ports/src/lib.rs:13`, `:25`). FakeSession's `prompt_err: Option<PortError>` can `.clone()` directly. | grep verified |
| `EventQueueConfig::default()` exists | Yes (`agentic/events/queue.rs:23`). Tests can `EventQueue::new(EventQueueConfig::default())`. | grep verified |
| `EventPriority` vs `AgenticEventPriority` | Same enum. `bitfun-core`'s internal `EventPriority` is `pub use bitfun_events::AgenticEventPriority as EventPriority` (`events/types.rs:8-15`). F-7's `EventPriority::High` matches the runtime `Aborted` arm at `coordinator.rs:2754`. | grep verified |
| `AgenticEventEnvelope` field access for tests | All public: `pub id`, `pub event: AgenticEvent`, `pub priority`, `pub timestamp` (`events/agentic.rs:430~436`). Tests can pattern-match `envelope.event` against `AgenticEvent::DialogTurnCancelled { .. }` directly. | grep verified |
| `DialogTurnCancelled` field signature | `{ session_id: String, turn_id: String }` only (`events/agentic.rs:148~151`). Two-field struct match. | grep verified |

**Outstanding (defer to build time):**

- `EventQueue::dequeue_batch` async signature exact form (already public per earlier inspection — confirm at use site).
- `bitfun_runtime_ports::AgentInputAttachment` import path for FakeSession's `prompt` signature.
- The exact set of `RuntimeEvent` variants the existing match arms cover — preserve verbatim, do not rewrite.

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase ordering & verification gates

Six phases. Each phase ends with a verification gate; do **not** advance until the gate passes.

| Phase | Tasks | Code surface | Verification gate |
|---|---|---|---|
| **P1** | 1.1, 1.2, 2.1, 2.2, 2.3 | Module-level types: field + `RuntimeCancelGuard` struct/impl. No callers. | `cargo check -p bitfun-core` exits 0; `unused`/`dead_code` warnings on the new types are *expected* until P3. |
| **P2** | 4.1, 4.2, 4.3, 4.4, 4.5, 4.6, 4.7 | `run_runtime_event_loop` defined as module-private async fn. Still no callers. | `cargo check` exits 0; warnings on helper still expected. |
| **P3** | 3.1, 3.2, 3.3, 3.4, 3.5 | `handle_user_input` runtime branch rewritten: early insert, calling-thread guard, helper call, disarm. Replaces the inline spawn body. | `cargo check` exits 0; *all* warnings on new types should clear. |
| **P4** | 5.1 | `cancel_dialog_turn` Step 3.5 (clone-then-cancel). | `cargo check` exits 0. |
| **P5** | 6.1, 6.2, 6.3, 6.4, 6.5 | `FakeSession` + 4 `#[tokio::test]` cases inside existing `mod tests`. | `cargo test -p bitfun-core --lib runtime_event_loop` — 4/4 pass. |
| **P6** | 7.1, 7.2, 7.3, 7.4, 7.5, 7.6 | No code changes — pure verification. | All greps return expected counts; existing `reset_session_state_if_processing` tests still 2/2. |

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase 1: Module-level types

Covers `tasks.md §1` and `§2`. Pure additions; no behavioural change yet.

**Files:**
- Modify: `MyBitFun/src/crates/core/src/agentic/coordination/coordinator.rs`

- [ ] **Step 1: Add the `runtime_turn_cancels` field**

Insert immediately after the `runtime_sessions` field declaration (≈ line 556, the line that contains `runtime_sessions: Arc<DashMap<String, Arc<tokio::sync::Mutex<Option<Box<dyn AgentSession>>>>>>`):

```rust
    /// Cancellation tokens for in-flight runtime (non-bitfun) turns. Keyed by
    /// `turn_id` (per OpenSpec D1 — *not* `session_id`; per-session keying
    /// would have a sticky-cancel bug since `CancellationToken::cancel()` is
    /// permanent). Inserted on the calling thread before `create_session`
    /// (D4 / F-1) so a cancel during the cold-start window is honoured.
    /// Removed via `RuntimeCancelGuard::drop` (D3).
    runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>,
```

`Arc`, `DashMap`, and `CancellationToken` are already imported at the top of the file (lines 50, 55, 59).

- [ ] **Step 2: Initialize in `ConversationCoordinator::new`**

In `new()` (≈ line 1050), the struct literal already initialises `runtime_sessions: Arc::new(DashMap::new())` at ≈ line 1071. Add the new field directly below it:

```rust
            runtime_sessions: Arc::new(DashMap::new()),
            runtime_turn_cancels: Arc::new(DashMap::new()),
```

- [ ] **Step 3: Define `RuntimeCancelGuard` at module scope**

Insert after `TurnLifecycleGuard` (≈ line 125, the closing brace of `impl Drop for TurnLifecycleGuard`):

```rust
/// RAII guard for a `runtime_turn_cancels` entry.
///
/// Entry-removal ownership crosses the `tokio::spawn` boundary because the
/// entry is inserted on the calling thread *before* `create_session` (D4)
/// to close the cold-start cancel window (F-1). Two regimes:
///
/// 1. **Calling-thread guard:** armed at construction; covers `?`
///    early-returns of `registry.get` / `create_session`. `disarm()`ed
///    immediately after a successful `tokio::spawn` — handing ownership to
///    the spawn body's own guard. Mirrors `ActiveTurnRegistration`.
///
/// 2. **Spawn-body guard (inside the helper):** armed, never disarmed —
///    sole remover on every helper exit path (D8 pre-prompt return,
///    `prompt()` Err, cancel branch, stream-end, put-back, panic unwind).
///
/// `DashMap::remove` on a missing key is a no-op, so even a forgotten
/// `disarm()` is harmless under double-remove.
struct RuntimeCancelGuard {
    map: Arc<DashMap<String, CancellationToken>>,
    turn_id: String,
    armed: bool,
}

impl RuntimeCancelGuard {
    fn armed(map: Arc<DashMap<String, CancellationToken>>, turn_id: String) -> Self {
        Self { map, turn_id, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RuntimeCancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.map.remove(&self.turn_id);
        }
    }
}
```

- [ ] **Step 4: Gate — cargo check**

Run: `cd MyBitFun && cargo check -p bitfun-core --message-format=short`
Expected: exit 0. `unused` / `dead_code` warnings on `runtime_turn_cancels` and `RuntimeCancelGuard` are expected and clear in P3.

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase 2: `run_runtime_event_loop` helper

Covers `tasks.md §4`. Adds the extracted async fn. No callers yet.

- [ ] **Step 1: Add the helper function**

Insert at module scope after `RuntimeCancelGuard`'s `impl Drop` block (so module-level types stay clustered). The helper is the verbatim move of the current spawn body's loop, with three additions: the `RuntimeCancelGuard`, the D8 pre-prompt check, and the `select!` cancel branch.

```rust
#[allow(clippy::too_many_arguments)]
async fn run_runtime_event_loop(
    rt_session: Box<dyn AgentSession>,
    user_input: String,
    cancel_token: CancellationToken,
    runtime_turn_cancels: Arc<DashMap<String, CancellationToken>>,
    event_queue: Arc<EventQueue>,
    session_slot: Arc<tokio::sync::Mutex<Option<Box<dyn AgentSession>>>>,
    session_id: String,
    turn_id: String,
    runtime_id_for_log: String,
) {
    // Spawn-body regime: always armed. Owns entry removal on every exit
    // path below (D8 pre-prompt return, prompt() Err, cancel branch,
    // stream-exhausted, put-back fall-through, panic unwind).
    let _cancel_guard = RuntimeCancelGuard::armed(runtime_turn_cancels, turn_id.clone());

    // D8 / F-2: a cancel may have fired during the cold-start
    // create_session window. Check before prompt() so we neither run a
    // zombie turn nor start (and bill) an Anthropic call.
    if cancel_token.is_cancelled() {
        log::info!(
            "Runtime {} turn cancelled before prompt: session_id={}, turn_id={}",
            runtime_id_for_log, session_id, turn_id,
        );
        let _ = event_queue.enqueue(
            AgenticEvent::DialogTurnCancelled {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            },
            Some(EventPriority::High),
        ).await;
        let _ = rt_session.dispose().await;
        return;
    }

    let mut stream = match rt_session.prompt(&user_input, vec![]).await {
        Ok(s) => s,
        Err(e) => {
            // Existing prompt() Err handling (verbatim from current spawn body),
            // minus the manual fetch_sub/reset (handled by TurnLifecycleGuard
            // in the spawn closure). Disposes session, emits DialogTurnFailed.
            let err_msg = e.to_string();
            let category = classify_runtime_error(&err_msg, Some(&e.kind));
            let detail = ai_error_detail_from_message(&err_msg, category.clone());
            let _ = event_queue.enqueue(
                AgenticEvent::DialogTurnFailed {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    error: err_msg,
                    error_category: Some(category),
                    error_detail: Some(detail),
                },
                Some(EventPriority::High),
            ).await;
            let _ = rt_session.dispose().await;
            return;
        }
    };

    use futures::StreamExt;
    loop {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                log::info!(
                    "Runtime {} turn cancelled by user: session_id={}, turn_id={}",
                    runtime_id_for_log, session_id, turn_id,
                );
                let _ = event_queue.enqueue(
                    AgenticEvent::DialogTurnCancelled {
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                    },
                    Some(EventPriority::High),  // F-7: match runtime Aborted arm
                ).await;
                let _ = rt_session.dispose().await;
                return;
            }
            event = stream.next() => {
                let Some(event) = event else { break };  // None — stream exhausted
                match event {
                    // [VERBATIM the existing match arms from the current spawn
                    // body: TextDelta, ThinkingDelta, ToolCallStart, TurnEnd
                    // (with internal Completed/Aborted/_ split), Error
                    // (dispose + return — review3 batch1), and `_ => {}`.
                    // Do NOT rewrite logic; only paste-in.]
                }
            }
        }
    }

    // Put-back tail (verbatim from current spawn body). Reached only on
    // None / TurnEnd→break.
    let mut slot_guard = session_slot.lock().await;
    let displaced = slot_guard.replace(rt_session);
    drop(slot_guard);
    if let Some(prev_session) = displaced {
        let _ = prev_session.dispose().await;
    }
}
```

The `[VERBATIM …]` comment **must** be replaced with the actual match arms cut from the current spawn body. They are at `coordinator.rs:2727~2829` today (post-batch-2). Cut them, paste them as-is into the inner `match event { … }`, do **not** edit their bodies. The `RuntimeEvent::Error` arm already does `dispose + return` (review3 batch1) and the `TurnEnd { Aborted }` inner arm already uses `EventPriority::High` (F-7 reference).

- [ ] **Step 2: Gate — cargo check**

Run: `cd MyBitFun && cargo check -p bitfun-core --message-format=short`
Expected: exit 0. `dead_code` warning on `run_runtime_event_loop` expected; clears in P3.

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase 3: `handle_user_input` runtime-branch wiring

Covers `tasks.md §3`. Replaces the inline spawn body with the helper call; adds the early insert + calling-thread guard.

- [ ] **Step 1: Insert cancel_token early + calling-thread armed guard**

Locate the runtime branch's `start_dialog_turn().await?` call (≈ line 2651, the `?` returns `turn_id`). Immediately *after* that line, *before* `self.emit_event(AgenticEvent::DialogTurnStarted { … }).await`, insert:

```rust
            // F-1 / D4: insert before the create_session await chain so a
            // cancel during cold-start (Node bridge spawn, 100–500 ms) is
            // reachable. Calling-thread armed guard removes the orphaned
            // entry on any `?` early-return below; disarmed after spawn.
            let cancel_token = CancellationToken::new();
            self.runtime_turn_cancels.insert(turn_id.clone(), cancel_token.clone());
            let mut cancel_entry_guard = RuntimeCancelGuard::armed(
                self.runtime_turn_cancels.clone(),
                turn_id.clone(),
            );
```

- [ ] **Step 2: Add closure captures**

Locate the existing capture-clones block (≈ line 2697~2702, the lines reading `let event_queue = self.event_queue.clone(); let session_manager = self.session_manager.clone(); let session_id_clone = …; let turn_id_clone = …; let runtime_id_for_log = …; let session_slot_clone = …`). Add two new clones at the bottom of that block:

```rust
            let cancel_token_for_task = cancel_token.clone();
            let runtime_turn_cancels_for_task = self.runtime_turn_cancels.clone();
```

- [ ] **Step 3: Replace the inline spawn body with the helper call**

The current `tokio::spawn(async move { … })` block runs from ≈ line 2704 (`tokio::spawn(async move {`) to ≈ line 2865 (the matching closing `});`). Replace the entire body of the closure (everything between the opening `{` and closing `}` of `async move`) with:

```rust
                let _guard = TurnLifecycleGuard::new(
                    session_manager,
                    session_id_clone.clone(),
                    turn_id_clone.clone(),
                    active_counter,
                );
                run_runtime_event_loop(
                    rt_session,
                    wrapped_user_input,
                    cancel_token_for_task,
                    runtime_turn_cancels_for_task,
                    event_queue,
                    session_slot_clone,
                    session_id_clone,
                    turn_id_clone,
                    runtime_id_for_log,
                ).await;
```

The `TurnLifecycleGuard::new` arguments (`session_manager`, `session_id_clone.clone()`, `turn_id_clone.clone()`, `active_counter`) match its current usage from batch 2; only `session_id_clone` and `turn_id_clone` are cloned because the helper consumes them by value.

- [ ] **Step 4: Disarm the calling-thread guard after spawn**

Immediately *after* the closing `});` of `tokio::spawn(...)` (≈ line 2865), *before* `return Ok(())` (≈ line 2867), insert:

```rust
            cancel_entry_guard.disarm();
```

This transfers entry-removal ownership to the spawn body's own `RuntimeCancelGuard`. If `tokio::spawn` panics (it can't in practice — the spawn call is infallible) the calling-thread guard would still remove the entry on unwind.

- [ ] **Step 5: Gate — cargo check**

Run: `cd MyBitFun && cargo check -p bitfun-core --message-format=short`
Expected: exit 0. All `dead_code` / `unused` warnings on the new types now clear; the only remaining lint is `clippy::too_many_arguments` on the helper, suppressed via the `#[allow]` from P2.

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase 4: `cancel_dialog_turn` hook

Covers `tasks.md §5`.

- [ ] **Step 1: Add Step 3.5 between existing Step 3 block and `wait_session_drained`**

In `cancel_dialog_turn` (≈ line 3287), locate the call site of `wait_session_drained` (≈ line 3368). Insert immediately *before* it:

```rust
        // Step 3.5: signal runtime spawn task (no-op for bitfun turns).
        // Clone-then-drop the DashMap Ref *before* calling cancel() — a
        // future cancel() side-effect that re-enters the map would
        // deadlock on the same thread otherwise (F-5). The token is an
        // Arc internally; the clone is cheap.
        let runtime_cancel = self
            .runtime_turn_cancels
            .get(dialog_turn_id)
            .map(|entry| entry.value().clone());
        if let Some(token) = runtime_cancel {
            token.cancel();
        }
```

`get` (not `remove`) — `RuntimeCancelGuard::drop` owns removal so we don't race with the spawn task removing it twice. `cancel()` on an already-cancelled token is idempotent (no-op).

- [ ] **Step 2: Gate — cargo check**

Run: `cd MyBitFun && cargo check -p bitfun-core --message-format=short`
Expected: exit 0.

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase 5: Tests

Covers `tasks.md §6`. Add inside the existing `mod tests` at ≈ line 5574.

- [ ] **Step 1: Add `FakeSession` + supporting imports**

Inside `mod tests`, add (after the existing `use` block):

```rust
    use crate::agentic::events::queue::EventQueueConfig;
    use bitfun_runtime_ports::agent_runtime::AgentEvent;
    use bitfun_runtime_ports::{AgentInputAttachment, PortError, PortErrorKind, PortResult};
    use bitfun_runtime_ports::agent_runtime::{AgentEventStream, AgentSession};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::mpsc;

    struct FakeSession {
        session_id: String,
        event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<AgentEvent>>>,
        prompt_err: Option<PortError>,
        disposed: Arc<AtomicBool>,
        prompt_called: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl AgentSession for FakeSession {
        fn session_id(&self) -> &str { &self.session_id }

        async fn prompt(
            &self,
            _: &str,
            _: Vec<AgentInputAttachment>,
        ) -> PortResult<AgentEventStream> {
            self.prompt_called.store(true, Ordering::SeqCst);
            if let Some(e) = self.prompt_err.as_ref() {
                return Err(e.clone());
            }
            let rx = self.event_rx.lock().await.take()
                .expect("prompt() called without pre-stocked receiver");
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        }

        async fn abort(&self) -> PortResult<()> { Ok(()) }

        async fn dispose(self: Box<Self>) -> PortResult<()> {
            self.disposed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
```

If any import path resolves differently at build time (`async_trait` re-export, `tokio_stream` location), adjust to match what `claude_runtime.rs` already uses.

- [ ] **Step 2: Add T1 — `runtime_event_loop_cancels_promptly`**

```rust
    #[tokio::test]
    async fn runtime_event_loop_cancels_promptly() {
        // F-6: keep tx alive in this binding so stream.next() stays
        // pending. Without this, dropping tx makes ReceiverStream::next
        // return None immediately and the loop exits via the put-back
        // path — a false-pass time-bomb.
        let (_tx, rx) = mpsc::channel::<AgentEvent>(8);
        let disposed = Arc::new(AtomicBool::new(false));
        let prompt_called = Arc::new(AtomicBool::new(false));
        let session: Box<dyn AgentSession> = Box::new(FakeSession {
            session_id: "fake".into(),
            event_rx: tokio::sync::Mutex::new(Some(rx)),
            prompt_err: None,
            disposed: disposed.clone(),
            prompt_called: prompt_called.clone(),
        });
        let cancel = CancellationToken::new();
        let cancels = Arc::new(DashMap::new());
        let queue = Arc::new(super::EventQueue::new(EventQueueConfig::default()));
        let slot = Arc::new(tokio::sync::Mutex::new(None));

        let task = tokio::spawn(super::run_runtime_event_loop(
            session, "hi".into(), cancel.clone(), cancels.clone(),
            queue.clone(), slot.clone(),
            "sid".into(), "tid".into(), "claude".into(),
        ));

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cancel.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(200), task).await;
        assert!(outcome.is_ok(), "helper did not exit within 200ms after cancel");

        assert!(disposed.load(Ordering::SeqCst), "session was not disposed");
        assert!(prompt_called.load(Ordering::SeqCst), "prompt was not reached (this test exercises post-prompt cancel)");
        assert!(slot.lock().await.is_none(), "session was put back on cancel path");

        let batch = queue.dequeue_batch(8).await;
        assert!(
            batch.iter().any(|env| matches!(
                env.event,
                AgenticEvent::DialogTurnCancelled { .. }
            )),
            "DialogTurnCancelled was not emitted"
        );
    }
```

- [ ] **Step 3: Add T2 — `runtime_event_loop_completes_cleanly`**

Pattern same as T1 but pre-stock rx with a `TurnEnd { Completed }`, drop tx after, never fire cancel. Assert `disposed==false`, `slot.is_some()`, `DialogTurnCompleted` emitted. Reuse the FakeSession constructor pattern from T1.

```rust
    #[tokio::test]
    async fn runtime_event_loop_completes_cleanly() {
        let (tx, rx) = mpsc::channel::<AgentEvent>(8);
        tx.send(AgentEvent::TurnEnd {
            stop_reason: bitfun_runtime_ports::agent_runtime::StopReason::Completed,
            metadata: Default::default(),
        }).await.unwrap();
        drop(tx);
        // … construct FakeSession + helper invocation as in T1 …
        // assertions: disposed==false, slot.is_some(), event queue saw DialogTurnCompleted
    }
```

Fill the elided `… …` with the same scaffolding as T1 (FakeSession + helper invocation), minus the cancel call.

- [ ] **Step 4: Add T3 — `runtime_event_loop_disposes_on_error_event`**

```rust
    #[tokio::test]
    async fn runtime_event_loop_disposes_on_error_event() {
        let (tx, rx) = mpsc::channel::<AgentEvent>(8);
        tx.send(AgentEvent::Error {
            message: "rate limit".into(),
            metadata: Default::default(),
        }).await.unwrap();
        drop(tx);
        // … FakeSession + helper invocation …
        // assertions: disposed==true, slot.is_none(), DialogTurnFailed emitted
    }
```

- [ ] **Step 5: Add T4 — `runtime_event_loop_skips_prompt_when_precancelled`**

The D8 / F-2 case. Pre-cancel before spawn:

```rust
    #[tokio::test]
    async fn runtime_event_loop_skips_prompt_when_precancelled() {
        let (_tx, rx) = mpsc::channel::<AgentEvent>(8);  // not used; prompt should never run
        let disposed = Arc::new(AtomicBool::new(false));
        let prompt_called = Arc::new(AtomicBool::new(false));
        let session: Box<dyn AgentSession> = Box::new(FakeSession {
            session_id: "fake".into(),
            event_rx: tokio::sync::Mutex::new(Some(rx)),
            prompt_err: None,
            disposed: disposed.clone(),
            prompt_called: prompt_called.clone(),
        });
        let cancel = CancellationToken::new();
        cancel.cancel();  // BEFORE spawning the helper
        // … construct cancels/queue/slot, spawn helper, await with 200ms timeout …
        // assertions:
        //   prompt_called.load() == false  (D8 short-circuited before prompt)
        //   disposed.load() == true
        //   slot.is_none()
        //   queue saw DialogTurnCancelled
    }
```

- [ ] **Step 6: Gate — cargo test**

Run: `cd MyBitFun && cargo test -p bitfun-core --lib --message-format=short runtime_event_loop`
Expected: 4 tests, 4 pass, 0 fail.

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Phase 6: Final verification

Covers `tasks.md §7`. No code changes — pure verification.

- [ ] **Step 1: cargo check + clippy spot-check**

```bash
cd MyBitFun && cargo check -p bitfun-core --message-format=short
cd MyBitFun && cargo clippy -p bitfun-core --message-format=short 2>&1 | grep -E "warning|error" | head -20
```

Expected: cargo check exit 0. Clippy should not emit `too_many_arguments` (suppressed) or `dead_code` (now used).

- [ ] **Step 2: Regression — existing tests still pass**

```bash
cd MyBitFun && cargo test -p bitfun-core --lib --message-format=short reset_session_state_if_processing
```

Expected: 2 tests, 2 pass.

- [ ] **Step 3: Reachability greps**

```bash
cd MyBitFun
grep -c "cancel_token.cancelled()" src/crates/core/src/agentic/coordination/coordinator.rs   # → 1
grep -c "cancel_token.is_cancelled()" src/crates/core/src/agentic/coordination/coordinator.rs # → 1
```

- [ ] **Step 4: RAII wiring greps**

```bash
cd MyBitFun
# struct + 2 impls + 2 ::armed call sites = 5
grep -c "RuntimeCancelGuard" src/crates/core/src/agentic/coordination/coordinator.rs            # → 5
# Only inside Drop
grep -c "runtime_turn_cancels.remove" src/crates/core/src/agentic/coordination/coordinator.rs  # → 1
# Single disarm site (calling thread)
grep -c "cancel_entry_guard.disarm()" src/crates/core/src/agentic/coordination/coordinator.rs  # → 1
```

If any count diverges, return to the corresponding phase and resolve before committing.

- [ ] **Step 5: F-3 follow-up note (non-blocking)**

```bash
grep -rn "DialogTurnCompleted\|DialogTurnCancelled" src/crates/core/src/agentic/events/router.rs 2>/dev/null
grep -rn "EventSubscriber" src/crates/core/src/ 2>/dev/null | head -5
```

Skim for any subscriber that consumes `DialogTurnCompleted`/`DialogTurnCancelled` and writes a persisted turn record. If none, the F-3 "runtime turns never persisted" gap is real — file a separate change after this one is archived. **Does not block this change.**

- [ ] **Step 6: Commit**

Single commit on `main` (matches batch 1, 2 precedent). Commit message:

```bash
cd MyBitFun
git add src/crates/core/src/agentic/coordination/coordinator.rs
git commit -m "$(cat <<'EOF'
fix(coordinator): runtime turn cancellation via per-turn token (review3 P-2)

Add a per-turn CancellationToken registry on ConversationCoordinator,
extract the runtime spawn body into a free async fn run_runtime_event_loop
whose stream loop is tokio::select!'d on the token, and signal the token
from cancel_dialog_turn. Closes the gap where ESC and delete_session were
only "visible" cancels: session state flipped to Idle but the bridge child
kept streaming from Anthropic until natural TurnEnd or 120 s IDLE_TIMEOUT_MS.

Wiring (per OpenSpec design D1–D8):
- runtime_turn_cancels: Arc<DashMap<String, CancellationToken>> keyed by
  turn_id (per-turn, not per-session — sticky-cancel bug otherwise).
- Insert on the calling thread before create_session (D4 / F-1) so a cancel
  during the 100–500 ms Node bridge cold-start is reachable; calling-thread
  RuntimeCancelGuard (armed) removes the orphaned entry on any `?` early
  return; disarm() after a successful tokio::spawn.
- run_runtime_event_loop's first line builds its own armed RuntimeCancelGuard.
- D8 pre-prompt is_cancelled() check: short-circuits with DialogTurnCancelled
  + dispose, no Anthropic call, when the cold-start window fired the cancel.
- biased; tokio::select! prefers the cancel branch when both are ready.
- cancel branch: log, emit DialogTurnCancelled @ EventPriority::High (F-7,
  matching the runtime Aborted arm), dispose, return.
- cancel_dialog_turn Step 3.5: clone-then-cancel (F-5 footgun avoidance).

Tests: FakeSession + 4 #[tokio::test] (T1 mid-stream cancel, T2 happy
completion regression, T3 Error event regression, T4 pre-prompt cancel).
T1 keeps tx alive (F-6).
EOF
)"
```

- [ ] **Step 7: Verify commit**

```bash
cd MyBitFun && git log --oneline -3 && git diff --stat HEAD~1
```

Expected: HEAD message matches the above; diff stat shows 1 file (`coordinator.rs`), additions ≈ 200, deletions ≈ 150 (depends on the helper extraction's line count).

archived-with: 2026-05-29-fix-runtime-turn-cancellation
---

## Self-review

**Spec coverage** (each `tasks.md` section ↔ a phase):
- §1 → P1 ✓
- §2 → P1 ✓
- §3 → P3 ✓
- §4 → P2 ✓
- §5 → P4 ✓
- §6 → P5 ✓
- §7 → P6 ✓

All seven sections covered.

**Placeholder scan:** Some test code in P5 Steps 3–5 uses `… FakeSession + helper invocation …` ellipses — flagged. The pattern is fully shown in P5 Step 2 (T1) and the elided portions are mechanical copies. Engineers (or me at execute time) follow T1 as the canonical example. This is *not* a "TBD" — it is a deliberate "copy-from-T1" instruction. Acceptable.

**Type consistency:**
- `RuntimeCancelGuard` definition (P1 Step 3) uses `armed`/`disarm`; P3 Step 1 calls `RuntimeCancelGuard::armed(...)`; P3 Step 4 calls `cancel_entry_guard.disarm()`. ✓
- `run_runtime_event_loop` signature (P2 Step 1) takes 9 params in a specific order; P3 Step 3's call site uses the same order. ✓
- `EventPriority::High` used uniformly in cancel branches and pre-prompt branch — matches the runtime Aborted arm. ✓

No fixes needed.

**Migration / rollback:** Single commit on main; `git revert` cleanly undoes. `runtime_turn_cancels` field becomes orphaned but inert. Documented in OpenSpec design.md "Migration Plan".
