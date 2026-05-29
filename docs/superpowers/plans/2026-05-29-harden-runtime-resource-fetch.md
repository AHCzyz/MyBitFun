---
change: harden-runtime-resource-fetch
design-doc: docs/superpowers/specs/2026-05-29-harden-runtime-resource-fetch-design.md
base-ref: 42432b0ed14afbba9942bb0edb6a3bceaaac9544
archived-with: 2026-05-29-harden-runtime-resource-fetch
---

# Hardened runtime-resource fetch — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ad-hoc fetch helpers in `MyBitFun/scripts/prepare-runtime-resources.mjs` with a hardened core (HTTPS-only, redirect cap, optional GitHub auth, deterministic stream teardown), and verify OMP binary integrity via the GitHub release asset digest.

**Architecture:** Single-file change. Centralize HTTP I/O in `httpsGetWithRedirects`; keep `fetchJson` and `downloadFile` as thin wrappers. Add `parseAssetDigest` + `sha256File` and wire them into `ensureOmpBinary`. No new dependencies.

**Tech Stack:** Node ≥18 built-ins only — `https`, `crypto`, `fs`, `url`. No tests added.

**Note on tests:** No script-test framework exists in this repo. Per the design doc Non-Goals, this change does not introduce one. Verification is manual (run by the `/comet-verify` phase against six scenarios in `tasks.md §6`). Helpers are designed pure for future testability.

**Working directory:** `F:/Work/Mybitfun/MyBitFun/` (the inner git repo). Plan/spec docs live at `F:/Work/Mybitfun/` (outside the git repo).

archived-with: 2026-05-29-harden-runtime-resource-fetch
---

### Task 1: Hardened HTTPS helper layer + wrapper refactor

Covers tasks.md groups 1, 2, 3, 5. Replaces existing `fetchJson` / `downloadFile` recursion with a single hardened core. Stream teardown moves to `destroy()` and observes response-side errors.

**Files:**
- Modify: `MyBitFun/scripts/prepare-runtime-resources.mjs` (full rewrite of imports + helpers + wrappers; `ensureClaudeBridge`, `getOmpTarget`, `prepareRuntimeResources`, and the standalone-invocation tail untouched)

- [ ] **Step 1: Update imports**

Replace the current imports block (lines 12–16) with:

```js
import { spawnSync } from 'child_process';
import {
  chmodSync,
  copyFileSync,
  createReadStream,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { get as httpsGet } from 'https';
import { createHash, timingSafeEqual } from 'crypto';
```

Adds `createReadStream` (for `sha256File`), `createHash`, `timingSafeEqual` (for digest verification used in Task 2). Keeps existing `fs` imports so `ensureClaudeBridge` keeps working.

- [ ] **Step 2: Add module-level constants**

After `const OMP_REPO = 'can1357/oh-my-pi';` add:

```js
const SHA256_HEX = /^sha256:([0-9a-f]{64})$/;
const DEFAULT_MAX_REDIRECTS = 5;
const USER_AGENT = 'BitFun-Build-Script';
```

- [ ] **Step 3: Replace fetchJson + downloadFile with hardened core + wrappers**

Replace lines 107–154 (the entire `fetchJson` and `downloadFile` functions) with the hardened helpers below.

