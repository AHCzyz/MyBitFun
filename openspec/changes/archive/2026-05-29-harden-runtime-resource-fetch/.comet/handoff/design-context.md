# Comet Design Handoff

- Change: harden-runtime-resource-fetch
- Phase: design
- Mode: compact
- Context hash: 829466b2efa9fd39747ed8ec3b77722ae63453ad019b166925da40cd13e50af5

Generated-by: comet-handoff.sh

OpenSpec remains the canonical capability spec. This handoff is a deterministic, source-traceable context pack, not an agent-authored summary.

## openspec/changes/harden-runtime-resource-fetch/proposal.md

- Source: openspec/changes/harden-runtime-resource-fetch/proposal.md
- Lines: 1-39
- SHA256: 384569efdbe07b5914d6100f5c4f57708793be1a58230b3bc5a2e99b2ac64837

```md
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
```

## openspec/changes/harden-runtime-resource-fetch/design.md

- Source: openspec/changes/harden-runtime-resource-fetch/design.md
- Lines: 1-151
- SHA256: 552b44aa9512f34788b5aab45b6b962046c824888a582a9f8b69589a3c8d821c

[TRUNCATED]

```md
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
```

Full source: openspec/changes/harden-runtime-resource-fetch/design.md

## openspec/changes/harden-runtime-resource-fetch/tasks.md

- Source: openspec/changes/harden-runtime-resource-fetch/tasks.md
- Lines: 1-41
- SHA256: 286363db6a004a853e823e6b3ed9c6b8b3f4e6d7c040124f9cd73e873a49c397

```md
## 1. Shared HTTPS helper (P2 + P7)

- [ ] 1.1 Add `httpsGetWithRedirects(url, { headers, maxRedirects, allowAuthHosts, authHeader })` to `scripts/prepare-runtime-resources.mjs`, returning a `Promise<IncomingMessage>` resolving with the final 200 response
- [ ] 1.2 Reject any URL whose `protocol !== 'https:'` (initial URL and every resolved `Location`); resolve relative `Location` against the current URL via `new URL(location, current)`
- [ ] 1.3 Reject empty / missing `Location` on a 3xx response with a `malformed-redirect` error
- [ ] 1.4 Decrement a redirect counter on each hop; throw a `redirect-limit` error when the budget hits zero. Default `maxRedirects = 5`
- [ ] 1.5 Forward `User-Agent: BitFun-Build-Script` on every hop (currently only set on the first call in `fetchJson`)

## 2. Optional GitHub authentication (P6)

- [ ] 2.1 Read `process.env.GITHUB_TOKEN` once at the top of `prepareRuntimeResources` (or per-call); pass `authHeader: \`Bearer ${token}\`` and `allowAuthHosts` (the GitHub host list) into `httpsGetWithRedirects` only when the token is non-empty
- [ ] 2.2 In the helper, attach the auth header only when the request URL's host matches the allowlist (`github.com`, `*.github.com`, `api.github.com`, `*.githubusercontent.com`); strip it on any redirect hop whose host falls outside the allowlist
- [ ] 2.3 Confirm `fetchJson` and `downloadFile` both go through this code path (so OMP asset downloads also get the higher rate-limit headroom)

## 3. Refactor existing helpers onto the new core

- [ ] 3.1 Reimplement `fetchJson` as a thin wrapper that calls `httpsGetWithRedirects` and consumes the response body into a JSON string
- [ ] 3.2 Reimplement `downloadFile` as a thin wrapper that pipes the response into a `WriteStream`
- [ ] 3.3 Delete the old recursive `doFetch` / `doDownload` inner functions

## 4. OMP integrity verification (P3)

- [ ] 4.1 Change `ensureOmpBinary` to look up the matching asset entry by `assets[].name === target.remoteName` and capture the `digest` field alongside the download URL
- [ ] 4.2 Validate `digest` matches `/^sha256:[0-9a-f]{64}$/`. On no-asset / no-digest / malformed-digest: log a distinct warning identifying the cause, delete any partial file, and skip OMP installation (preserve current "OMP runtime will not be available" contract)
- [ ] 4.3 After `downloadFile` resolves, stream the local file through `crypto.createHash('sha256')` and compare to the parsed digest with `crypto.timingSafeEqual`
- [ ] 4.4 On hash mismatch: delete the downloaded file and throw an `Error` with both expected and actual hashes; do not write `.omp-version`. The thrown error propagates from `prepareRuntimeResources` and exits the script non-zero (current top-level catch handles this)
- [ ] 4.5 On hash match: proceed to existing chmod + version-stamp path

## 5. Stream teardown (P9 sweep-along)

- [ ] 5.1 Replace `stream.on('finish', () => { stream.close(); resolve(); })` with `stream.destroy()` + resolve
- [ ] 5.2 Replace `stream.on('error', reject)` with a handler that calls `stream.destroy()` then `reject(err)`

## 6. Manual verification

- [ ] 6.1 Run `pnpm setup:runtimes` on a clean checkout; confirm OMP downloads, hash matches, `.omp-version` is written
- [ ] 6.2 Run with `GITHUB_TOKEN` exported; confirm `gh api -i rate_limit` shows the authenticated bucket consumed
- [ ] 6.3 Stand up a local HTTPS test server (or use `node --experimental-...`) returning `302 Location: http://example.com/x` and point the helper at it; confirm rejection before any second request leaves
- [ ] 6.4 Same rig, return six fresh `https:` redirects; confirm abort at hop 5 with a `redirect-limit` error
- [ ] 6.5 Temporarily inject a wrong expected hash in `ensureOmpBinary`; confirm hard-fail, file is deleted, script exits non-zero
- [ ] 6.6 Temporarily strip the `digest` field after the API response; confirm soft-fail with the dedicated warning and the rest of the build (claude-bridge) still completes
```

## openspec/changes/harden-runtime-resource-fetch/specs/runtime-resource-fetch/spec.md

- Source: openspec/changes/harden-runtime-resource-fetch/specs/runtime-resource-fetch/spec.md
- Lines: 1-81
- SHA256: 0d9fe0103999f87dabd5ba18e55ac28527a06585302d58873e92499df3396eff

[TRUNCATED]

```md
## ADDED Requirements

