## Context

`bridge.mjs` is a long-lived Node bridge that owns one Claude Agent SDK session. The Rust side writes a JSONL `prompt` command per turn; the bridge calls `query()` which returns an async iterable, and the bridge for-awaits it to translate SDK messages into BitFun JSONL events on stdout.

Hang surface (review2 P1): if `query()` returns a working iterable but the underlying HTTP request hangs without ever yielding (network partition, dead TLS handshake, server pause), `for await` blocks the whole bridge for that turn. Mid-stream stalls (server stops sending tokens but socket stays open) are similar.

Today's only recovery is a Rust-side `child.kill()` via `dispose()` or `kill_on_drop`. That works but gives the frontend no structured "this turn timed out" signal — UI just observes the bridge die.

Already in place from prior hotfixes:
- α-1 added `classify_runtime_error` + `classify_ai_error_message`. The latter has "timeout" / "timed out" string heuristics that route to `ErrorCategory::Timeout`. So if bridge emits an `error` event whose message contains "timed out", the coordinator's runtime-path Site 3 will properly classify it.
- The bridge already has a `try { ... } catch (err) { emit error + turn_end }` shell around the prompt loop. So any throw inside the loop becomes a clean error event without further coordinator changes.

That means the bridge-side fix needs only one thing: actually throw on timeout.

## Goals / Non-Goals

**Goals:**
- A first-event timeout: from `query()` return to first message yielded.
- An idle (inter-event) timeout: between consecutive yielded messages.
- Both env-configurable; both 120 s by default.
- Timeout fires a clean `Error` whose message contains "timed out" so the existing classification heuristic catches it.
- Best-effort iterator cleanup on timeout (`iter.return()` if the iterator implements it).

**Non-Goals:**
- Cancel the underlying SDK HTTP request via `AbortSignal`. The SDK may or may not accept one; checking is an additional dependency-spec lookup beyond hotfix scope. The bridge will leak one pending fetch per timeout event, which the OS GCs when the bridge process eventually dies.
- Coordinator-side decision change about whether to dispose-vs-cache a runtime session after a Timeout event. Currently α-1 caches; the leak from above is bounded because the next `prompt()` writes a fresh JSONL command, and the cached session uses a fresh `query()` call — old hung iterator becomes garbage with no consumer.
- Tests. No JS test harness in this repo; verification is `node --check` + code review.

## Decisions

### D1. Manual iteration with `Promise.race`, not AbortController

```js
const iter = messages[Symbol.asyncIterator]();
let firstEvent = true;
while (true) {
  const timeoutMs = firstEvent ? FIRST_EVENT_TIMEOUT_MS : IDLE_TIMEOUT_MS;
  const phase = firstEvent ? 'first response' : 'next event';
  let timer;
  const timeoutPromise = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`Claude SDK ${phase} timed out after ${timeoutMs}ms`)),
      timeoutMs,
    );
  });
  let step;
  try {
    step = await Promise.race([iter.next(), timeoutPromise]);
  } catch (err) {
    clearTimeout(timer);
    try { await iter.return?.(); } catch { /* ignore */ }
    throw err;
  }
  clearTimeout(timer);
  if (step.done) break;
  firstEvent = false;
  const events = translateMessage(step.value);
  for (const ev of events) {
    process.stdout.write(JSON.stringify(ev) + '\n');
  }
}
```

**Why not AbortController:** the @anthropic-ai/claude-agent-sdk public surface for `query()` doesn't have a documented `signal` parameter in any docs I can verify from inside this hotfix. Building on an unverified API would gate this hotfix on an upstream-spec investigation. `Promise.race` works regardless and surfaces the timeout as a thrown error, which is exactly what the existing catch block expects.

**Why best-effort `iter.return()`:** if the SDK's iterator implements the optional `return()` method (per JS async iterator protocol), calling it lets the SDK release resources (close the underlying fetch, abort the stream). If not implemented, the call is a no-op. Wrapped in try/catch so any failure during cleanup doesn't shadow the timeout error.

### D2. Two timeouts, both env-configurable, both default 120s

Defaults:
- `BITFUN_CLAUDE_BRIDGE_FIRST_EVENT_TIMEOUT_MS` = 120000
- `BITFUN_CLAUDE_BRIDGE_IDLE_TIMEOUT_MS` = 120000

Lower-bound clamp at 1000ms (1 second) to prevent footgun. No upper bound.

**Why two distinct timeouts:** they protect against different failure modes. First-event protects HTTP-layer hangs (TLS handshake stalls, request never reaches API). Idle protects mid-stream stalls (API silent after first burst). A single timeout would force a trade-off — too short and slow legitimate streams die, too long and HTTP hangs take forever to surface.

**Why 120s default for both:** matches typical OpenAI/Anthropic recommendation for synchronous API calls. Tunable via env for ops who run on slower networks or want tighter SLOs.

### D3. No SDK-internal state observation

The fix does not inspect SDK internals (private fields, request objects, etc.). It observes only what the public iterator yields. This keeps the fix robust to SDK version bumps.

## Risks / Trade-offs

- **Leaked HTTP fetch per timeout.** The SDK's underlying HTTP request continues until OS-level timeout or completion. Memory cost is tens-to-hundreds of KB per leak. Bounded — bridge process resets on session dispose. Acceptable.
- **Default 120s might be too long for some users.** Configurable via env; users with stricter SLOs can lower it.
- **Default 120s might be too short for some legitimate slow streams.** Idle timeout resets on every event — slow streams that emit any token within 120s are fine. Only complete silence triggers timeout. The most pathological case (massive tool result in one chunk that takes 90+s to assemble server-side) could theoretically false-positive; users hitting that can raise the env var.
- **No coverage test.** Same constraint as previous hotfixes — verified via code review + `node --check`. The control flow is small enough that a test would be valuable but disproportionate scope for hotfix.
- **`iter.return()` on a still-pending iterator.** Per JS async iterator protocol, calling `return()` while the iterator is mid-await is allowed and signals the iterator to clean up. The SDK iterator may or may not honor it gracefully. Wrapped in try/catch so a misbehaving SDK doesn't break our timeout flow.

## Migration / rollback

Single-commit revert. No data migration. No interface change. Env vars are additive — unset behaves like before but with defaults applied.

## Open questions

None blocking. Tracked as follow-up:
- Investigate whether `@anthropic-ai/claude-agent-sdk` accepts an `AbortSignal`; if so, add it to actually cancel the underlying fetch on timeout.
- Consider promoting the timeout values to coordinator-passed config (per-runtime, per-call) instead of bridge env vars.
- Consider whether RuntimeEvent::Error with Timeout category should dispose-rather-than-cache the session in coordinator (today it caches; an explicitly-Timeout-categorized error could special-case to dispose).
