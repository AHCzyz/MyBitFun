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
- **THEN** the stream SHALL be destroyed and the promise SHALL reject exactly once, with no subsequent `finish` callback resolving it
