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
