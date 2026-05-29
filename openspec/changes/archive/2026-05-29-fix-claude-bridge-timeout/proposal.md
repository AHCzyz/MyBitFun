## Why

Code review 2 flagged `P1: bridge.mjs has no timeout — query() can hang indefinitely, abort cannot cancel an in-flight API call`. The core hang surface is the `for await (const msg of messages)` loop at `bridge.mjs:197`: if the Claude Agent SDK's HTTP request stalls before yielding the first message, or stops yielding mid-stream, this loop awaits forever. The only recovery today is a hard process kill from Rust (which works after the previous A-group hotfix added `kill_on_drop`, but is a nuclear option that gives the UI no graceful timeout signal).

This is the last fix needed before the "Beta + monitoring" rollout decision can land cleanly: the previous α-1 hotfix already routes runtime errors through `ErrorCategory` so the frontend can render category-specific UI. Without a timeout signal at the bridge layer, no error of any kind is generated for hangs — telemetry observes nothing, UI just spins forever.

## What Changes

- Replace the bare `for await (const msg of messages)` with manual iteration that races each `iter.next()` against a timeout promise.
- Two timeout values, both env-configurable, both 120 s by default:
  - **First-event timeout** — from `query()` return until the first message yielded. Catches HTTP-layer hangs.
  - **Idle (inter-event) timeout** — between any two consecutive messages. Catches mid-stream stalls.
- On timeout: throw a clear `Error("Claude SDK <phase> timed out after Nms")`. The existing `catch` block at `bridge.mjs:208` already emits `error` + `turn_end` events; α-1's coordinator-side `classify_ai_error_message` already detects "timed out" / "timeout" strings and routes them to `ErrorCategory::Timeout`.
- Best-effort call to `iter.return?.()` on timeout to release SDK iterator resources.

No public API change. No Rust change. No spec change.

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None._

## Impact

- **Code:** `MyBitFun/resources/claude-bridge/bridge.mjs` only (~30 added lines).
- **Behaviour on happy path:** zero change (timer is reset on every event; cleared on stream end).
- **Behaviour on hang:** UI receives a structured `DialogTurnFailed` with `error_category: Timeout` within ≤120 s of the SDK going silent (vs. infinite hang today).
- **Behaviour on configured short timeouts:** `BITFUN_CLAUDE_BRIDGE_FIRST_EVENT_TIMEOUT_MS=10000` etc. lets ops dial in tighter SLOs without a code change.
- **Out of scope (follow-up):** integrating the SDK's `AbortSignal` (if it accepts one) for true server-side cancellation; coordinator-side decision to dispose-rather-than-cache the runtime session after a Timeout event (today α-1 caches it for reuse and the next prompt() flushes any leaked SDK state).