```js
// ── Hardened HTTPS core ──────────────────────────────────────────────────────

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

function readGitHubToken() {
  const raw = process.env.GITHUB_TOKEN;
  if (typeof raw !== 'string') return null;
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * Issue an HTTPS GET, following at most `maxRedirects` 3xx responses.
 *
 * Rejects any URL whose protocol is not https: (initial or redirected).
 * Forwards `Authorization: Bearer <token>` only on hops to GitHub-owned hosts;
 * the header is dropped when redirected to any other host.
 *
 * Resolves with the live IncomingMessage of the final 200 response.
 * Rejects with .code in {EPROTOCOL, EREDIRECT_LIMIT, EREDIRECT_MALFORMED, EHTTP}.
 *
 * @param {string} url
 * @param {{ headers?: Record<string,string>, maxRedirects?: number, authBearer?: string|null }} [opts]
 * @returns {Promise<import('http').IncomingMessage>}
 */
function httpsGetWithRedirects(url, opts = {}) {
  const headers = { ...(opts.headers || {}) };
  const authBearer = opts.authBearer ?? null;
  const maxRedirects = opts.maxRedirects ?? DEFAULT_MAX_REDIRECTS;
  let hopsLeft = maxRedirects;
  let currentUrl = url;

  return new Promise((resolve, reject) => {
    const fail = (code, message) => {
      const err = new Error(message);
      err.code = code;
      reject(err);
    };

    const step = () => {
      let parsed;
      try {
        parsed = new URL(currentUrl);
      } catch {
        fail('EREDIRECT_MALFORMED', `Invalid URL: ${currentUrl}`);
        return;
      }
      if (parsed.protocol !== 'https:') {
        fail('EPROTOCOL', `Refusing non-https URL: ${currentUrl}`);
        return;
      }

      const reqHeaders = { ...headers };
      if (authBearer && isGitHubHost(parsed.hostname)) {
        reqHeaders.Authorization = `Bearer ${authBearer}`;
      }

      const req = httpsGet(parsed, { headers: reqHeaders }, (res) => {
        const status = res.statusCode ?? 0;

        if (status >= 200 && status < 300) {
          resolve(res);
          return;
        }

        if (status >= 300 && status < 400) {
          res.resume();
          if (hopsLeft <= 0) {
            fail('EREDIRECT_LIMIT', `Exceeded ${maxRedirects} redirects starting from ${url}`);
            return;
          }
          const location = res.headers.location;
          if (typeof location !== 'string' || location.length === 0) {
            fail('EREDIRECT_MALFORMED', `3xx response from ${currentUrl} with empty Location`);
            return;
          }
          let next;
          try {
            next = new URL(location, currentUrl);
          } catch {
            fail('EREDIRECT_MALFORMED', `3xx response from ${currentUrl} with invalid Location: ${location}`);
            return;
          }
          currentUrl = next.toString();
          hopsLeft -= 1;
          step();
          return;
        }

        let body = '';
        res.on('data', (c) => {
          if (body.length < 200) body += c;
        });
        res.on('end', () =>
          fail('EHTTP', `HTTP ${status} from ${currentUrl}: ${body.slice(0, 200)}`)
        );
      });

      req.on('error', reject);
    };

    step();
  });
}

function pipeToFile(res, dest) {
  return new Promise((resolve, reject) => {
    const stream = createWriteStream(dest);
    let settled = false;
    const finish = (err) => {
      if (settled) return;
      settled = true;
      stream.destroy();
      if (err) reject(err);
      else resolve();
    };
    stream.on('finish', () => finish());
    stream.on('error', finish);
    res.on('error', finish);
    res.pipe(stream);
  });
}

// ── Wrappers ─────────────────────────────────────────────────────────────────

async function fetchJson(url) {
  const res = await httpsGetWithRedirects(url, {
    headers: { 'User-Agent': USER_AGENT },
    authBearer: readGitHubToken(),
  });
  let data = '';
  for await (const chunk of res) {
    data += chunk;
  }
  return JSON.parse(data);
}

async function downloadFile(url, dest) {
  const res = await httpsGetWithRedirects(url, {
    headers: { 'User-Agent': USER_AGENT },
    authBearer: readGitHubToken(),
  });
  await pipeToFile(res, dest);
}
```

- [ ] **Step 4: Syntax check**

Run from `F:/Work/Mybitfun/MyBitFun`:

```bash
node --check scripts/prepare-runtime-resources.mjs
```

Expected: no output, exit 0.

- [ ] **Step 5: Commit**

```bash
git add scripts/prepare-runtime-resources.mjs
git commit -m "$(cat <<'EOF'
harden(scripts): centralize HTTPS fetch with protocol/redirect/auth controls

Replace the recursive doFetch/doDownload helpers in
prepare-runtime-resources.mjs with a single httpsGetWithRedirects core that:

- rejects non-https URLs (initial and redirected)
- caps redirects at 5, rejects empty/malformed Location
- forwards Authorization: Bearer GITHUB_TOKEN only to GitHub-owned hosts
- destroys the WriteStream deterministically on finish and error, and
  observes response-side errors (closes a pre-existing fd leak)

fetchJson and downloadFile become thin wrappers. No new dependencies.

OpenSpec change: harden-runtime-resource-fetch
EOF
)"
```

