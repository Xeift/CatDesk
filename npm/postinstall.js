#!/usr/bin/env node

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const packageRoot = path.resolve(__dirname, "..");
const packageJson = require(path.join(packageRoot, "package.json"));
const version = packageJson.version;
const releaseTag = `v${version}`;
const releaseBaseUrl = `https://github.com/Xeift/CatDesk/releases/download/${releaseTag}`;

const supportedTargets = new Set([
  "linux-x64",
  "linux-arm64",
  "darwin-x64",
  "darwin-arm64",
  "win32-x64",
]);

const platform = process.platform;
const arch = process.arch;
const target = `${platform}-${arch}`;

if (!supportedTargets.has(target)) {
  console.error(`CatDesk does not provide a prebuilt binary for ${target}.`);
  console.error(`Supported targets: ${Array.from(supportedTargets).join(", ")}`);
  process.exit(1);
}

const assetName = platform === "win32" ? `catdesk-${target}.exe` : `catdesk-${target}`;
const executableName = platform === "win32" ? "catdesk.exe" : "catdesk";
const binDir = path.join(__dirname, "bin");
const installedBinary = path.join(binDir, executableName);

async function fetchRequired(url) {
  const response = await fetch(url, {
    headers: {
      "User-Agent": `catdesk-npm-install/${version}`,
    },
  });

  if (!response.ok) {
    throw new Error(`${url} returned HTTP ${response.status}`);
  }

  return response;
}

async function downloadBuffer(url) {
  const response = await fetchRequired(url);
  return Buffer.from(await response.arrayBuffer());
}

async function downloadText(url) {
  const response = await fetchRequired(url);
  return response.text();
}

function expectedSha256(checksums, name) {
  for (const line of checksums.split(/\r?\n/)) {
    const match = line.trim().match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (match && path.basename(match[2]) === name) {
      return match[1].toLowerCase();
    }
  }

  throw new Error(`SHA256SUMS does not contain ${name}`);
}

function sha256(buffer) {
  return crypto.createHash("sha256").update(buffer).digest("hex");
}

async function main() {
  const checksums = await downloadText(`${releaseBaseUrl}/SHA256SUMS`);
  const expected = expectedSha256(checksums, assetName);
  const binary = await downloadBuffer(`${releaseBaseUrl}/${assetName}`);
  const actual = sha256(binary);

  if (actual !== expected) {
    throw new Error(`Checksum mismatch for ${assetName}: expected ${expected}, got ${actual}`);
  }

  fs.mkdirSync(binDir, { recursive: true });
  fs.writeFileSync(installedBinary, binary);

  if (platform !== "win32") {
    fs.chmodSync(installedBinary, 0o755);
  }
}

main().catch((error) => {
  console.error(`CatDesk install failed: ${error.message}`);
  process.exit(1);
});
