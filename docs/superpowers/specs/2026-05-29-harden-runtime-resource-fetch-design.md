---
comet_change: harden-runtime-resource-fetch
role: technical-design
canonical_spec: openspec
archived-with: 2026-05-29-harden-runtime-resource-fetch
status: final
---

# Hardened runtime-resource fetch — technical design

## Scope

Code-level RFC for the changes specified in
`openspec/changes/harden-runtime-resource-fetch/`. Architectural decisions
(D1–D7) and threat model already live in `openspec/changes/.../design.md` —
not duplicated here. This document fixes the implementation surface so the
build phase has nothing left to invent.

## Module shape after the change

`scripts/prepare-runtime-resources.mjs` keeps its existing public surface
(`ensureClaudeBridge`, `ensureOmpBinary`, `prepareRuntimeResources`).
Internal layout becomes:

```
prepare-runtime-resources.mjs
├── isGitHubHost(hostname)            // pure
├── readGitHubToken()                 // env → string|null, trim'd
├── parseAssetDigest(value)           // 'sha256:<hex>' → Buffer|null
├── sha256File(path)                  // streams file → Buffer
├── httpsGetWithRedirects(url, opts)  // ★ shared core, returns IncomingMessage
├── fetchJson(url)                    // wrapper: consumes body → JSON
├── downloadFile(url, dest)           // wrapper: pipes body → WriteStream
├── ensureClaudeBridge()              // unchanged behaviour
├── ensureOmpBinary()                 // gains digest verification
└── prepareRuntimeResources()         // unchanged
```

The recursive `doFetch` / `doDownload` inner functions are deleted.

## `httpsGetWithRedirects` — the only place that talks to the network

### Signature

```js
/**
 * @param {string} url
 * @param {object} [opts]
 * @param {Record<string,string>} [opts.headers]   forwarded on every hop
 * @param {number}                [opts.maxRedirects=5]
 * @param {string|null}           [opts.authBearer=null]   token, or null to skip
 * @returns {Promise<import('http').IncomingMessage>} resolves with the final 200 response
 */
function httpsGetWithRedirects(url, opts = {}) { ... }
```

The promise resolves with the live `IncomingMessage` of the final 200 response.
Callers consume it (string body, or pipe to file). On any non-2xx final
response, or any rejected redirect, the promise rejects with a typed `Error`
whose `.code` is one of:

| `.code` | when |
|---|---|
| `EPROTOCOL` | initial URL or any `Location` is not `https:` |
| `EREDIRECT_LIMIT` | hop counter hit zero |
| `EREDIRECT_MALFORMED` | 3xx with empty / unparseable `Location` |
| `EHTTP` | non-2xx, non-3xx final response (existing behaviour, code added) |

### Per-hop control flow

```
loop with hopsLeft = maxRedirects:
  parsed = new URL(currentUrl)
  if parsed.protocol !== 'https:'  → reject EPROTOCOL
  effectiveHeaders = { ...headers }
  if authBearer && isGitHubHost(parsed.hostname):
    effectiveHeaders.Authorization = `Bearer ${authBearer}`
  res = await https.get(parsed, { headers: effectiveHeaders })
  if 200 <= status < 300:
    resolve(res); return
  if 300 <= status < 400 and Location header:
    res.resume()  // drain to free socket
    if hopsLeft <= 0  → reject EREDIRECT_LIMIT
    nextRaw = res.headers.location
    if !nextRaw or typeof !== 'string'  → reject EREDIRECT_MALFORMED
    try nextUrl = new URL(nextRaw, currentUrl)
    catch                              → reject EREDIRECT_MALFORMED
    currentUrl = nextUrl.toString()
    hopsLeft -= 1
    continue
  // any other status:
  drain body up to ~200 bytes for diagnostics
  reject EHTTP with status + snippet
```

Implemented as an iterative `while` loop, not recursion, so the redirect cap
also bounds stack depth trivially.

The `https.get` callback's `'error'` listener also rejects (network errors).

### `isGitHubHost`

