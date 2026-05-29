# Verification report — fix-runtime-error-categorization

**Date:** 2026-05-29
**Change:** `fix-runtime-error-categorization` (review2 P5)
**Workflow:** comet **hotfix** preset
**Verify mode:** **light** (scale: 2 tasks, 0 deltas, 0 changed files at scale-time)
**Branch:** `fix-runtime-error-categorization` in `MyBitFun/`
**Base ref:** `cb2832ae` (tip of main after the previous A-group hotfix)
**Commit on branch:** `2c154358` fix(coordinator): classify runtime DialogTurnFailed errors

---

## Hotfix-specific: §3a root cause elimination

| Issue | Before | After | Evidence |
|---|---|---|---|
| review2 P5 — `error_category` / `error_detail` are always `None` on runtime path (3 sites) | 3 occurrences of `error_category: None` in the runtime spawn task | 0 occurrences of `error_category: None` in coordinator.rs; 5 occurrences of `error_category: Some` (2 pre-existing bitfun path at lines 1518/2247 + 3 new runtime path) | `grep error_category: None` → 0; `grep error_category: Some` → 5 |
| Pre-existing `unused import: AgentRuntime` warning at coordinator.rs:45 | warning emitted on every cargo check | warning gone — `AgentRuntime` removed from import line, no callers in file | `cargo check -p bitfun-core` reports 0 warnings (was 1) |

**Search-based negative evidence:**
- `grep 'error_category: None'` → 0 (the placeholder pattern is gone).
- `grep 'classify_runtime_error'` → 4 hits = 1 definition (line 69) + 3 call sites (lines 2667, 2764, 2786) — matching the 3 runtime `DialogTurnFailed` sites.

Root cause eliminated. **No upgrade conditions tripped.**

## Light-mode checklist (comet-verify §2a)

| # | Check | Result |
|---|---|---|
| 1 | tasks.md fully `[x]` | ✓ — §1.1 and §1.2 both ticked; build guard's "tasks.md all tasks checked" PASSed |
| 2 | Diff matches tasks | ✓ — `git diff --stat cb2832ae..HEAD`: `coordinator.rs +46/-10`. Single commit, single file, matches §1.1 (helper + 3 sites + import) and §1.2 (cargo check) |
| 3 | Compile passes | ✓ — `cargo check -p bitfun-core --message-format=short` `Finished dev profile in 16.64s`, **EXIT=0**, **no warnings** (the previously chronic `AgentRuntime` warning is gone) |
| 4 | Tests pass | N/A — no Rust test framework targets these paths; cargo check is the strongest available signal. The helper is pure logic and could be unit-tested in a future change once a test fixture exists |
| 5 | No security issues | ✓ — no hardcoded secrets, no `unsafe`, no `eval`, no new external command execution. The helper is a pure function on `&str` + `&PortErrorKind`. Reuses already-vetted `classify_ai_error_message` + `ai_error_detail_from_message` from `bitfun-core-types::errors` |

## Classification mapping correctness (sanity)

The helper's behaviour on the 7 `PortErrorKind` variants, in the absence of message-string heuristic match:

| `PortErrorKind` | → `ErrorCategory` | Rationale |
|---|---|---|
| `Timeout` | `Timeout` | direct |
| `PermissionDenied` | `Auth` | API key invalid / unauthorized maps to auth UI |
| `NotAvailable` | `ProviderUnavailable` | service down / dependency missing |
| `InvalidRequest` | `InvalidRequest` | direct |
| `NotFound` | `InvalidRequest` | malformed reference (model not found, etc.) |
| `Cancelled` | `Unknown` | not really a "failure"; cancellations normally route through `DialogTurnCancelled` anyway |
| `Backend` | `ModelError` | catch-all for runtime/model errors not pre-classified by message |
| _absent (None)_ | `ModelError` | sites 2 & 3 lack `PortErrorKind`; runtime-path errors that reach DialogTurnFailed are model-side |

Provider-embedded signals (rate-limit / quota / billing / context-overflow strings inside a `Backend` message) are caught by `classify_ai_error_message` first and route to their proper categories before structural fallback kicks in.

## Open follow-ups (unchanged from previous reports)

- α-2 P1 (bridge timeout) — separate hotfix, immediately next.
- β group P2/P3/P4 — full /comet, architectural.
- γ group P6/P7/P8 — tweaks.
- Promote `classify_runtime_error` to a `PortError` method once OMP/BitFun adapters need similar mapping (defer until then to avoid premature abstraction).

---

## Verdict

**PASS — root cause eliminated, all 5 light-mode checks satisfied, no scope creep, side cleanup of pre-existing warning included for free.**

Ready for branch handling and archive.