archived-with: 2026-05-29-harden-runtime-resource-fetch
---

### Task 2: OMP binary integrity verification

Covers tasks.md group 4. Adds digest parsing + file hashing, then wires into `ensureOmpBinary` so a tampered download is hard-failed and an absent/malformed digest is soft-failed (skip OMP, build continues).

**Files:**
- Modify: `MyBitFun/scripts/prepare-runtime-resources.mjs`

- [ ] **Step 1: Add digest helpers above ensureOmpBinary**

Insert after `pipeToFile` (or anywhere in the helper layer above `getOmpTarget`):

```js
// ── Integrity verification ───────────────────────────────────────────────────

function parseAssetDigest(value) {
  if (typeof value !== 'string') return null;
  const m = SHA256_HEX.exec(value.toLowerCase());
  return m ? Buffer.from(m[1], 'hex') : null;
}

function sha256File(path) {
  return new Promise((resolve, reject) => {
    const hash = createHash('sha256');
    createReadStream(path)
      .on('error', reject)
      .on('data', (chunk) => hash.update(chunk))
      .on('end', () => resolve(hash.digest()));
  });
}
```

- [ ] **Step 2: Replace ensureOmpBinary with the verifying version**

Replace the existing `ensureOmpBinary` body (currently lines ~156–209) with:

```js
export async function ensureOmpBinary() {
  const ompDir = join(ROOT, 'resources', 'omp');
  const target = getOmpTarget();

  if (!target) {
    console.warn(`[runtime-resources] OMP: unsupported platform '${process.platform}/${process.arch}', skipping.`);
    return;
  }

  const localPath = join(ompDir, target.localName);
  const versionFile = join(ompDir, '.omp-version');

  if (existsSync(localPath)) {
    const existingVersion = existsSync(versionFile) ? readFileSync(versionFile, 'utf8').trim() : '(manual)';
    console.log(`[runtime-resources] OMP binary already present (${existingVersion}): ${target.localName}`);
    return;
  }

  console.log('[runtime-resources] Fetching latest OMP release info...');
  let release;
  try {
    release = await fetchJson(`https://api.github.com/repos/${OMP_REPO}/releases/latest`);
  } catch (e) {
    console.warn(`[runtime-resources] WARNING: Failed to fetch OMP release info: ${e.message}`);
    console.warn('[runtime-resources] OMP runtime will not be available in this build.');
    return;
  }

  const tag = release.tag_name;
  if (!tag) {
    console.warn('[runtime-resources] WARNING: Could not determine latest OMP version.');
    return;
  }

  const asset = Array.isArray(release.assets)
    ? release.assets.find((a) => a && a.name === target.remoteName)
    : null;
  if (!asset) {
    console.warn(`[runtime-resources] WARNING: No asset named '${target.remoteName}' in OMP release ${tag}.`);
    console.warn('[runtime-resources] OMP runtime will not be available in this build.');
    return;
  }

  const expected = parseAssetDigest(asset.digest);
  if (!expected) {
    console.warn(
      `[runtime-resources] WARNING: OMP asset '${target.remoteName}' in release ${tag} has no/malformed digest (got: ${JSON.stringify(asset.digest)}).`
    );
    console.warn('[runtime-resources] Refusing to install without an integrity check. OMP runtime will not be available in this build.');
    return;
  }

  const url = asset.browser_download_url
    || `https://github.com/${OMP_REPO}/releases/download/${tag}/${target.remoteName}`;
  console.log(`[runtime-resources] Downloading OMP ${tag}: ${target.remoteName} (${target.localName})...`);

  mkdirSync(ompDir, { recursive: true });

  try {
    await downloadFile(url, localPath);
  } catch (e) {
    console.warn(`[runtime-resources] WARNING: Failed to download OMP binary: ${e.message}`);
    console.warn('[runtime-resources] OMP runtime will not be available in this build.');
    try { unlinkSync(localPath); } catch {}
    return;
  }

  let actual;
  try {
    actual = await sha256File(localPath);
  } catch (e) {
    try { unlinkSync(localPath); } catch {}
    throw new Error(`Failed to hash downloaded OMP binary: ${e.message}`);
  }

  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
    try { unlinkSync(localPath); } catch {}
    throw new Error(
      `OMP integrity check failed for ${target.remoteName}: expected ${expected.toString('hex')}, got ${actual.toString('hex')}`
    );
  }

  if (process.platform !== 'win32') {
    chmodSync(localPath, 0o755);
  }

  writeFileSync(versionFile, `${tag}\n`, 'utf8');
  console.log(`[runtime-resources] OMP ${tag} downloaded and verified: ${target.localName}`);
}
```

Behaviour map:
- Asset missing → soft-fail (warn + return).
- `digest` missing/malformed → soft-fail (warn + return).
- Download fails → soft-fail (warn + return; existing contract).
- Hash mismatch → hard-fail (delete file + throw).
- Match → chmod + write `.omp-version` (existing happy path).

- [ ] **Step 3: Syntax check**

```bash
node --check scripts/prepare-runtime-resources.mjs
```

Expected: no output, exit 0.

- [ ] **Step 4: Tick off tasks.md**

Edit `F:/Work/Mybitfun/openspec/changes/harden-runtime-resource-fetch/tasks.md`: mark every checkbox in groups §1–§5 as `- [x]`. Group §6 (manual verification) stays `- [ ]` for the verify phase to claim.

- [ ] **Step 5: Commit**

```bash
git add scripts/prepare-runtime-resources.mjs
git commit -m "$(cat <<'EOF'
harden(scripts): verify OMP binary SHA-256 against GitHub release digest

