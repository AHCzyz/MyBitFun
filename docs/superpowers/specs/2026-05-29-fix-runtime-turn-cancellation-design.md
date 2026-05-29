---
comet_change: fix-runtime-turn-cancellation
role: technical-design
canonical_spec: openspec
archived-with: 2026-05-29-fix-runtime-turn-cancellation
status: final
---

# Runtime turn cancellation — technical design

## Scope

Code-level RFC for the change specified in
`openspec/changes/fix-runtime-turn-cancellation/`. Architectural decisions
(D1–D7: per-turn keying, `tokio::select!` placement, RAII guard split,
calling-thread insert, `cancel_dialog_turn` hook, P-6 race out-of-scope,
dispose-no-putback) already live in that change's `design.md` — not
duplicated here. This document fixes the **implementation surface** so the
build phase has nothing left to invent: function shape, test infrastructure,
borrow-check reasoning, and edge cases beyond the canonical decisions.

## Module shape after the change

`coordinator.rs` gains:

```
ConversationCoordinator { … runtime_turn_cancels: Arc<DashMap<String, CancellationToken>> }
ConversationCoordinator::new(…)               // adds 1 line for new field init
ConversationCoordinator::handle_user_input    // runtime branch — early insert (after
                                              //   start_dialog_turn, before create_session),
                                              //   calling-thread armed guard, spawn helper,
                                              //   disarm after spawn
ConversationCoordinator::cancel_dialog_turn   // adds Step 3.5 (clone-then-cancel, ~5 lines)

[module-private]
struct RuntimeCancelGuard { map, turn_id, armed }   // RAII removal; armed for cross-spawn ownership
async fn run_runtime_event_loop(…)                  // extracted spawn-body content (testable)
```

The runtime spawn body in `handle_user_input` shrinks from ~165 lines (lines
2717–2865 today) to ~25 lines: build cancel token + **insert it before the
`create_session` await chain** (OpenSpec D4 / finding F-1), build a
calling-thread `RuntimeCancelGuard::armed(...)` to cover the pre-spawn `?`
paths, build the `TurnLifecycleGuard`, spawn a task that calls
`run_runtime_event_loop(…).await`, then `cancel_entry_guard.disarm()` after
`tokio::spawn` returns (ownership transfers to the spawn body). The body of
the loop — the D8 pre-prompt check, `prompt()`, the `select!` block, all
`match` arms, and the put-back tail — moves into the helper.

**Two `RuntimeCancelGuard` instances, one entry:**
- *Calling thread:* `RuntimeCancelGuard::armed(...)`, disarmed right after a
  successful `tokio::spawn`. Only fires if `registry.get` / `create_session`
  bail via `?` before the spawn — then it removes the orphaned entry.
- *Spawn body (in the helper):* `RuntimeCancelGuard::armed(...)`, never
  disarmed — owns removal on every helper exit path. `DashMap::remove` is
  idempotent, so the (disarmed) calling-thread guard and this one can never
  double-remove harmfully even under a logic slip.

## `run_runtime_event_loop` — signature & rationale

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
    // Spawn-body regime: always armed, never disarmed — owns entry removal
    // on every exit path below (pre-prompt return, prompt() Err, cancel
    // branch, stream-exhausted, put-back fall-through, panic unwind).
    let _cancel_guard = RuntimeCancelGuard::armed(runtime_turn_cancels, turn_id.clone());

    // D8 / finding F-2: a cancel may have fired during the cold-start
    // create_session window — i.e. the token is already cancelled before we
    // ever reach select!. Check before prompt() so we neither run a zombie
    // turn nor start (and bill) an Anthropic call.
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
            // emit DialogTurnFailed, dispose, return — existing behaviour
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
                    Some(EventPriority::High),  // F-7: match runtime Aborted arm, not bitfun Critical
                ).await;
                let _ = rt_session.dispose().await;
                return;
            }
            event = stream.next() => {
                match event {
                    Some(RuntimeEvent::TextDelta { … }) => { … },
                    /* all existing match arms — unchanged */
                    Some(RuntimeEvent::Error { … }) => {
                        /* dispose + return — unchanged from review3 batch1 */
                        return;
                    }
                    Some(RuntimeEvent::TurnEnd { … }) => break,
                    Some(_) => {}
                    None => break,
                }
            }
        }
    }

    // put-back path — unchanged from current spawn body
    let mut slot_guard = session_slot.lock().await;
    let displaced = slot_guard.replace(rt_session);
    drop(slot_guard);
    if let Some(prev) = displaced {
        let _ = prev.dispose().await;
    }
}
```

**Why a free `async fn`, not a method?** The helper has no `&self` use —
its inputs are exactly what the current spawn body's closure captures.
Making it a method would require a `&Self` borrow that would have to
live across awaits inside the spawn task. A free function is simpler and
keeps the call site unchanged (`tokio::spawn(async move { run_runtime_event_loop(…).await })`).

## Calling-thread wiring in `handle_user_input` (the other half)

The helper above is the spawn-body half. The calling-thread half — the
F-1 early insert, the `armed` calling-thread guard, and the post-spawn
`disarm()` — is **mandatory and equally authoritative**; a build that only
copies the helper and re-opens the original insert site will re-introduce
the cold-start cancel bug. In the runtime branch of `handle_user_input`,
immediately after `start_dialog_turn` returns (`coordinator.rs:2651`):

```rust
let turn_id = self.session_manager.start_dialog_turn(...).await?;

