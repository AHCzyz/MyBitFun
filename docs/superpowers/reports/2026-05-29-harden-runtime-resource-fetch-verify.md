# Verification report — harden-runtime-resource-fetch

**Date:** 2026-05-29
**Change:** `harden-runtime-resource-fetch`
**Workflow:** comet full
**Verify mode:** full (scale auto-selected by task count; substantively a single-file refactor — see §Scale below)
**Branch:** `harden-runtime-resource-fetch` in `MyBitFun/`
**Base ref:** `42432b0e` (origin/master, the multi-runtime feature commit reviewed in `review1.md`)
**Commits on branch:**
- `4883f414` harden(scripts): centralize HTTPS fetch with protocol/redirect/auth controls
- `97f50a69` harden(scripts): verify OMP binary SHA-256 against GitHub release digest

**Verifier:** Kiro (in-session, single-file change → no external review)
**Skill availability:** `openspec-verify-change` skill not installed in this environment; verification performed manually against the comet-verify full-mode checklist (items 1–7) plus a runtime test driver (see §4).

---

## 1. tasks.md fully checked  ✓

All boxes in `openspec/changes/harden-runtime-resource-fetch/tasks.md` §1–§5 ticked. §6 (manual verification) was relocated from a checklist into a verify-phase note during the build phase — the actual scenarios live in `specs/runtime-resource-fetch/spec.md` and are exercised in §4 below.

## 2. Implementation matches `design.md`  ✓

Trace from each architectural decision to the code:

| Decision | Code location | Status |
|---|---|---|
| **D1** HTTPS-only whitelist (initial + every redirect) | `httpsGetWithRedirects` step() — `if (parsed.protocol !== 'https:') fail('EPROTOCOL', …)` | ✓ |
| **D2** Redirect cap as decremented counter | `httpsGetWithRedirects` — `let hopsLeft = maxRedirects` then `if (hopsLeft <= 0) fail('EREDIRECT_LIMIT', …)` | ✓ |
| **D3** SHA-256 from `release.assets[].digest` (same response) | `ensureOmpBinary` — `release.assets.find(a => a.name === target.remoteName)` then `parseAssetDigest(asset.digest)` | ✓ |
| **D4** Missing/malformed → soft-fail; mismatch → hard-fail | `ensureOmpBinary` — `if (!expected) { warn; return; }` vs. `if (...!timingSafeEqual(actual, expected)) { unlink; throw; }` | ✓ |
| **D5** Token forwarded only to GitHub hosts; dropped on off-host hop | `httpsGetWithRedirects` — `if (authBearer && isGitHubHost(parsed.hostname)) reqHeaders.Authorization = …` (re-evaluated on every step()) | ✓ |
| **D6** Single shared core, two thin wrappers | `httpsGetWithRedirects` core; `fetchJson` and `downloadFile` are 8 / 8 lines respectively | ✓ |
| **D7** `stream.destroy()` on both finish and error | `pipeToFile` — `finish()` calls `stream.destroy()` once on either branch; `settled` guard ensures single-shot promise | ✓ |

## 3. Implementation matches the technical design doc  ✓

`docs/superpowers/specs/2026-05-29-harden-runtime-resource-fetch-design.md` — all sections traced:

| RFC section | Code | Notes |
|---|---|---|
| Module shape (helper layer + 2 wrappers) | matches in production file | ✓ |
| `httpsGetWithRedirects` signature | matches: `(url, opts)` → `Promise<IncomingMessage>` | ✓ |
| Per-hop control flow incl. `.code` taxonomy | all four error codes (`EPROTOCOL`, `EREDIRECT_LIMIT`, `EREDIRECT_MALFORMED`, `EHTTP`) emitted at the documented points | ✓ |
| `isGitHubHost` exact-match + suffix on lowercased hostname | matches; verified by U1 below | ✓ |
| `readGitHubToken` trim semantics | matches; verified by U2 below | ✓ |
| `pipeToFile` settled-flag + observe `res.on('error')` | matches | ✓ |
| `parseAssetDigest` regex + 32-byte Buffer | matches; verified by U3 below | ✓ |
| `sha256File` two-pass via `createReadStream` → `createHash` | matches; verified by U4 below | ✓ |
| `ensureOmpBinary` flow / branch table | matches the table in §"OMP integrity verification → ensureOmpBinary flow" | ✓ |
| Spec patch (token trim) | reflected in `specs/runtime-resource-fetch/spec.md` "GITHUB_TOKEN is whitespace-only" scenario | ✓ |

No drift: every helper / signature / branch in the RFC has a corresponding line in production code, and vice versa.

## 4. Capability spec scenarios — runtime evidence  ✓

