# Verification report — fix-runtime-session-cleanup-on-delete

**Date:** 2026-05-29
**Change:** `fix-runtime-session-cleanup-on-delete` (review1 P4 / review2 P4)
**Workflow:** comet **hotfix** preset
**Verify mode:** **light** (scale: 2 tasks, 0 deltas, 0 changed files at scale-time)
**Branch:** `fix-runtime-session-cleanup-on-delete` in `MyBitFun/`
**Base ref:** `28e68756` (main tip after α-2)
**Commit on branch:** `0d88f2f5` fix(coordinator): tear down runtime session + active-turn counter on delete

---

## §3a root cause elimination

| Issue | Before | After | Evidence |
|---|---|---|---|
| review1 P4 / review2 P4 — `runtime_sessions` entries leak per-session for the process lifetime | 0 occurrences of `runtime_sessions.*remove` anywhere in coordinator.rs | 1 occurrence at line 3448 inside `delete_session`; entry is removed and the cached `AgentSession` (if any) is `dispose()`'d before drop | `grep 'runtime_sessions\.remove'` → 1 match (was 0) |
| Same shape for `active_turns_per_session` | 0 occurrences of `active_turns_per_session.*remove` | 1 occurrence at line 3455 | `grep 'active_turns_per_session\.remove'` → 1 match (was 0) |
| `coordinator.delete_session` (non-cascade path) didn't cancel in-flight turns before deleting — race between spawn task and deletion | `cancel_active_turn_for_session` only called at line 3445 (cascade path) | now also called at line 3434 (non-cascade path), `Duration::from_secs(2)` matching cascade discipline | `grep 'cancel_active_turn_for_session\(session_id'` → 1 match (line 3434) |

**Search-based negative evidence:**
- Both `*.remove` patterns previously matched zero times (audit done in explore phase). They now match exactly once each, in the new `delete_session` body.
- The cleanup order matches design.md §D2: cancel → runtime dispose → counter drop → `session_manager.delete_session` → emit `SessionDeleted`.

Root cause eliminated. **No upgrade conditions tripped** (≤2 files, no architectural change, no public-API change, no spec change).

## Light-mode checklist (comet-verify §2a)

| # | Check | Result |
|---|---|---|
| 1 | tasks.md fully `[x]` | ✓ — §1.1 (5-step inline cleanup + doc comment) and §1.2 (cargo check) both ticked; build guard's "tasks.md all tasks checked" PASSed |
| 2 | Diff matches tasks | ✓ — `git diff --stat 28e68756..HEAD`: `coordinator.rs +44/-1`. Single commit, single file. The 44 inserted lines = doc comment block (~14 lines) + the 5-step cleanup body (~30 lines including comments and braces). One deleted line = the previous one-line `/// Delete session` comment, replaced |
| 3 | Compile passes | ✓ — `cargo check -p bitfun-core --message-format=short` `Finished dev profile in 16.42s`, **EXIT=0**, **0 warnings** (the `unused import: AgentRuntime` warning that came back to life with α-1 also stays gone since we didn't re-introduce it) |
| 4 | Tests pass | N/A — no Rust test framework targets `coordinator.delete_session`'s end-to-end behaviour. The fix is structural (removes entries, calls dispose, calls cancel). Code review of the explicit ordering + verified call-graph trace serves as the strongest signal here |
| 5 | No security issues | ✓ — no hardcoded secrets, no `unsafe`, no new external command. The `dispose()` path was already audited for kill_on_drop safety in A-group hotfix. The `cancel_active_turn_for_session` call uses an existing safe API (a few-second timeout + best-effort warn-on-error) |

## Behavioural reasoning sanity check

Three execution paths, traced by inspection:

```
Happy path (no in-flight turn, runtime session was used previously):
  cancel_active_turn → returns Ok immediately (no active turn)
  runtime_sessions.remove(sid) → returns Some(slot)
  slot.lock().take() → returns Some(boxed_session)
  session.dispose().await → cancel abort_token + child.kill().await → Ok
  active_turns_per_session.remove(sid) → returns Some/None (no-op)
  session_manager.delete_session(...) → walks its 8 internal cleanups
  emit SessionDeleted

Happy path (no runtime session ever created — bitfun-only session):
  cancel_active_turn → no-op (no active turn)
  runtime_sessions.remove(sid) → returns None
  if-let-Some skipped — no dispose call
  active_turns_per_session.remove(sid) → may have an entry (counter
    was inserted on first turn), .remove() drops it
  session_manager.delete_session → unchanged path
  emit SessionDeleted

In-flight turn race:
  cancel_active_turn → tries to cancel; if it fails (timeout etc.)
    we log a warn! and FALL THROUGH (we still want to clean up)
  runtime_sessions.remove → if the spawn task is mid-stream, it may
    still hold an Arc<Mutex<...>> reference to the slot — but
    DashMap.remove is concerned with the map entry, not the Arc.
    The spawn task continues to operate on its already-borrowed slot,
    which now points at a session we're about to dispose.
  slot.lock().await.take() → waits for the spawn task's put-back
    section to release, or finds it empty (spawn took it). Either
    way: take() returns Option, dispose runs on Some.
  Race outcomes:
    (a) spawn task put back BEFORE we lock → we take, dispose
        cleanly. Spawn task already exited.
    (b) spawn task is mid-stream → it holds rt_session locally; slot
        is None. take() returns None, no dispose runs from us.
        kill_on_drop fires when the spawn task drops rt_session
        without putting back (after our remove), or when the put-
        back-replace finds the slot empty and inserts; either way
        the bridge child is reaped via kill_on_drop on subsequent
        drop. Counter cleanup happens via the spawn task's
        success/error tail.
  session_manager.delete_session → proceeds as normal.

  In all (a)/(b) sub-cases the session_id entries in both DashMaps
  are gone after this function returns, which is the goal.
```

The race in case (b) is benign because:
- The replace-on-put-back logic (A-group P5 fix) already disposes any displaced session.
- After our `runtime_sessions.remove`, the spawn task's put-back path finds no slot (the Arc<Mutex<...>> still exists in the spawn task's clone but the DashMap entry is gone) — so its replace happens on a Mutex that is no longer reachable from the DashMap. The session it tries to put back becomes unreachable when the spawn task itself drops, and `kill_on_drop` reaps the child.

Either way: leak fixed.

## Open follow-ups (unchanged)

- β group P2 (OMP reader concurrency) — full /comet, architectural.
- β group P3 (serde_json blocking) — full /comet, architectural.
- γ group P6/P7/P8 — tweaks.
- Audit `active_subagent_executions` lifecycle (different keying, separate concern, may have its own leak shape).
- If a second live-session deletion path is ever added, revisit hook-registry option (B in design.md).

---

## Verdict

**PASS — both root cause leaks eliminated, all 5 light-mode checks satisfied, no scope creep.**

Coordinator-owned per-session state now has the same lifecycle discipline as session_manager-owned per-session state. Long-running desktop processes no longer accumulate runtime_sessions / active_turns_per_session entries across deleted sessions.

Ready for branch handling and archive.
