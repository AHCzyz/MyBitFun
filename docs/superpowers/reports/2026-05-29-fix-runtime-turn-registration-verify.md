# Verification report — fix-runtime-turn-registration

**Date:** 2026-05-29 | **Workflow:** hotfix | **Verify mode:** light
**Commit:** `d4b95828` fix(coordinator): panic-safe TurnLifecycleGuard for runtime spawn (review3 P-3)

## Root cause elimination

| Issue | Fixed | Evidence |
|---|---|---|
| P-3 runtime spawn task non panic-safe | ✓ TurnLifecycleGuard owns counter + state-reset | `coordinator.rs:97-125` defines guard at module level; `coordinator.rs:2710` instantiates it at runtime spawn entry; bitfun spawn at `coordinator.rs:3073` references the same lifted type |
| Three manual `fetch_sub + reset_session_state_if_processing` triples in runtime spawn body | ✓ removed | grep over runtime spawn block (lines 2696..2825): zero matches for either method (only comments reference them) |

## Light-mode checks

| # | Check | Result |
|---|---|---|
| 1 | tasks.md [x] | ✓ 8/8 |
| 2 | Diff matches tasks | ✓ 1 file (`coordinator.rs`), +62/-56 (lift + delete duplications) |
| 3 | Compile | ✓ `cargo check -p bitfun-core --message-format=short` EXIT=0 (50.77s) |
| 4 | Tests | ✓ `reset_session_state_if_processing_resets_the_matching_turn` + `_ignores_a_newer_turn` PASS (2/2) |
| 5 | Security | ✓ no secrets, no new `unsafe`, no new external IO |

## Behavioural impact

- Panic on any `await` inside the runtime spawn body (e.g. `event_queue.enqueue`, `stream.next`, future code) now decrements `active_counter` and unsticks `Session.state` from `Processing`. Pre-fix: counter leaked → every later `wait_session_drained` deadline-out → every `cancel_dialog_turn` / `delete_session` paid full timeout window.
- Bitfun and runtime spawn paths now share one RAII contract — no future divergence between them.
- `dispose()` ordering unchanged on every path; event ordering on the happy path unchanged.

**Verdict: PASS**
