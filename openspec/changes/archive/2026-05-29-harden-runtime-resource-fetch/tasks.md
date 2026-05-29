## 1. Shared HTTPS helper (P2 + P7)

- [x] 1.1 Add `httpsGetWithRedirects(url, { headers, maxRedirects, allowAuthHosts, authHeader })` to `scripts/prepare-runtime-resources.mjs`, returning a `Promise<IncomingMessage>` resolving with the final 200 response
- [x] 1.2 Reject any URL whose `protocol !== 'https:'` (initial URL and every resolved `Location`); resolve relative `Location` against the current URL via `new URL(location, current)`
- [x] 1.3 Reject empty / missing `Location` on a 3xx response with a `malformed-redirect` error
- [x] 1.4 Decrement a redirect counter on each hop; throw a `redirect-limit` error when the budget hits zero. Default `maxRedirects = 5`
- [x] 1.5 Forward `User-Agent: BitFun-Build-Script` on every hop (currently only set on the first call in `fetchJson`)

## 2. Optional GitHub authentication (P6)

- [x] 2.1 Read `process.env.GITHUB_TOKEN` once at the top of `prepareRuntimeResources` (or per-call); pass `authHeader: \`Bearer ${token}\`` and `allowAuthHosts` (the GitHub host list) into `httpsGetWithRedirects` only when the token is non-empty
- [x] 2.2 In the helper, attach the auth header only when the request URL's host matches the allowlist (`github.com`, `*.github.com`, `api.github.com`, `*.githubusercontent.com`); strip it on any redirect hop whose host falls outside the allowlist
- [x] 2.3 Confirm `fetchJson` and `downloadFile` both go through this code path (so OMP asset downloads also get the higher rate-limit headroom)

## 3. Refactor existing helpers onto the new core

- [x] 3.1 Reimplement `fetchJson` as a thin wrapper that calls `httpsGetWithRedirects` and consumes the response body into a JSON string
- [x] 3.2 Reimplement `downloadFile` as a thin wrapper that pipes the response into a `WriteStream`
- [x] 3.3 Delete the old recursive `doFetch` / `doDownload` inner functions

## 4. OMP integrity verification (P3)

- [x] 4.1 Change `ensureOmpBinary` to look up the matching asset entry by `assets[].name === target.remoteName` and capture the `digest` field alongside the download URL
- [x] 4.2 Validate `digest` matches `/^sha256:[0-9a-f]{64}$/`. On no-asset / no-digest / malformed-digest: log a distinct warning identifying the cause, delete any partial file, and skip OMP installation (preserve current "OMP runtime will not be available" contract)
- [x] 4.3 After `downloadFile` resolves, stream the local file through `crypto.createHash('sha256')` and compare to the parsed digest with `crypto.timingSafeEqual`
- [x] 4.4 On hash mismatch: delete the downloaded file and throw an `Error` with both expected and actual hashes; do not write `.omp-version`. The thrown error propagates from `prepareRuntimeResources` and exits the script non-zero (current top-level catch handles this)
- [x] 4.5 On hash match: proceed to existing chmod + version-stamp path

## 5. Stream teardown (P9 sweep-along)

- [x] 5.1 Replace `stream.on('finish', () => { stream.close(); resolve(); })` with `stream.destroy()` + resolve
- [x] 5.2 Replace `stream.on('error', reject)` with a handler that calls `stream.destroy()` then `reject(err)`

## 6. Manual verification

> Owned by the **/comet-verify** phase, not the build phase. Build phase produces the implementation; verify phase exercises every spec scenario against the running script and emits the verification report. The scenario list is the canonical one in `specs/runtime-resource-fetch/spec.md` (Requirements: HTTPS-only fetches, Bounded redirect chain, Optional GitHub authentication, OMP binary integrity verification, Deterministic stream teardown). See `.comet/handoff/build-smoke.md` for the build-phase pre-image and `verification-report.md` (written during /comet-verify) for the executed evidence.
