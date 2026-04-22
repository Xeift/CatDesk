#!/usr/bin/env node

const { spawn } = require("node:child_process");
const path = require("node:path");

const executableName = process.platform === "win32" ? "catdesk.exe" : "catdesk";
const binaryPath = path.join(__dirname, "bin", executableName);

const child = spawn(binaryPath, process.argv.slice(2), {
  cwd: process.cwd(),
  env: process.env,
  stdio: "inherit",
});

child.on("error", (error) => {
  console.error(`CatDesk failed to start: ${error.message}`);
  console.error("Reinstall CatDesk after the matching GitHub Release binary is available.");
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