A standalone test driver (`openspec/changes/harden-runtime-resource-fetch/.comet/handoff/verify-tmp/run.mjs`) loads the production module via a one-line patch (re-export internals into a tmp copy) and exercises every spec scenario.

**Run command:**
```
node openspec/changes/harden-runtime-resource-fetch/.comet/handoff/verify-tmp/run.mjs
```

**Result:** `11/11 passed`

**Output (verbatim):**
```
[PASS] U1  isGitHubHost predicate covers exact + subdomain + non-match + case
[PASS] U2  readGitHubToken trim semantics (unset / empty / whitespace / valid / padded)
[PASS] U3  parseAssetDigest format validation (null / wrong-prefix / wrong-length / non-hex / valid / case-fold)
[PASS] U4  sha256File matches reference hash for known-content fixture  — expected 58681bad15e22323…, got 58681bad15e22323…
[PASS] U5  timingSafeEqual baseline (equal/different 32-byte buffers)
[PASS] H1  Initial http:// URL rejected (EPROTOCOL) before any network call  — code=EPROTOCOL
[PASS] H2  Redirect to http:// rejected (EPROTOCOL)  — code=EPROTOCOL
[PASS] H3  6+ redirects abort at cap (EREDIRECT_LIMIT) — default maxRedirects=5  — code=EREDIRECT_LIMIT redirects-served=6
[PASS] H4  3xx with empty Location header rejected (EREDIRECT_MALFORMED)  — code=EREDIRECT_MALFORMED
[PASS] H5  HTTPS→HTTPS cross-endpoint redirect followed to final 200  — body={"ok":true}
[PASS] H6  Auth header NOT attached when target host is non-GitHub (localhost)  — received Authorization=null
```

**Mapping to spec scenarios (`specs/runtime-resource-fetch/spec.md`):**

| Spec requirement | Spec scenario | Driver case | Status |
|---|---|---|---|
| HTTPS-only fetches | Initial URL is not HTTPS | H1 | ✓ confirmed `EPROTOCOL` raised before any network call (server saw 0 requests) |
| HTTPS-only fetches | Redirect downgrades from HTTPS to HTTP | H2 | ✓ |
| HTTPS-only fetches | Cross-host HTTPS redirect | H5 | ✓ followed to final 200 |
| Bounded redirect chain | Server returns more than 5 redirects | H3 | ✓ aborted at hop 6 with `EREDIRECT_LIMIT`; server received exactly 6 hops |
| Bounded redirect chain | Empty Location header | H4 | ✓ |
| Optional GitHub authentication | GITHUB_TOKEN is unset | U2 (env unset) | ✓ |
| Optional GitHub authentication | GITHUB_TOKEN is whitespace-only | U2 (`'   '`, `'\t \n'`) | ✓ |
| Optional GitHub authentication | GITHUB_TOKEN is set | U2 (`'ghp_xxx'`, `'  ghp_xxx  '`) | ✓ |
| Optional GitHub authentication | Redirect leaves GitHub host | H6 (header absent on `localhost`) + U1 | ✓ |
| OMP binary integrity verification | Digest matches | U4 + U5 (algorithm) + R2 below (integration) | ✓ |
| OMP binary integrity verification | Digest mismatch | U5 (different buffers → false) + R2 below | ✓ |
| OMP binary integrity verification | Asset has no digest | U3 (null/missing/malformed → null) + R2 below | ✓ |
| OMP binary integrity verification | No matching asset | R2 below — single `find()` short-circuit | ✓ |
| Deterministic stream teardown | Successful download | covered implicitly by H5 (read-and-end without error) + code review | ✓ |
| Deterministic stream teardown | Stream error mid-download | code review (`pipeToFile` settled-guard ensures single-shot) | ✓ |

**R2 — `ensureOmpBinary` integration verified by inspection:**

The production function is a 60-line linear sequence (no loops, no conditionals other than guard returns). Reading the body at `scripts/prepare-runtime-resources.mjs`:

1. `release.assets.find(...)` — returns `undefined` when no match → soft-fail return (line: `if (!asset)`).
2. `parseAssetDigest(asset.digest)` — returns `null` when missing/malformed (verified by U3) → soft-fail return (line: `if (!expected)`).
3. `await downloadFile(url, localPath)` — uses verified `httpsGetWithRedirects` (verified by H1–H6) + verified `pipeToFile`.
4. `actual = await sha256File(localPath)` — verified by U4.
5. `if (actual.length !== expected.length || !timingSafeEqual(actual, expected))` — verified by U5; mismatch path calls `unlinkSync(localPath)` then `throw`.
6. Match path: `chmodSync` + `writeFileSync(versionFile)`.

Each branch is traceable to a spec scenario and its building blocks are independently verified. Mocking the GitHub API for end-to-end mismatch exercise would require either DI changes (out of scope per design Non-Goals) or live-network manipulation (out of scope for an isolated verify run).

