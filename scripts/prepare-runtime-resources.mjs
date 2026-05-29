#!/usr/bin/env node
/**
 * Prepares optional runtime resources before desktop build.
 *
 * 1. Claude-bridge: runs `npm install` in resources/claude-bridge/ if node_modules missing.
 * 2. OMP: downloads the latest binary for the current platform if not already present.
 *
 * Called automatically by desktop-tauri-build.mjs and the CI pipeline.
 * Developers can also run: pnpm run setup:runtimes
 */

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

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');

const OMP_REPO = 'can1357/oh-my-pi';

const SHA256_HEX = /^sha256:([0-9a-f]{64})$/;
const DEFAULT_MAX_REDIRECTS = 5;
const USER_AGENT = 'BitFun-Build-Script';

export function ensureClaudeBridge() {
  const bridgeDir = join(ROOT, 'resources', 'claude-bridge');
  const pkgJson = join(bridgeDir, 'package.json');
  const nodeModules = join(bridgeDir, 'node_modules');

  if (!existsSync(pkgJson)) {
    console.log('[runtime-resources] claude-bridge/package.json not found, skipping.');
    return;
  }

  if (existsSync(nodeModules)) {
    console.log('[runtime-resources] claude-bridge/node_modules exists, skipping install.');
  } else {
    console.log('[runtime-resources] Installing claude-bridge dependencies...');
    const r = spawnSync('npm', ['install', '--omit', 'dev'], {
      cwd: bridgeDir,
      stdio: 'inherit',
      shell: true,
    });
    if (r.status !== 0) {
      console.warn('[runtime-resources] WARNING: claude-bridge npm install failed. Claude runtime will not be available.');
    } else {
      console.log('[runtime-resources] claude-bridge dependencies installed.');
    }
  }

  // Bundle Node.js binary for self-contained runtime
  const nodeBinaryName = process.platform === 'win32' ? 'node.exe' : 'node';
  const bundledNode = join(bridgeDir, nodeBinaryName);

  if (!existsSync(bundledNode)) {
    const systemNode = process.execPath;
    console.log(`[runtime-resources] Copying Node.js binary: ${systemNode} → ${nodeBinaryName}`);
    try {
      copyFileSync(systemNode, bundledNode);
      if (process.platform !== 'win32') {
        chmodSync(bundledNode, 0o755);
      }
      console.log('[runtime-resources] Node.js binary bundled.');
    } catch (e) {
      console.warn(`[runtime-resources] WARNING: Failed to copy Node.js binary: ${e.message}`);
      console.warn('[runtime-resources] Claude runtime will require Node.js on the target system.');
    }
  } else {
    console.log(`[runtime-resources] Node.js binary already bundled: ${nodeBinaryName}`);
  }
}

// ---------------------------------------------------------------------------
// OMP binary download
// ---------------------------------------------------------------------------

function getOmpTarget() {
  const platform = process.platform;
  const arch = process.arch;

  let ompPlatform;
  let ompArch;
  let localName;

  switch (platform) {
    case 'win32':
      ompPlatform = 'windows';
      ompArch = arch === 'arm64' ? 'arm64' : 'x64';
      localName = 'omp.exe';
      break;
    case 'darwin':
      ompPlatform = 'darwin';
      ompArch = arch === 'arm64' ? 'arm64' : 'x64';
      localName = 'omp';
      break;
    case 'linux':
      ompPlatform = 'linux';
      ompArch = arch === 'arm64' ? 'arm64' : 'x64';
      localName = 'omp';
      break;
    default:
      return null;
  }

  const remoteName = `omp-${ompPlatform}-${ompArch}${platform === 'win32' ? '.exe' : ''}`;
  return { remoteName, localName };
}

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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

export async function prepareRuntimeResources() {
  ensureClaudeBridge();
  await ensureOmpBinary();
}

// Run standalone if invoked directly
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  prepareRuntimeResources().catch((e) => {
    console.error('[runtime-resources] Error:', e);
    process.exit(1);
  });
}
