## Context

`scripts/prepare-runtime-resources.mjs` runs at desktop build time. Today it:

1. Calls `npm install` for the bundled `claude-bridge` package (npm handles its own integrity via `package-lock.json`'s SHA-512 lockfile entries).
2. Hits `https://api.github.com/repos/can1357/oh-my-pi/releases/latest` (unauthenticated) to discover the latest OMP binary, then downloads it from `github.com/.../releases/download/<tag>/<asset>` and `chmod +x`s it.

Both the API call and the binary download share two ad-hoc helpers (`fetchJson`, `downloadFile`). Each helper has its own redirect-following loop that calls itself recursively whenever it sees a 3xx response with a `Location` header — no protocol check, no hop counter, no body integrity check on the downloaded file.

A security review (B group of `review1.md`) scoped four issues to this file:

- **P2** — redirects can downgrade `https:` → `http:` or jump to an attacker-controlled host.
- **P3** — the OMP binary is installed without any integrity verification.
- **P6** — unauthenticated GitHub API calls share the 60/hour rate-limit bucket; CI builds break.
- **P7** — recursion is unbounded and can be driven into stack overflow.

P9 (deprecated `stream.close()`) is bundled in here because it lives in the same `downloadFile` body — the cost of fixing it separately is higher than fixing it together.

## Goals / Non-Goals

**Goals:**
- Eliminate any path that lets a network attacker substitute the OMP binary or downgrade the connection.
- Keep the OMP "warn and skip" degradation contract intact: a missing or broken release MUST NOT break the desktop build of unrelated runtimes (claude-bridge, flashgrep). Only a *positive* integrity failure (hash mismatch) is allowed to hard-fail.
- Make CI builds with `GITHUB_TOKEN` automatically lift their rate-limit ceiling, with no caller-side change.
- Centralize the redirect/protocol/cap logic so future build scripts that fetch remote assets can adopt the same helper.

**Non-Goals:**
- Pinning OMP to a fixed version (still using `releases/latest` — version locking is a separate proposal).
- Maintaining an out-of-band hash manifest in this repo (defends against full GitHub compromise; out of scope).
- Mirroring OMP to self-hosted storage.
- Switching `npm install` to `npm ci` for the claude-bridge path.
- Changing any Rust code, Tauri build, or runtime behaviour at app start.
- Adding a script test framework — there is none today, and standing one up exceeds this change. Verification is via manual exercise of the script (see Migration Plan).

## Decisions

### D1. Protocol enforcement is whitelist + per-redirect

The fetcher rejects anything that is not `https:`, applied at three points:
1. The initial URL passed in by the caller.
2. Every `Location` header before issuing the redirected request.
3. After resolving relative `Location` values against the current URL (using `new URL(location, currentUrl)`).

**Why not "downgrade only"?** Allowing `https:` → `https:` redirects across hosts is required (GitHub's release download path goes `api.github.com` → `objects.githubusercontent.com`). A literal "no downgrade" check (current proto vs next proto) would be equivalent to the whitelist here because the only protocol we ever start with is `https:`. The whitelist is simpler to reason about and survives a future caller that accidentally passes an `http:` base URL.

**Alternative considered**: maintain a host allowlist. Rejected — overfits to GitHub today and would need maintenance every time a new download host appears. HTTPS-only is sufficient given the threat model (we trust TLS + downstream integrity check).

### D2. Redirect cap is a counter passed through the chain

Implemented as an integer parameter that decrements on each recursive call; at zero, throw. Cap is 5 (matches `fetch` spec, `curl --max-redirs` default behaviour).

**Alternative**: use a `Set` of seen URLs and reject loops. Rejected — strictly weaker (a server emitting a fresh URL each hop would still loop indefinitely) and more code.

### D3. SHA-256 source is `release.assets[].digest` from the same `releases/latest` response

GitHub's Releases API has populated `assets[].digest` (format `sha256:<hex>`) since early 2024 for any asset uploaded through the standard Release publishing flow. `oh-my-pi` uses GitHub Actions' release workflow, so its assets carry the field today.

The verification path:
1. Fetch `releases/latest` (already happening).
2. Find the asset whose `name == remoteName` (already happening, implicitly — we currently build the URL by string concatenation, but we need the asset entry itself to read the digest, so we look it up by name).
3. Parse `assets[].digest`, expecting `sha256:<64-hex>`. If absent or malformed → see D4.
4. Download the asset.
5. Stream the downloaded file through `crypto.createHash('sha256')`. Compare to the parsed digest using a constant-time comparison (`crypto.timingSafeEqual` on equal-length buffers).
6. On mismatch → delete file, throw. On match → proceed to chmod and version-stamp as before.

**Why constant-time?** Defense in depth. The comparison runs locally and the attacker doesn't time it, so the practical risk is zero. But `timingSafeEqual` is a one-liner and removes a class of trivial-to-introduce bugs.

**Why not download first, then look up digest?** We already need the release JSON to know the tag and build the asset URL. Reading `digest` from the same response costs nothing.

**Alternative considered**: separate `.sha256` sidecar download. Rejected — `oh-my-pi` doesn't publish one, and adding a second remote dependency expands the attack surface.

### D4. Missing/malformed `digest` is a soft-fail, mismatch is a hard-fail

The two failure modes are fundamentally different:

| Outcome | Diagnosis | Action |
|---|---|---|
| Asset has no `digest` field | Old release, manual upload, or upstream regression | **Soft-fail**: warn, delete partial file, skip OMP |
| Asset has malformed `digest` (not `sha256:<hex>`) | Possibly tampered API response, or a future format we don't understand | **Soft-fail** with a distinct warning pointing at the value |
| Computed hash != declared digest | Download corruption or active attack | **Hard-fail**: delete file, throw |

The hard-fail is justified because a hash mismatch is the *only* signal that the rest of this script's defenses (HTTPS, redirect cap, GitHub TLS) have been bypassed. Letting the build continue with a tampered binary defeats the purpose of the whole change.

The soft-fail aligns with the script's existing "OMP runtime will not be available in this build" contract on download or API failures — the desktop build is not blocked, OMP is simply absent at runtime. This contract is what lets us land integrity verification without coordinating with the OMP release process: even on the bad-luck day that an OMP release ships without a digest, the build still produces a working desktop with claude-bridge intact.

### D5. `GITHUB_TOKEN` is read once at the top of the request and forwarded only to GitHub hosts

Read `process.env.GITHUB_TOKEN` once when starting a fetch. If non-empty, pass the token alongside the helper's URL state. The helper attaches `Authorization: Bearer <token>` to every hop whose host ends in `.github.com` or is `api.github.com` or `github.com` or `objects.githubusercontent.com`. On a redirect to any other host, the token is dropped before issuing the redirected request.

**Why drop on redirect off-host?** A misbehaving server (or attacker who's been able to inject a `Location` header but not break TLS to GitHub) could otherwise exfiltrate the token. The redirect is followed (we still want to download the asset, just unauthenticated), but the credential isn't.

**Alternative considered**: send `Authorization` only on the initial request. Rejected — GitHub redirects asset downloads to `objects.githubusercontent.com`, which is also a GitHub host where the token is safe and where rate limits matter for large CI fleets.

**Why a hard-coded host list?** Three entries (`*.github.com`, `*.githubusercontent.com`, exact `github.com`) cover all observed GitHub redirect targets. If GitHub adds a fourth, the worst case is unauthenticated requests work as today.

### D6. Single shared helper, two thin wrappers

Replace both `fetchJson` and `downloadFile` recursion bodies with one helper:

```
httpsGetWithRedirects(url, { headers, maxRedirects = 5 })
  → returns Promise<IncomingMessage> with the final 200 response
```

Wrappers:
- `fetchJson(url)` consumes the response into a string and `JSON.parse`s it.
- `downloadFile(url, dest)` pipes the response into a `WriteStream` (with `destroy()` teardown — D7).

This is the only place that knows about protocol checks, redirect counting, or auth-header forwarding. It's also where the GitHub host check lives.

**Why not keep two parallel implementations?** The two existing helpers have already drifted (one calls `res.resume()` on redirects, the other doesn't; one has the `User-Agent` header, the other lacks it). Future drift is exactly what we want to avoid for security-critical code.

### D7. Stream teardown uses `destroy()` on both finish and error

```
stream.on('finish', () => { stream.destroy(); resolve(); });
stream.on('error', (err) => { stream.destroy(); reject(err); });
```

Replaces the existing `stream.close()` (deprecated) and the missing destroy on error. Both promises are still single-shot because Node guarantees `resolve`/`reject` are no-ops after the first call.

## Risks / Trade-offs

- **OMP release without `digest` field** → soft-fail skips the runtime. *Mitigation*: warning is loud and unique-to-this-cause; users with the older release pinned can either upgrade upstream or (a future change) introduce version locking + a manifest. Accepted because it cleanly preserves the existing degradation path.
- **`GITHUB_TOKEN` leaked through a redirect** → mitigated by dropping the header when redirecting off GitHub hosts. Residual risk: the four-host whitelist is hard-coded. *Mitigation*: if GitHub introduces a new download host, requests from that host fall back to unauthenticated, which is still safe.
- **`crypto.timingSafeEqual` requires equal-length buffers** → if `digest` parses to a non-32-byte buffer (e.g. corrupted hex), comparison would throw. *Mitigation*: validate format `^sha256:[0-9a-f]{64}$` before comparison; treat anything else as soft-fail per D4.
- **Whole-file hashing of a few-MB binary** → ~tens of ms overhead. Negligible against the network download.
- **No automated test for the build script** → relying on manual exercise. *Mitigation*: design the helper as a pure function (URL + options → Promise) so it can be moved into a test harness later without further refactoring; cover the soft-fail paths by exercising them in the migration plan.

## Migration Plan

This is a build-script-only change. No deploy, no rollback gating.

Verification steps (run locally before merge):

1. **Clean run, network OK, no token** → script completes; `resources/omp/omp[.exe]` is present; `.omp-version` is written. Confirms happy path still works.
2. **Clean run with `GITHUB_TOKEN` set** → same outcome; verify the token took effect via `gh api -i rate_limit` before/after (authenticated bucket).
3. **Force missing digest** → temporarily edit the helper to drop the digest before comparison; confirm soft-fail message and that the desktop build still completes without OMP.
4. **Force hash mismatch** → temporarily edit the helper to compare against a wrong hash; confirm hard-fail, file deleted, exit code non-zero.
5. **Force HTTP redirect** → run a tiny local server that returns `302 Location: http://...` and point the script at it; confirm rejection before the second request goes out.
6. **Force redirect loop** → same test rig, return a fresh `https:` location each time; confirm abort at hop 5.

Rollback: revert the single file. No state stored.

## Open Questions

None blocking. Items deferred to follow-up changes (out of scope here):

- Version-locking OMP and maintaining a checked-in hash manifest (defends against full GitHub compromise).
- Mirroring OMP to self-hosted storage with signed URLs.
- Bundling a script test framework so the helper can have unit tests.
