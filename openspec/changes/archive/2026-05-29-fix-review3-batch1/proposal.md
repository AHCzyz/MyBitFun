## Why

review3 found four issues in the previous hotfix commits that need immediate correction before Beta:

- **P-1** 🔴 `classify_runtime_error`'s kind-fallback is dead code — `classify_ai_error_message` never returns `Unknown` (its else is `ModelError`), so the early-return guard is always true and the structural `PortErrorKind` mapping never fires. Most common runtime errors (missing API key, missing Node) get misclassified as `ModelError`.
- **P-4** 🟠 `iter.return?.()` in bridge.mjs has no timeout — if the SDK's return() awaits the in-flight next(), the hang migrates from next to return.
- **P-5** 🟡 `RuntimeEvent::Error` still puts session back in cache — bridge may be in a bad state; should dispose like the prompt() Err branch.
- **P-7** 🟡 `delete_session` doc comment inaccurately describes session_api.rs's behavior.

## What Changes

- `coordinator.rs`: rewrite `classify_runtime_error` to kind-first/message-fallback-only-for-Backend-or-None; change `RuntimeEvent::Error` branch from `break` to `dispose + cleanup + return`; fix doc comment on `delete_session`.
- `bridge.mjs`: wrap `iter.return?.()` in a 2s `Promise.race` timeout.

## Capabilities

### New Capabilities
_None._

### Modified Capabilities
_None._

## Impact

- **Code:** 2 files, ~25 lines changed.
- **Behaviour:** runtime errors now correctly classified (Auth/ProviderUnavailable/InvalidRequest instead of ModelError); bridge timeout cleanup can't re-hang; error-state sessions are disposed instead of reused.
