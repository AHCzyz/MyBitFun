## Context

Three runtime-path `DialogTurnFailed` sites in `coordinator.rs::handle_user_input`'s spawn task carry literal `None` for `error_category` and `error_detail`. The bitfun path at lines 1518 and 2247 calls `BitFunError::error_category()` / `error_detail()` to fill these. We need an equivalent for runtime errors but `PortError` doesn't have these methods.

Existing helpers in `bitfun_core_types::errors` (already used by `BitFunError`):
- `classify_ai_error_message(msg: &str) -> ErrorCategory` — heuristics over the message string. Catches provider-embedded signals: rate-limit, quota, billing, content-policy, etc. Returns `Unknown` if nothing matches.
- `ai_error_detail_from_message(msg: &str, category: ErrorCategory) -> AiErrorDetail` — builds the structured detail (provider, code, retryable, etc.) from category + message.

`PortErrorKind` has 7 variants: `NotAvailable, NotFound, InvalidRequest, PermissionDenied, Cancelled, Timeout, Backend`.

## Goals / Non-Goals

**Goals:**
- Every `DialogTurnFailed` from the runtime path carries a real `ErrorCategory` and `AiErrorDetail`, matching the bitfun path's contract.
- Provider-embedded signals (rate-limit / quota in the SDK error message string) are routed to their proper category, not flattened to a single "runtime error" bucket.

**Non-Goals:**
- Move the helper into `PortError` as a trait method. Cleaner long-term but expands scope to a 2nd crate; defer.
- New categories or detail fields. Reuse what exists.
- Cancelled-as-failure semantics. Cancellations normally route through `DialogTurnCancelled`, not `DialogTurnFailed`. If a cancellation does surface here, mapping it to `Unknown` is acceptable hotfix behaviour.

## Decisions

### D1. Two-tier classifier: message-first, kind-fallback

```rust
fn classify_runtime_error(message: &str, kind: Option<&PortErrorKind>) -> ErrorCategory {
    let from_message = classify_ai_error_message(message);
    if !matches!(from_message, ErrorCategory::Unknown) {
        return from_message;
    }
    match kind {
        Some(PortErrorKind::Timeout)          => ErrorCategory::Timeout,
        Some(PortErrorKind::PermissionDenied) => ErrorCategory::Auth,
        Some(PortErrorKind::NotAvailable)     => ErrorCategory::ProviderUnavailable,
        Some(PortErrorKind::InvalidRequest)
            | Some(PortErrorKind::NotFound)   => ErrorCategory::InvalidRequest,
        Some(PortErrorKind::Cancelled)        => ErrorCategory::Unknown,
        Some(PortErrorKind::Backend) | None   => ErrorCategory::ModelError,
    }
}
```

**Why message-first:** the SDK frequently returns errors as `PortErrorKind::Backend` with the actual signal embedded in the message string (e.g. `"rate_limit_error from anthropic"` becomes `Backend("rate_limit_error from anthropic")`). Direct kind-mapping would route this to `ModelError`, losing the routable category. Message classification catches it first. Only if the message has no recognized signal do we fall back to kind.

**Why `Cancelled → Unknown`:** cancellations normally don't reach `DialogTurnFailed` (they emit `DialogTurnCancelled`). If one slips through, "Unknown" is honest — it's neither a real failure nor a category we have telemetry for.

### D2. Inline in coordinator.rs, not as `PortError` method

Keeps the change strictly 1-file. The mapping is opinionated about what counts as runtime-relevant categorization (e.g. ProviderUnavailable for NotAvailable), and that opinion belongs at the call site in coordinator, not in the generic ports crate. If/when `omp_runtime` and `bitfun_runtime` need similar mapping, promote.

### D3. Apply to all three sites uniformly

The same `classify_runtime_error(...) → ai_error_detail_from_message(...)` pair runs at:
1. `prompt()` Err branch — `kind` available from `PortError`
2. `TurnEnd` with `stop_reason` not in `{Completed, Aborted}` — no `kind`, message synthesized as `"Runtime turn ended: {:?}"`
3. `RuntimeEvent::Error` from bridge — no `kind`, message is the bridge-emitted string

Sites 2 and 3 pass `None` for `kind`, so classification leans entirely on message-string heuristics + `ModelError` fallback. That's the right behaviour: those paths intrinsically lack structured error metadata.

## Risks / Trade-offs

- **Misclassification on novel SDK error strings.** `classify_ai_error_message` was designed for OpenAI/Anthropic-style messages; Claude SDK errors may use different phrasing. *Mitigation:* falls through to `ModelError` (still better than `None`); bug is observable as "everything bucketed to ModelError" and addressable by extending the heuristic dictionary in `core-types/errors.rs`.
- **`PortErrorKind::Backend` is the catch-all.** Mapping it to `ModelError` means transient infrastructure errors (e.g. bridge stdin pipe broken) get categorized as model errors. Imperfect but consistent with how the bitfun path treats unclassified `BitFunError::AIClient`.
- **No test coverage.** Same constraint as previous hotfixes — no runtime-mockable test fixture exists. Verified via cargo check + code review of the 3 sites.

## Migration / rollback

Single-commit revert. No data migration. No interface change.

## Open questions

None blocking. Tracked as follow-up:
- Promote `classify_runtime_error` to `impl PortError` once another adapter needs it.
- Extend `classify_ai_error_message` heuristics with Claude SDK-specific strings if telemetry shows ModelError-bucket dominance.