Look up the matching asset entry by name and parse its digest
(`sha256:<hex>`) from the same releases/latest response we already fetch.
After download, re-hash the file via createReadStream → createHash and
compare with timingSafeEqual.

- Hash mismatch → delete the file and throw (build hard-fails).
- Asset absent or digest missing/malformed → warn and skip OMP, matching
  the existing "OMP runtime will not be available" degradation path.

OpenSpec change: harden-runtime-resource-fetch
EOF
)"
```

archived-with: 2026-05-29-harden-runtime-resource-fetch
---

### Task 3: Smoke-run the script

Final build-phase check — happy path execution. Negative scenarios (HTTPS rejection, redirect cap, hash mismatch, missing digest) live in the verify phase and don't run here.

- [ ] **Step 1: Run setup**

From `F:/Work/Mybitfun/MyBitFun`:

```bash
node scripts/prepare-runtime-resources.mjs
```

Expected output ends with one of:
- `[runtime-resources] OMP <tag> downloaded and verified: <file>` — fresh download succeeded
- `[runtime-resources] OMP binary already present (<tag>): <file>` — existing binary kept

If a hash mismatch or other hard-fail surfaces here, **stop** and treat as a real bug (could indicate network compromise or an upstream regression). Do not work around it.

- [ ] **Step 2: Confirm output state**

```bash
ls -la resources/omp/
cat resources/omp/.omp-version
```

Expected: binary file present + matching version stamp.

- [ ] **Step 3: Capture verification report stub**

Write `F:/Work/Mybitfun/openspec/changes/harden-runtime-resource-fetch/.comet/handoff/build-smoke.md` with:

```
# Build smoke (Task 3)

Command: node scripts/prepare-runtime-resources.mjs
Result: <PASS / FAIL>
Output tail:
<paste last ~10 lines>
.omp-version: <contents>

Negative scenarios deferred to /comet-verify.
```

The verify phase will pull the rest of `tasks.md §6` from this baseline.

archived-with: 2026-05-29-harden-runtime-resource-fetch
---

## Self-review

- ✅ Spec coverage: every requirement in `specs/runtime-resource-fetch/spec.md` maps to code in Task 1 (HTTPS-only, redirect cap, GITHUB_TOKEN, stream teardown) or Task 2 (digest verification).
- ✅ Token trim scenario added in Task 1 Step 3 (`readGitHubToken()`).
- ✅ All code blocks are complete — no `TODO` or `// ...` stubs.
- ✅ Type/name consistency: `httpsGetWithRedirects`, `isGitHubHost`, `readGitHubToken`, `pipeToFile`, `parseAssetDigest`, `sha256File` used identically across tasks and the design doc.
- ✅ No test framework introduced (out of scope per design); manual smoke in Task 3, full negative-path verification in `/comet-verify`.