// F-1: reachable before the create_session await chain (cold-start Node spawn),
// so a cancel during that window is honoured, not silently dropped.
let cancel_token = CancellationToken::new();
self.runtime_turn_cancels.insert(turn_id.clone(), cancel_token.clone());
let mut cancel_entry_guard =
    RuntimeCancelGuard::armed(self.runtime_turn_cancels.clone(), turn_id.clone());

self.emit_event(AgenticEvent::DialogTurnStarted { ... }).await;
let runtime = registry.get(runtime_id).cloned().ok_or_else(...)?;   // ? → guard removes entry
let rt_session = { /* slot take-or-create */ ...create_session(...).await...? };  // ? → guard removes entry
let active_counter = ...; active_counter.fetch_add(1, Ordering::SeqCst);

let runtime_turn_cancels = self.runtime_turn_cancels.clone();
// ... existing clones (event_queue, session_slot_clone, ids, runtime_id_for_log) ...
tokio::spawn(async move {
    let _guard = TurnLifecycleGuard::new(             // closure scope — owns counter + state
        session_manager, session_id_clone, turn_id_clone.clone(), active_counter,
    );
    run_runtime_event_loop(
        rt_session, wrapped_user_input, cancel_token, runtime_turn_cancels,
        event_queue, session_slot_clone, session_id_clone2, turn_id_clone, runtime_id_for_log,
    ).await;
});
cancel_entry_guard.disarm();  // spawn succeeded → helper's own guard now owns removal
return Ok(());
```

Note `cancel_entry_guard` stays on the calling thread (it is **not** moved
into the closure). It is disarmed after `tokio::spawn` returns and drops
as a no-op at `return`. The `?` operators between the insert and the spawn
are the only reason it is armed — they are the early-exit paths it guards.
There is no `?` or `.await` between the `insert` and the guard
construction, so no early return can slip in before the guard is in place.

**Why pass `runtime_turn_cancels` as a parameter (not capture from coordinator)?**
Same reason. Keeping the helper free of `&Self` makes it trivially
testable and removes any implicit lifetime coupling.

**9 parameters is heavy.** Acknowledged. A wrapper struct
(`RuntimeTurnContext { event_queue, session_slot, ids, … }`) would clean it
up, but the only caller is the spawn body, the helper is private, and
adding a struct just to thread it through one call adds friction without
clarifying anything. **YAGNI** — leave as 9 args; revisit if a second
caller emerges.

## `RuntimeCancelGuard`

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

Lives at module scope (next to `TurnLifecycleGuard`, after
`classify_runtime_error`). `DashMap::remove` on a missing key is a no-op,
so the guard is idempotent against `cancel_dialog_turn` having already
removed the entry — it won't, because per OpenSpec design D5 we use `get`
(clone-then-cancel) not `remove` in `cancel_dialog_turn`, but defensive
idempotency is free.

**`armed` flag is required (this reverses the earlier draft).** OpenSpec
D4 / finding F-1 moves the `runtime_turn_cancels.insert(...)` to the
calling thread *before* `create_session`, to close the cold-start cancel
window. That makes entry-removal ownership span the `tokio::spawn`
boundary — exactly the situation `ActiveTurnRegistration`
(`coordinator.rs:2894~2910`) uses `armed`/`disarm()` for:

- *Calling-thread instance:* armed at construction; covers the `?`
  early-returns of `registry.get` / `create_session`. `disarm()`ed right
  after a successful `tokio::spawn`, handing ownership to the spawn body.
- *Spawn-body instance (in the helper):* armed, never disarmed — the sole
  remover on every helper exit path.

The earlier "no `armed` needed, guard is built inside the spawn task"
reasoning held only while the insert *also* lived inside the spawn body.
It no longer does (it can't, without re-opening F-1), so the flag comes
back.

## Borrow-check analysis (two `rt_session` consume sites)

`rt_session: Box<dyn AgentSession>` is consumed (moved via `dispose(self: Box<Self>)`)
on **two** paths now — the D8 pre-prompt branch and the `select!` cancel
branch. Both compile cleanly; proof below.

**Site 1 — D8 pre-prompt (`rt_session` consumed before `stream` exists):**

```rust
if cancel_token.is_cancelled() {
    let _ = rt_session.dispose().await;  // moves rt_session
    return;                              // diverges
}
let mut stream = match rt_session.prompt(...).await { ... };  // later use
```

The move happens only on a branch that ends in `return` (diverges). Rust's
move analysis sees that the fall-through path has *not* moved `rt_session`,
so the later `rt_session.prompt(...)` is valid. This is the standard
"move-in-diverging-branch" pattern — no conflict, and *simpler* than Site 2
because no `stream` is alive yet.

**Site 2 — `select!` cancel branch (`rt_session` consumed while `stream` is alive):**

1. `rt_session.prompt(&user_input, vec![])` borrows `&self`, returns the
   stream by value. After this call, `rt_session` is still owned and
   unborrowed.
2. The returned stream (`Pin<Box<dyn Stream<Item=AgentEvent> + Send>>`)
   carries no lifetime tied to `rt_session`. It owns an mpsc `Receiver`
   plumbed through `claude_runtime.rs::ClaudeSession::event_tx` (an
   `Arc<Mutex<Option<Sender>>>`); `rt_session` going away does *not*
   immediately invalidate the stream.
3. In the cancel branch, `rt_session.dispose().await` consumes the
   `Box<dyn AgentSession>`. The `stream` variable is dropped at end of
   scope (after `return`); the mpsc `Receiver` it holds drops with it.
   The `select!`'s in-flight `stream.next()` future is dropped on the way
   out — `ReceiverStream::next` is cancel-safe (drops the poll, loses no
   already-delivered item), so there is no torn read.
4. `dispose()` cancels the bridge's abort token and kills the child.
   The reader task in `claude_runtime.rs` exits on stdout EOF, drops
   its sender, but our receiver is also dropping — no one cares.

Result: both `dispose().await; return;` sites are clean consumptions. No
"`rt_session` moved while still borrowed by `stream`" error.

## Test infrastructure

### `FakeSession`

```rust
#[cfg(test)]
struct FakeSession {
    session_id: String,
    /// Pre-stocked receiver; prompt() consumes it into a stream.
    event_rx: tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<AgentEvent>>>,
    /// If Some, prompt() returns this error instead of a stream.
    prompt_err: Option<PortError>,
    /// Set to true when dispose() runs — assertion target.
    disposed: Arc<AtomicBool>,
    /// Set to true the moment prompt() is entered — lets T4 assert the
    /// D8 pre-prompt check short-circuited *before* any prompt() call.
    prompt_called: Arc<AtomicBool>,
}