### Requirement: HTTPS-only fetches

All HTTP requests issued by the runtime resource fetcher MUST use the `https:` protocol. The fetcher SHALL reject any initial URL whose protocol is not `https:` and SHALL refuse to follow redirects to non-`https:` targets.

#### Scenario: Initial URL is not HTTPS
- **WHEN** the fetcher is invoked with `http://example.com/release.json`
- **THEN** the fetch SHALL fail before issuing any network request, with an error identifying the rejected protocol

#### Scenario: Redirect downgrades from HTTPS to HTTP
- **WHEN** an `https://` request returns a 3xx response with `Location: http://other.example/payload`
- **THEN** the fetcher SHALL stop following the chain and report an error identifying the rejected redirect target

#### Scenario: Cross-host HTTPS redirect
- **WHEN** an `https://api.github.com/...` request returns a 3xx redirect to `https://objects.githubusercontent.com/...`
- **THEN** the fetcher SHALL follow the redirect (cross-host HTTPS redirects are allowed)

### Requirement: Bounded redirect chain

The fetcher MUST cap the number of redirect hops at 5 and MUST reject responses with empty or malformed `Location` headers.

#### Scenario: Server returns more than 5 redirects
- **WHEN** every response in the chain is a 302 with a fresh `https:` `Location`
- **THEN** the fetcher SHALL abort after the 5th redirect and report a redirect-limit error

#### Scenario: Empty Location header
- **WHEN** a 3xx response is returned with an empty or missing `Location` value
- **THEN** the fetcher SHALL fail with a malformed-redirect error rather than recursing on the same URL

### Requirement: Optional GitHub authentication

When the `GITHUB_TOKEN` environment variable is set to a non-empty value after trimming ASCII whitespace, the fetcher SHALL attach `Authorization: Bearer <trimmed-token>` to every request in the chain (including redirected hops to `*.github.com` / `api.github.com`). When the variable is unset, empty, or whitespace-only, no `Authorization` header is sent and the fetcher behaves as before.

#### Scenario: GITHUB_TOKEN is set
- **WHEN** `GITHUB_TOKEN=ghp_xxx` is exported and the fetcher requests `https://api.github.com/repos/.../releases/latest`
- **THEN** the request SHALL carry `Authorization: Bearer ghp_xxx` so that the higher authenticated rate limit applies

#### Scenario: GITHUB_TOKEN is unset
- **WHEN** `GITHUB_TOKEN` is not present in the environment
- **THEN** the request SHALL be issued without an `Authorization` header (no behaviour change versus the pre-hardening script)

#### Scenario: GITHUB_TOKEN is whitespace-only
- **WHEN** `GITHUB_TOKEN="   "` (only ASCII whitespace) is exported
- **THEN** the fetcher SHALL treat it as unset and SHALL NOT attach a malformed `Authorization: Bearer    ` header

#### Scenario: Redirect leaves GitHub host
- **WHEN** an authenticated request is redirected to a host that is not under `github.com` or `githubusercontent.com`
- **THEN** the fetcher SHALL drop the `Authorization` header before issuing the redirected request, to avoid leaking the token to third-party hosts

### Requirement: OMP binary integrity verification

Before installing a downloaded OMP binary, the fetcher MUST verify its SHA-256 against the `digest` field of the matching asset in the GitHub release metadata. The asset is matched by exact filename (`assets[].name == remoteName`).

#### Scenario: Digest matches
- **WHEN** the downloaded file's SHA-256, lower-case hex, equals the value parsed from `assets[].digest` (after stripping the `sha256:` prefix)
- **THEN** the file SHALL be marked executable (on non-Windows) and recorded in `.omp-version`

#### Scenario: Digest mismatch
- **WHEN** the downloaded file's SHA-256 differs from the asset's declared digest
- **THEN** the fetcher SHALL delete the downloaded file and abort the build with an error that includes both the expected and actual hashes

#### Scenario: Asset has no digest
- **WHEN** the matching asset entry has no `digest` field, or the field is not in the form `sha256:<hex>`
- **THEN** the fetcher SHALL delete the downloaded file, log a warning explaining that integrity verification was not possible, and skip OMP installation (matching the existing "OMP runtime will not be available" degradation path)

#### Scenario: No matching asset in release metadata
- **WHEN** no entry in `release.assets[]` matches the platform-specific `remoteName`
- **THEN** the fetcher SHALL not perform the download, log a warning, and skip OMP installation

### Requirement: Deterministic stream teardown

The download path MUST tear down its write stream using `stream.destroy()` rather than the deprecated `stream.close()` and MUST destroy the stream on both success and error.

#### Scenario: Successful download
- **WHEN** the response body finishes piping into the local file
- **THEN** the write stream SHALL be destroyed and the promise SHALL resolve exactly once

#### Scenario: Stream error mid-download
- **WHEN** the write stream emits `error`
```

Full source: openspec/changes/harden-runtime-resource-fetch/specs/runtime-resource-fetch/spec.md

