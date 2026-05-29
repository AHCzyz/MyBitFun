## Why

A security review (`review1.md`, group B) flagged four issues in `scripts/prepare-runtime-resources.mjs` that affect the integrity and reliability of the desktop build:

- **P2** — `fetchJson` / `downloadFile` blindly follow 3xx `Location` headers without checking the protocol, so an HTTPS request can silently downgrade to HTTP and the asset URL can be redirected to an arbitrary host (CWE-918, CWE-601).
- **P3** — The OMP binary is downloaded from GitHub Releases and made executable without any integrity verification, so a CDN-layer or MITM attacker can replace the payload (CWE-494, OWASP A08).
- **P6** — The GitHub API call to `releases/latest` is unauthenticated and capped at 60 requests/hour. Shared CI IPs trigger this rate limit and break builds.
- **P7** — Redirects are followed recursively with no upper bound, so a misbehaving server (or attacker) can drive the script into runaway recursion.

These flow through the build path that ships `claude-bridge` and the `omp` runtime, so an undetected compromise here distributes a tampered binary to every installer and CI artifact.

## What Changes

- Reject any redirect whose resolved URL is not `https://` and reject any non-HTTPS initial URL.
- Cap the redirect chain at 5 hops; reject empty or malformed `Location` values.
- When fetching `releases/latest`, attach `Authorization: Bearer $GITHUB_TOKEN` if the env var is set; otherwise proceed unauthenticated as today.
- Compute SHA-256 of the downloaded OMP binary and compare against the `digest` field on the matching `release.assets[]` entry. On mismatch: delete the file and abort. When `digest` is absent: warn, delete the file, skip OMP (matches existing degradation path).
- Replace `stream.close()` with `stream.destroy()` in `downloadFile`'s finish/error handlers (P9 sweep-along — same file, zero extra cost).
- Refactor `fetchJson` and `downloadFile` to share a single `httpsGetWithRedirects` helper so the protocol/cap/redirect rules live in one place.

No public API changes. No Rust changes. No package.json changes.

## Capabilities

### New Capabilities
- `runtime-resource-fetch`: Build-time HTTP fetching and integrity verification used by `scripts/prepare-runtime-resources.mjs` (and any future build scripts that need to download remote runtime assets).

### Modified Capabilities
_None — no existing spec covers this build script._

## Impact

- **Code**: `scripts/prepare-runtime-resources.mjs` only.
- **Dependencies**: none added (uses Node built-in `crypto`, `https`, `url`, `fs`).
- **Build behaviour**:
  - Builds with `GITHUB_TOKEN` set will silently get higher rate-limit headroom.
  - Builds against an OMP release whose asset has no `digest` field will skip OMP (with a warning) instead of installing it. Current upstream `oh-my-pi` releases include `digest`, so this should be a no-op in practice.
  - A tampered OMP download will hard-fail the script instead of being silently chmod+x'd.
- **Threat model coverage**: addresses MITM, transparent CDN compromise, and HTTP downgrade. Does **not** address full GitHub API compromise (would require a separately maintained hash manifest — out of scope).