```js
function isGitHubHost(hostname) {
  if (!hostname) return false;
  const h = hostname.toLowerCase();
  return (
    h === 'github.com' ||
    h === 'api.github.com' ||
    h.endsWith('.github.com') ||
    h === 'githubusercontent.com' ||
    h.endsWith('.githubusercontent.com')
  );
}
```

Whitelist is exact-match plus subdomain suffix; case-folded once at the top.
This is the only place the auth scope is enforced — wrappers don't see it.

### `readGitHubToken`

```js
function readGitHubToken() {
  const raw = process.env.GITHUB_TOKEN;
  if (typeof raw !== 'string') return null;
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : null;
}
```

Trim ASCII whitespace. Empty / whitespace-only → `null`. Caller passes the
value (or `null`) into `httpsGetWithRedirects` as `authBearer`.

This widens the spec's "non-empty" wording to "non-empty after trim". A
matching spec patch is captured in §"Spec patch" below.

## Wrappers

```js
async function fetchJson(url) {
  const res = await httpsGetWithRedirects(url, {
    headers: { 'User-Agent': 'BitFun-Build-Script' },
    authBearer: readGitHubToken(),
  });
  let data = '';
  for await (const chunk of res) data += chunk;
  return JSON.parse(data);
}

async function downloadFile(url, dest) {
  const res = await httpsGetWithRedirects(url, {
    headers: { 'User-Agent': 'BitFun-Build-Script' },
    authBearer: readGitHubToken(),
  });
  await pipeToFile(res, dest);
}
```

`pipeToFile` is a small inner helper (or inlined in `downloadFile`) that
handles stream teardown deterministically:

```js
function pipeToFile(res, dest) {
  return new Promise((resolve, reject) => {
    const stream = createWriteStream(dest);
    let settled = false;
    const finish = (err) => {
      if (settled) return;
      settled = true;
      stream.destroy();
      if (err) reject(err); else resolve();
    };
    stream.on('finish', () => finish());
    stream.on('error', finish);
    res.on('error', finish);    // ← closes a pre-existing leak: res errors weren't observed
    res.pipe(stream);
  });
}
```

Notes:
- `stream.destroy()` replaces the deprecated `stream.close()`.
- `res.on('error', ...)` is new: the old code ignored response-side errors so
  a connection-reset mid-download could leak the open WriteStream. Same file,
  zero extra cost — captured under tasks 5.x.
- `settled` guard makes the promise single-shot even if both `error` events
  fire.

## OMP integrity verification

### `parseAssetDigest`

```js
const SHA256_HEX = /^sha256:([0-9a-f]{64})$/;
function parseAssetDigest(value) {
  if (typeof value !== 'string') return null;
  const m = SHA256_HEX.exec(value.toLowerCase());
  return m ? Buffer.from(m[1], 'hex') : null;   // 32-byte Buffer
}
```

Returns `null` for missing / wrong-format / wrong-length inputs. Callers
treat `null` as soft-fail (D4 in canonical design).

### `sha256File`

```js
import { createReadStream } from 'fs';
import { createHash } from 'crypto';

function sha256File(path) {
  return new Promise((resolve, reject) => {
    const hash = createHash('sha256');
    createReadStream(path)
      .on('error', reject)
      .on('data', (chunk) => hash.update(chunk))
      .on('end', () => resolve(hash.digest()));   // 32-byte Buffer
  });
}
```

Two-pass design (download to disk, then re-read for hash). Justified in the
canonical design — at OMP's size (~few MB) the second pass is ~10 ms and the
control flow is much easier to reason about than a tee. The downloaded file
is the artifact we keep on success; if we kept a tee'd hash and never wrote
the file, we'd just be re-downloading on retry anyway.

### `ensureOmpBinary` flow