#[async_trait]
impl AgentSession for FakeSession {
    fn session_id(&self) -> &str { &self.session_id }

    async fn prompt(&self, _: &str, _: Vec<AgentInputAttachment>) -> PortResult<AgentEventStream> {
        self.prompt_called.store(true, Ordering::SeqCst);
        if let Some(e) = self.prompt_err.as_ref() {
            return Err(PortError::new(e.kind.clone(), &e.message));
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

Lives inside `mod tests` (gated `#[cfg(test)]`). Constructor takes
`(session_id, Option<Receiver>, Option<PortError>, Arc<AtomicBool> disposed,
Arc<AtomicBool> prompt_called)`. `steer` is not overridden — the trait's
default impl is sufficient.

### `EventQueue` driving

`EventQueue::new(EventQueueConfig::default())` is sufficient — no
external dependencies. To assert `DialogTurnCancelled` was emitted, the
test calls `event_queue.dequeue_batch(8).await` and inspects the
returned `EventEnvelope`s. Confirmed `dequeue_batch` exists and is
public from inspection of `events/queue.rs`.

(If `EventQueue::subscribe()` is also public, tests may use the
broadcast path instead — verify during build phase. `dequeue_batch` is
the conservative fallback.)

## Test cases

Four `#[tokio::test]` cases inside the existing `mod tests` block at
`coordinator.rs:5574` (no new test file):

### T1 — `runtime_event_loop_cancels_promptly`

- Set up: pre-stocked rx with no events. **Keep the matching `Sender` alive
  in a local binding for the whole test (F-6).** If the Sender is dropped,
  `ReceiverStream::next()` yields `None` immediately, the loop exits via the
  put-back/completion path, and the test silently exercises the *wrong*
  branch (slot=Some, disposed=false) — a false pass. Holding the Sender keeps
  `stream.next()` pending so the only way out is the cancel branch.
- FakeSession (`disposed=false`, `prompt_called=false`), fresh
  `CancellationToken`, EventQueue, empty slot.
- Spawn the helper.
- `tokio::time::sleep(Duration::from_millis(10)).await;` — let helper
  enter the `select!`.
- `cancel.cancel();`
- `tokio::time::timeout(Duration::from_millis(200), task).await` —
  helper must exit.
- Assert `disposed.load(Ordering::SeqCst) == true`.
- Assert `dequeue_batch` returned a `DialogTurnCancelled` envelope.
- Assert `slot.lock().await.is_none()` — cancel skips put-back.

### T2 — `runtime_event_loop_completes_cleanly`

- Pre-stock rx with `[AgentEvent::TurnEnd { stop_reason: Completed, … }]`.
  Drop the sender afterwards (so receiver yields `None` after the one
  event if the loop polls again).
- Run helper to completion.
- Assert `disposed == false` (happy-path leaves session for reuse).
- Assert `slot.lock().await.is_some()` — session put back.
- Assert event queue saw `DialogTurnCompleted`.
- Cancel token never fired — checks the no-cancel happy path.

### T3 — `runtime_event_loop_disposes_on_error_event`

- Pre-stock rx with `[AgentEvent::Error { message: "rate limit", … }]`.
- Run helper.
- Assert `disposed == true` (matches review3 batch1 P-5 behaviour:
  Error → dispose + return, no put-back).
- Assert event queue saw `DialogTurnFailed`.
- Assert `slot.lock().await.is_none()`.

### T4 — `runtime_event_loop_skips_prompt_when_precancelled` (D8 / F-2)

- The one genuinely-new behaviour this change adds: a token already
  cancelled before the helper runs must short-circuit **before** `prompt()`.
- Set up: FakeSession (`prompt_called=false`, `disposed=false`), no
  pre-stocked rx needed (we assert prompt is never reached). Construct the
  `CancellationToken` and **`cancel()` it immediately**, before spawning.
- Run helper.
- Assert `prompt_called.load() == false` — the D8 check returned first; no
  Anthropic call would have been issued.
- Assert `disposed == true` — pre-prompt branch disposes.
- Assert event queue saw `DialogTurnCancelled`.
- Assert `slot.lock().await.is_none()`.

T1 is the mid-stream cancel smoke; T4 is the pre-prompt (cold-start window)
cancel smoke. T2 and T3 are regression guards for the two pre-existing exit
paths the helper extraction must preserve. (A separate `prompt_err` case is
*not* added — it duplicates T3's dispose+return shape and was covered by
review3 batch1 manual verification.)

## Edge cases & how they're handled

| Edge case | Handling |
|---|---|
| Cancel fires *before* `prompt()` (cold-start `create_session` window, F-1/D8) | Entry is inserted on the calling thread before `create_session` (D4), so `cancel_dialog_turn` finds the token and fires it. The helper's D8 pre-prompt `is_cancelled()` check then short-circuits: emit `DialogTurnCancelled`, dispose, return — **no `prompt()` call, no Anthropic spend**. T4 covers this. |
| Cancel fires *during* `prompt().await` (stdin write) | `prompt()` is not wrapped in `select!` (D2) and the D8 check already passed (token wasn't cancelled when we checked). The stdin write completes (≈µs), we enter `select!`, the cancel branch fires on first poll, dispose runs, bridge dies via kill_on_drop. At most one bridge command was written (one API call may *start*); worst-case latency 1 prompt() round-trip. |
| Cancel fires *after* stream returns `None` | Helper is past the loop, in put-back. `RuntimeCancelGuard::drop` removes the entry. If `cancel_dialog_turn` raced and called `cancel()` between stream-end and guard-drop, the signal goes to a token whose only listener (the `select!`) already exited. Arc drops; no side-effects. Harmless. |
| `?` early-return on calling thread (`registry.get` / `create_session` fails) **after** the entry was inserted | The calling-thread `RuntimeCancelGuard::armed(...)` (D3/D4) drops on the `?` unwind and removes the orphaned entry. This is the whole reason the `armed` flag exists. No leak. |
| `dispose().await` blocks long | `ClaudeSession::dispose` does `abort_token.cancel()` + `child.kill().await`. Neither blocks meaningfully (`cancel` is a flag set; `kill` is `TerminateProcess`/`SIGKILL` returning ~µs). No HTTP-level waits; in-flight Anthropic requests are abandoned, which is exactly the intent. |
| `event_queue.enqueue(DialogTurnCancelled)` blocks because queue full | `EventQueue::enqueue` early-returns `Ok` without enqueueing when `queue.len() >= config.max_queue_size`. Cannot hang. UI may miss the cancel event under extreme load — acceptable trade-off; the bridge still dies. |
| Two `cancel()` calls on the same token | Idempotent — `CancellationToken::cancel()` is no-op on already-cancelled. Both `cancel_dialog_turn` (per OpenSpec D5) and any direct upstream caller can safely fire. |
| Double removal: calling-thread guard + spawn-body guard both fire on the same `turn_id` | Can't happen on the common path: the calling-thread guard is `disarm()`ed right after a successful `tokio::spawn`. Even if a logic slip left both armed, `DashMap::remove` on an absent key is a no-op — at worst a redundant, harmless remove. |
| `runtime_turn_cancels` already has an entry for this `turn_id` | Impossible: `turn_id` is generated fresh per call to `start_dialog_turn` (UUID). DashMap insert is unconditional; collision would only happen if UUID generation broke, out of scope. |
| Helper panics (e.g. future code adds `unwrap()`) | The closure's `TurnLifecycleGuard::drop` (batch 2) decrements counter + resets state; the helper's `RuntimeCancelGuard::drop` removes the map entry; `rt_session` Drop kicks in via `kill_on_drop(true)` — bridge dies. All guards combine for full panic safety. |

## Spec patches

None. The OpenSpec proposal declared `_None_` for both new and modified
capabilities; nothing surfaced during technical design that demands a
spec patch.

## Risks not in canonical design

- **Helper signature has 9 parameters.** Static-analysis tools (clippy
  `too_many_arguments`) may flag this. Suppress with
  `#[allow(clippy::too_many_arguments)]` on the helper. A struct wrapper
  is the future cleanup; not done here for hotfix scope.

- **Test flakiness from the 10ms sleep before cancel.** If the test
  scheduler is starved, the helper might not have entered `select!`
  before `cancel()` fires. This is *still correct* (cancelled token
  fires on first poll) but if assertions check ordering they'd be
  flaky. Mitigation: assertions check **outcomes** (disposed flag,
  emitted event, helper exited) within a 200ms timeout — they don't
  check ordering against the sleep. CI-safe.

- **`dequeue_batch` consumption races with broadcast subscribers.** Not
  applicable in tests — we construct a fresh `EventQueue` with no
  external consumers. In production this concern doesn't apply because
  the test code never runs.

## Migration / rollback

Single-commit-on-main change. No feature flag, no data migration. The
new `runtime_turn_cancels` field is initialised empty in
`ConversationCoordinator::new` and only populated for non-bitfun
runtimes. Bitfun spawn task is unaffected (no entry inserted, no entry
read).

Rollback: `git revert` the commit. The field becomes orphaned but
inert. No cascading cleanup needed.

## Open questions

None.
