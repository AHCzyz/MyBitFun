# Verification report — fix-runtime-session-lifecycle

**Date:** 2026-05-29
**Change:** `fix-runtime-session-lifecycle`
**Workflow:** comet **hotfix** preset
**Verify mode:** full (scale auto-selected by task count = 4; substantively a 2-file localized fix — see §Scale)
**Branch:** `fix-runtime-session-lifecycle` in `MyBitFun/`
**Base ref:** `0b4ff520` (multi-runtime WIP baseline; itself on top of `97f50a69` → `42432b0e`)
**Commits on branch:**
- `82970140` fix(coordinator): runtime session lifecycle on error/displacement (review1 P1+P5)
- `cb2832ae` fix(claude_runtime): kill_on_drop on bridge Child as orphan-process safety net

**Skill availability:** `openspec-verify-change` not installed; full-mode checks executed manually (matches the B group precedent in `2026-05-29-harden-runtime-resource-fetch-verify.md`).

---

## Hotfix-specific: §3a root cause elimination

For each issue called out in `proposal.md`, the after-state was confirmed by reading the post-fix file directly:

| Issue | Root cause (before) | Code state (after) | Evidence |
|---|---|---|---|
| **P1** | spawn task `Err(e)` branch → `dispose()` → `return`, skipping counter/state cleanup | `dispose()` → `fetch_sub(1)` → `reset_session_state_if_processing(...)` → `return` | coordinator.rs:2649–2657 |
| **P5-A** | `*slot_guard = Some(rt_session)` silently overwrote any prior session | `slot_guard.replace(rt_session)` returns displaced; `if let Some(prev) = … { prev.dispose().await }` off-lock | coordinator.rs:2773–2778 |
| **P5-B** | `tokio::process::Child` defaults `kill_on_drop=false`; ClaudeSession has no `Drop` → orphan Node bridge on any non-`dispose()` drop path | `Command::new(&node_binary)…stderr(…).kill_on_drop(true).spawn()` | claude_runtime.rs:149 |
| **E0063** (pre-existing in WIP baseline) | `AgenticEvent::DialogTurnFailed { session_id, turn_id, error }` missing `error_category` and `error_detail` | initializer now carries `error_category: None, error_detail: None,` matching all 4 other `DialogTurnFailed` sites in the file | coordinator.rs:2643–2644 |

**Search-based negative evidence:**
- `grep '\*slot_guard = Some' coordinator.rs` → 0 matches (the silent-overwrite pattern is gone).
- `grep 'rt_session\.dispose().await' -C 2 coordinator.rs` → both occurrences are followed by counter decrement + state reset (success path tail) or directly precede the new cleanup-then-return (error path).
- `grep 'kill_on_drop' claude_runtime.rs` → 1 match at line 149.

All root causes eliminated. **No upgrade conditions tripped** (no architectural change, no new public API, no scope creep beyond review1.md A group + the bundled E0063 baseline fix).

---

## Full-mode checklist (comet-verify §2b)

| # | Check | Applicability | Result |
|---|---|---|---|
| 1 | tasks.md fully `[x]` | applies | ✓ — §1 (3 tasks) and §2 (1 task) all ticked, build guard's "tasks.md all tasks checked" PASSed |
| 2 | Implementation matches `design.md` | applies | ✓ — D1 (P1 explicit cleanup before return), D2 (replace-and-dispose at put-back), D3 (kill_on_drop safety net) each traced 1:1 to commits 82970140 / cb2832ae |
| 3 | Implementation matches Superpowers Design Doc | **N/A** — hotfix preset skips Superpowers brainstorming; no Design Doc exists |
| 4 | Capability spec scenarios pass | **N/A** — proposal.md "Modified Capabilities: None" (internal coordinator state machine, no spec coverage) |
| 5 | proposal.md goals satisfied | applies | ✓ — see §3a above; each numbered issue addressed |
| 6 | delta spec ↔ design doc consistency | **N/A** — no delta spec |
| 7 | `docs/superpowers/specs/` design doc reachable | **N/A** — hotfix has no associated Design Doc |

## Light-mode checklist (also satisfied, kept as redundant evidence)

| # | Check | Result |
|---|---|---|
| 1 | tasks.md `[x]` | ✓ |
| 2 | Diff matches tasks | ✓ — `git diff --stat 0b4ff520..HEAD`: `coordinator.rs +18/-1`, `claude_runtime.rs +5/-0`. Three logical changes match three task entries (1.1, 1.2, 2.1); kill_on_drop in claude_runtime.rs is the second half of 1.2 |
| 3 | Compile passes | ✓ — `cargo check -p bitfun-core --message-format=short` `Finished dev profile in 1m 05s` (exit 0 captured directly). One pre-existing `unused import: AgentRuntime` warning at coordinator.rs:45 — out of scope, follow-up |
| 4 | Tests pass | N/A — no test framework targets these paths; cargo check is the strongest available signal. Future tests would need a mockable `AgentRuntime` fixture, which doesn't exist yet (out of scope per design Non-Goals) |
| 5 | No security issues | ✓ — no hardcoded secrets, no new `unsafe`, `eval`, or external command execution; the only env-var read in the touched files is the unchanged `ANTHROPIC_API_KEY` check in `claude_runtime::create_session` |

## Scale note

`comet-state scale` selected `full` because **task count (4) ≥ threshold (3)**. Material change is **2 files, +22 / -1 lines, 0 capabilities**, which is comfortably under light thresholds by file/spec count. Full-mode evidence collected anyway, but most full-mode checks are N/A for hotfix preset.

## Concurrency reasoning sanity check (P5-A)

Since this is a concurrency fix without a runtime test, traced the race outcomes by inspection:

```
Slot starts empty. T1 takes None, creates session A. T2 takes None
(slot still empty because T1 already took), creates session B.
Both spawn tasks run; each holds its own session.

Finish order doesn't matter:
  - If T1 finishes first: replace(A) returns None → no dispose. Slot now holds A.
    Later T2 finishes: replace(B) returns Some(A) → dispose(A). Slot now holds B.
  - If T2 finishes first: symmetric — A is the survivor, B is disposed.

Either way: 2 sessions created during the race, 1 disposed cleanly,
1 cached for the next turn. No orphan child process. ✓
```

The fix accepts the brief duplication (one extra Node bridge process spawned during the race window) but eliminates the persistent leak. A future change could serialize per-session via semaphore/actor to avoid the duplication entirely; tracked as follow-up in design.md.

## Open follow-ups

- Same `kill_on_drop` + replace/dispose pattern likely applies to `omp_runtime.rs` and `bitfun_runtime.rs`. Review only flagged ClaudeRuntime; broader sweep is a separate change.
- Pre-existing `unused import: AgentRuntime` warning at `coordinator.rs:45` — leave for code-style cleanup pass.
- Per-session turn serialization to avoid duplicate session creation on race. Not blocking.
- Mockable `AgentRuntime` fixture so future hotfixes in this area can have proper TDD coverage.

---

## Verdict

**PASS — root causes eliminated, build clean, all applicable full-mode checks satisfied, no scope creep.**

Ready for branch handling and archive.
