## Context

review3 audited the 5 hotfix commits and found integration-level issues that individual patches missed. This batch addresses the 4 items reviewer marked "must fix" or "strongly recommended" that don't require architectural changes.

## Decisions

### D1. P-1: kind-first classifier (reviewer's "方案 A")

```rust
fn classify_runtime_error(message: &str, kind: Option<&PortErrorKind>) -> ErrorCategory {
    match kind {
        Some(PortErrorKind::Timeout) => return ErrorCategory::Timeout,
        Some(PortErrorKind::PermissionDenied) => return ErrorCategory::Auth,
        Some(PortErrorKind::NotAvailable) => return ErrorCategory::ProviderUnavailable,
        Some(PortErrorKind::InvalidRequest) | Some(PortErrorKind::NotFound) => {
            return ErrorCategory::InvalidRequest
        }
        Some(PortErrorKind::Cancelled) => return ErrorCategory::Unknown,
        Some(PortErrorKind::Backend) | None => {}
    }
    classify_ai_error_message(message)
}
```

Structural PortErrorKind is the more reliable signal for runtime-path errors. Message heuristics only run for Backend (where the SDK embeds provider signals in the message body) or when no kind is available (RuntimeEvent::Error from bridge).

### D2. P-5: Error event → dispose + early return

Matches prompt() Err branch exactly. Session in error state should not be reused — bridge's internal state may be corrupted (stuck in catch block, SDK retry state dirty). Next turn will create a fresh session via or_insert_with.

### D3. P-4: iter.return with 2s hard cap

If SDK's return() awaits the pending next(), we give it 2s then abandon. The timeout promise resolving (not rejecting) means we silently move on — the original timeout error still propagates via the outer throw.

### D4. P-7: Accurate doc comment

session_api.rs's `delete_persisted_session` instantiates PersistenceManager directly and only touches disk. It never calls session_manager.delete_session. The doc comment should say this accurately.