```
release = await fetchJson(releases/latest)       // already happens
asset = release.assets.find(a => a.name === target.remoteName)
if (!asset)                                      → warn 'no-matching-asset', return (skip OMP)
expected = parseAssetDigest(asset.digest)
if (!expected)                                   → warn 'no-or-malformed-digest', return (skip OMP)

mkdirSync(ompDir, { recursive: true })
await downloadFile(url, localPath)
let actual
try {
  actual = await sha256File(localPath)
} catch (e) {
  unlinkSync(localPath); throw e               // hash failure ≠ network failure; surface it
}

// equal-length by construction (both 32 bytes), but defend against parser bugs:
if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
  unlinkSync(localPath)
  throw new Error(`OMP integrity check failed: expected ${expected.toString('hex')}, got ${actual.toString('hex')}`)
}

if (process.platform !== 'win32') chmodSync(localPath, 0o755)
writeFileSync(versionFile, `${tag}\n`)
```

Behaviours mapped to spec scenarios:

| scenario | branch above |
|---|---|
| Digest matches | falls through to chmod + version-stamp |
| Digest mismatch | hard-fail throw, file deleted |
| No `digest` field / malformed | early return after `parseAssetDigest` returns null |
| No matching asset | early return after `find` returns undefined |

The hard-fail rethrows past `prepareRuntimeResources`; the existing
top-level `process.exit(1)` in the standalone-invocation branch propagates
the non-zero exit. When called from `desktop-tauri-build.mjs` the same throw
surfaces as a build failure — same shape as any other build script error.

## Spec patch (write back to OpenSpec)

The OpenSpec delta spec at
`openspec/changes/harden-runtime-resource-fetch/specs/runtime-resource-fetch/spec.md`
needs one tightening: replace "set to a non-empty value" with "set to a
non-empty value after trimming ASCII whitespace" in the
"Optional GitHub authentication" requirement. Whitespace-only `GITHUB_TOKEN`
otherwise becomes a malformed `Authorization: Bearer    ` header.

This is the only spec change discovered during technical design.

## Test strategy

No script-test framework is introduced (out of scope, per canonical
design's Non-Goals). Coverage is two-fold:

**Designed for testability.** Every helper is a pure function or a small
async function with no module-level mutable state. URLs and headers are
parameters, not closures. A future change can drop the file behind `nock`
fixtures or `undici` MockAgent without further refactoring.

**Manual verification before merge.** Six scenarios covered in tasks.md §6:
- happy path no-token
- happy path with `GITHUB_TOKEN`
- HTTP-redirect rejection
- redirect-limit rejection at hop 5
- forced hash mismatch (hard-fail)
- forced missing-digest (soft-fail, build still completes without OMP)

Acceptance evidence is captured in the verification report at the verify
phase.

## Edge cases & how they're handled

| Edge case | Handling |
|---|---|
| Relative `Location` (e.g. `/path`) | `new URL(loc, currentUrl)` resolves against current host; protocol on the resolved URL is checked (still rejects implicit downgrade) |
| `Location` to a different scheme like `ftp:` | Fails the protocol whitelist check |
| GitHub returns 200 directly with the JSON (no redirect) | Hop counter never decremented; works |
| Asset name collides with a different platform | `find` matches the first; `target.remoteName` is platform-qualified, so collision impossible in practice |
| `chmod` fails on the cached binary | Existing top-level error path (no change) |
| `timingSafeEqual` on length-mismatched buffers throws | Pre-checked with `.length` comparison; both should be 32 by construction, but defending against future parser changes |
| Hash computation throws (e.g. file gone) | Unlink + rethrow; build fails fast rather than installing an unverified binary |
| `process.env.GITHUB_TOKEN` set to literal `"undefined"` (string) | Trim is no-op, so non-empty → token sent. Not handled — caller-side mistake; same behaviour as any other env-var consumer |

## Risks not in canonical design

- **Two-pass hash on a slow disk** (e.g. spinning disk, network mount).
  Trade-off accepted: correctness > a few hundred ms on a build that already
  runs `npm install`.
- **`pipeToFile` settled-flag adds branching.** Tested implicitly by tasks 6.5
  (force hash mismatch — exercises rejection path) and 6.1 (happy path —
  exercises resolution path); error+finish race is unobservable but the
  guard makes it safe.

## Migration / rollback

Single-file change. Revert the file, no state to restore. `.omp-version` is
forward-compatible — old script reads the same format.
