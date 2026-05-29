# Verification report — fix-claude-bridge-timeout

**Date:** 2026-05-29
**Change:** `fix-claude-bridge-timeout` (review2 P1)
**Workflow:** comet **hotfix** preset
**Verify mode:** **light** (scale: 2 tasks, 0 deltas, 0 changed files at scale-time)
**Branch:** `fix-claude-bridge-timeout` in `MyBitFun/`
**Base ref:** `2c154358` (tip of main after α-1)
**Commit on branch:** `28e68756` fix(claude-bridge): first-event + idle timeouts on SDK query loop

---

## §3a root cause elimination

| Issue | Before | After | Evidence |
|---|---|---|---|
| review2 P1 — `for await (const msg of messages)` consumes the SDK iterator with no timeout; SDK hangs propagate as infinite UI hangs | 1 occurrence of the bare `for await (const msg of messages)` at bridge.mjs:197 | 0 occurrences of that loop; replaced with `iter.next()` + `Promise.race` against per-step timeout (lines 224–238); best-effort `iter.return?.()` cleanup on timeout (line 242) | `grep 'for await (const msg of messages)'` → 0 matches; `grep 'FIRST_EVENT_TIMEOUT_MS\|IDLE_TIMEOUT_MS\|Promise\.race\|iter\.next\|iter\.return'` → all present at expected lines (36, 40, 217, 224, 238, 242) |

**Search-based negative evidence:**
- `for await (const msg of messages)` → 0 matches (the original hang surface is gone).
- The other for-await in the file at line 165 (`for await (const line of rl)`) is the stdin command loop — different shape, different scope, intentionally untouched.

Root cause eliminated. **No upgrade conditions tripped.**

## Light-mode checklist (comet-verify §2a)

| # | Check | Result |
|---|---|---|
| 1 | tasks.md fully `[x]` | ✓ — §1.1 (timeout config + manual iteration + iter.return cleanup) and §1.2 (verify) both ticked |
| 2 | Diff matches tasks | ✓ — `git diff --stat 2c154358..HEAD`: `bridge.mjs +55/-2`. Single commit, single file. Diff covers (a) timeout env parsing block, (b) replaced for-await body, (c) inline doc comment — all match §1.1 |
| 3 | Compile passes | ✓ — `node --check resources/claude-bridge/bridge.mjs` → **EXIT=0** (no syntax errors) |
| 4 | Tests pass | N/A — no JS test harness in this repo. Light-mode skips spec-scenario verification. The fix is small, control-flow-only, verifiable by code review |
| 5 | No security issues | ✓ — no hardcoded secrets, no shell-out, no `eval`. Env-var reads use `parseInt` with `Number.isFinite` guard (NaN-safe) and a 1000 ms lower clamp (no zero-timeout footgun). The thrown error message includes only timeout phase + duration, no sensitive payload |

## Behavioural reasoning sanity check

Three execution paths through the new loop, traced by inspection:

```
Happy path (stream completes normally):
  iter.next() resolves with {done: false, value: msg}
  → clearTimeout (no leak)
  → translateMessage + emit events
  → loop
  ...
  iter.next() resolves with {done: true}
  → clearTimeout
  → break
  → outer try emits turn_end{Completed}

First-event timeout (HTTP-layer hang):
  firstEvent = true; timeout = FIRST_EVENT_TIMEOUT_MS
  iter.next() never resolves → timeoutPromise rejects
  → catch block: clearTimeout, await iter.return?.() (best-effort), throw
  → outer catch at bridge.mjs:208 emits {error, "Claude SDK first response timed out after Nms"} + {turn_end, "error"}
  → α-1's classify_ai_error_message sees "timed out" → ErrorCategory::Timeout
  → frontend receives DialogTurnFailed with category=Timeout

Idle timeout (mid-stream stall):
  firstEvent = false (after first event arrived)
  Same as above, timeout = IDLE_TIMEOUT_MS, message says "next event timed out"
  Same routing path.
```

Timer cleanup audit:
- Happy path: `clearTimeout(timer)` called explicitly after each successful `iter.next()` race (line 245).
- Timeout path: `clearTimeout(timer)` called inside the catch handler (line 240) before re-throwing.
- No code path leaves a timer active after the race resolves.

## Concurrency check

The bridge processes one stdin command at a time (outer `for await (const line of rl)` on stdin). No two `query()` calls run concurrently. The new timeout state (`firstEvent`, `timer`, `iter`) is local to one prompt's loop — no shared state, no lock contention.

## Open follow-ups (unchanged from previous reports)

- β group P2/P3/P4 — full `/comet`, architectural.
- γ group P6/P7/P8 — tweaks.
- Investigate `@anthropic-ai/claude-agent-sdk` `AbortSignal` support for true server-side cancellation on timeout (replaces the best-effort `iter.return?.()` with proper request abort).
- Decide whether α-1's coordinator-side caching of a runtime session after a Timeout-categorized RuntimeEvent::Error should special-case to dispose instead of cache.

---

## Verdict

**PASS — root cause eliminated, all 5 light-mode checks satisfied, no scope creep.**

Bridge SDK hangs now surface as structured `{error_category: Timeout}` events within the configured timeout window. Combined with α-1, the runtime-path observability story is now complete: every failure mode (P1 hang, P5 unclassified errors, A-group P1 stuck-state, A-group P5 orphan child) emits a routable, classified DialogTurnFailed.

Ready for branch handling and archive.
