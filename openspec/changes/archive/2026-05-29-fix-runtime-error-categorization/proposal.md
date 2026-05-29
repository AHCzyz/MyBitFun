## Why

Code review 2 flagged `P5: error_category/error_detail are always None` on the runtime dispatch path in `coordinator.rs`. Three `DialogTurnFailed` emit sites in the runtime spawn task (prompt() Err, TurnEnd error stop_reason, RuntimeEvent::Error) all carry literal `None`, so the frontend's structured error UI receives no classification and downstream telemetry/alerting cannot route on category. The bitfun path at line 1518 and 2247 already populates these fields via `BitFunError::error_category() / error_detail()`; runtime path has no equivalent.

**My share of the bug**: the prompt() Err site got `None, None` from me during the previous A-group hotfix (`82970140`) when I bundled an E0063 baseline fix — I added the missing fields with placeholder Nones to make it compile. The other two sites came in with the WIP baseline at the same shape. Either way, all three need real classification.

This blocks the "Beta + monitoring" decision from code review 2: monitoring keyed on `error_category` would observe zero events for runtime errors regardless of frequency, defeating the rollout safeguard.

## What Changes

- Reuse the existing `bitfun_core_types::errors::{ErrorCategory, AiErrorDetail, classify_ai_error_message, ai_error_detail_from_message}` helpers.
- Add a small inline helper in `coordinator.rs` that classifies runtime errors: try message-string heuristics first (catches provider-embedded signals like rate-limit / quota), fall back to `PortErrorKind` structural mapping, default to `ModelError`.
- Apply the helper at all three runtime `DialogTurnFailed` sites in `handle_user_input`'s spawn task.

No public API change. No spec change.

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None._

## Impact

- **Code:** `MyBitFun/src/crates/core/src/agentic/coordination/coordinator.rs` only (+~30 lines: import + helper + 3 call-site updates).
- **Behaviour:** runtime errors now arrive at the frontend with the same structured shape as bitfun errors (category + detail). UI error rendering, retry logic that branches on category, and any telemetry that aggregates by category begin observing real values.
- **Out of scope (follow-up):** promoting `classify_runtime_error` to a method on `PortError` in the `runtime-ports` crate. Inline keeps strict 1-file scope; can refactor later when omp/bitfun adapters need the same.