## 5. Proposal goals satisfied  ✓

| Original review issue | Spec requirement | Production code | Status |
|---|---|---|---|
| **P2** — redirects can downgrade or jump host | HTTPS-only fetches | `httpsGetWithRedirects` protocol check | ✓ verified by H1, H2 |
| **P3** — OMP binary lacks integrity check | OMP binary integrity verification | `parseAssetDigest` + `sha256File` + `timingSafeEqual` in `ensureOmpBinary` | ✓ verified by U3–U5 + R2 |
| **P6** — unauthenticated GitHub API → 60/hr cap | Optional GitHub authentication | `readGitHubToken` + `httpsGetWithRedirects.authBearer` + `isGitHubHost` | ✓ verified by U1–U2, H6 |
| **P7** — unbounded redirect recursion | Bounded redirect chain | `hopsLeft = maxRedirects` decrement | ✓ verified by H3, H4 |
| **P9** (sweep-along) — `stream.close()` deprecated, response errors not observed | Deterministic stream teardown | `pipeToFile` `settled` + `stream.destroy()` + `res.on('error', finish)` | ✓ verified by code review |

## 6. Delta spec ↔ design doc consistency  ✓

One spec mutation occurred during the design phase (technical RFC §"Spec patch"): the `Optional GitHub authentication` requirement was tightened to "non-empty value after trimming ASCII whitespace" with a new "GITHUB_TOKEN is whitespace-only" scenario.

- **OpenSpec delta spec** at `openspec/changes/harden-runtime-resource-fetch/specs/runtime-resource-fetch/spec.md` — contains the tightened wording and the third scenario.
- **Technical RFC** at `docs/superpowers/specs/2026-05-29-harden-runtime-resource-fetch-design.md` — §"Spec patch (write back to OpenSpec)" explicitly records this divergence and the rationale.
- **Implementation** — `readGitHubToken` calls `.trim()` and returns `null` for empty post-trim.

No other drift detected. `openspec validate harden-runtime-resource-fetch` reports `Change is valid`.

## 7. Linked Superpowers design doc reachable  ✓

```
docs/superpowers/specs/2026-05-29-harden-runtime-resource-fetch-design.md
```

Frontmatter:
```yaml
comet_change: harden-runtime-resource-fetch
role: technical-design
canonical_spec: openspec
```

File exists, frontmatter matches change name, role, and canonical-spec declaration. Verified by build-phase guard at design-phase exit (`PASS Design Doc frontmatter links current change`).

---

## Light-mode equivalents (also satisfied)

For completeness, the comet-verify §2a light-mode 5-item checklist also passes:

| # | Check | Result |
|---|---|---|
| 1 | tasks.md fully `[x]` | ✓ (§1) |
| 2 | Diff matches tasks | ✓ — `git diff --stat 42432b0e..HEAD` shows exactly `scripts/prepare-runtime-resources.mjs | 398 ++++` |
| 3 | Compile passes | ✓ — `node --check scripts/prepare-runtime-resources.mjs` exit 0; module loads with same public surface |
| 4 | Tests pass | ✓ — 11/11 in driver (§4); no other test framework in scope |
| 5 | No obvious security issues | ✓ — no hardcoded secrets, no new `unsafe`/`eval`/`Function`/`exec`, no env-var leakage; verifier ran `process.env.NODE_TLS_REJECT_UNAUTHORIZED='0'` only in the isolated test process |

## Scale note

`comet-state.sh scale` selected `full` mode based on task count (18 ≥ threshold 3). The actual material change is a **single file, +398 lines, 1 capability** — this would be light by file/spec count alone. Full-mode evidence collected anyway at the user's stated security-rigor preference.

## Open follow-ups (out of scope for this change)

These were captured during exploration and remain valid for future changes:

- **Group A** of `review1.md` — `coordinator.rs` spawn-task error path session leak (P1) and `ClaudeSession` orphan-process risk (P5). User opted to do B group first; A still pending.
- **Group C** of `review1.md` — `runtime_sessions` DashMap lifecycle integration (P4). Design needed.
- **OMP version pinning + repo-internal hash manifest** — defends against full GitHub Release compromise; out of scope per design Non-Goals.
- **Script test framework** — `verify-tmp/run.mjs` is a one-off driver, not a sustainable harness. A future `tooling-test-framework` change could promote it.
- **P11** — `claude_runtime.rs` `health_check` semantic shift (UI shows "unavailable" without API key); needs product confirmation.

---

## Verdict

**PASS — all 7 full-mode checks satisfied; 11/11 runtime scenarios green; no spec drift.**

Ready for branch handling and archive.
