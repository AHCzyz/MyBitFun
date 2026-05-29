# Build smoke (Plan Task 3)

**Branch:** `harden-runtime-resource-fetch`
**Commits on branch:**
- `4883f414` harden(scripts): centralize HTTPS fetch with protocol/redirect/auth controls
- `97f50a69` harden(scripts): verify OMP binary SHA-256 against GitHub release digest
**Date:** 2026-05-29

## Static checks

| Check | Command | Result |
|---|---|---|
| Syntax (Task 1) | `node --check scripts/prepare-runtime-resources.mjs` | PASS (exit 0) |
| Syntax (Task 2) | `node --check scripts/prepare-runtime-resources.mjs` | PASS (exit 0) |
| Module loads + exports unchanged | `node -e "import('./scripts/prepare-runtime-resources.mjs').then(m => console.log(Object.keys(m).join(',')))"` | PASS — exports `ensureClaudeBridge, ensureOmpBinary, prepareRuntimeResources` (same public surface as before) |
| Downstream importer | `grep "from.*prepare-runtime-resources" scripts/` | only `desktop-tauri-build.mjs` imports `prepareRuntimeResources`, still exported — no downstream breakage |

## Happy-path script run

**Command:** `node scripts/prepare-runtime-resources.mjs`
**Result:** PASS (exit 0)
**Output:**
```
[runtime-resources] claude-bridge/node_modules exists, skipping install.
[runtime-resources] Node.js binary already bundled: node.exe
[runtime-resources] OMP binary already present (v15.5.10): omp.exe
```
**`.omp-version` contents:** `v15.5.10`

## ⚠ Coverage gap on this run (intentional, deferred to verify)

Local resource cache is fully populated:
- `resources/claude-bridge/node_modules/` — present
- `resources/claude-bridge/node.exe` — present
- `resources/omp/omp.exe` — present
- `resources/omp/.omp-version` — `v15.5.10`

This run therefore short-circuits at the existing `existsSync(localPath)` early returns and **does not exercise any of the new code paths** introduced in this change:

| New code | Exercised? |
|---|---|
| `httpsGetWithRedirects` (any path) | ✗ no — no network call made |
| `isGitHubHost` host whitelist | ✗ no |
| `readGitHubToken` | ✗ no |
| `pipeToFile` finish/error/destroy | ✗ no |
| `parseAssetDigest` | ✗ no |
| `sha256File` | ✗ no |
| `timingSafeEqual` digest comparison | ✗ no |
| `ensureOmpBinary` digest check branches | ✗ no |

The smoke run only confirms that **the module parses, loads, exports the same public surface, and still degrades correctly when caches are warm** — i.e. the change has not regressed the cache-hit path.

## Negative & full-network paths → /comet-verify

The verify phase will exercise the negative scenarios listed in `tasks.md §6` against an explicitly-prepared environment. None of these are run here:

| § | Scenario | Owner |
|---|---|---|
| 6.1 | Fresh download, no token — happy GitHub API + redirect + digest verification | verify |
| 6.2 | With `GITHUB_TOKEN` — auth header attached on `api.github.com` and `objects.githubusercontent.com` hops | verify |
| 6.3 | HTTP redirect rejection (EPROTOCOL) | verify |
| 6.4 | 6+ HTTPS redirects — abort at hop 5 (EREDIRECT_LIMIT) | verify |
| 6.5 | Forced hash mismatch — hard-fail, file deleted, exit non-zero | verify |
| 6.6 | Forced missing-digest — soft-fail, claude-bridge still completes | verify |

Build phase exit criteria are met: code compiles, public surface is preserved, the script runs to completion on the warm-cache path. Proceeding to build guard.
