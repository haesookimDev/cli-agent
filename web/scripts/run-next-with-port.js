#!/usr/bin/env node

const { spawnSync } = require("node:child_process");

const [, , subcommand, ...extraArgs] = process.argv;

if (!subcommand) {
  console.error(
    "Usage: node scripts/run-next-with-port.js <dev|start> [args...]",
  );
  process.exit(1);
}

const hasExplicitPort = extraArgs.some(
  (arg) => arg === "--port" || arg === "-p" || arg.startsWith("--port="),
);

let nextArgs = [require.resolve("next/dist/bin/next"), subcommand, ...extraArgs];

if (!hasExplicitPort) {
  const rawPort = process.env.AGENT_WEB_PORT || process.env.PORT || "3000";
  const port = Number.parseInt(rawPort, 10);

  if (!/^\d+$/.test(rawPort) || !Number.isSafeInteger(port) || port < 0) {
    console.error(
      `AGENT_WEB_PORT must be a non-negative integer, received: ${rawPort}`,
    );
    process.exit(1);
  }

  nextArgs = [
    require.resolve("next/dist/bin/next"),
    subcommand,
    "--port",
    String(port),
    ...extraArgs,
  ];
}

const result = spawnSync(process.execPath, nextArgs, {
  env: process.env,
  stdio: "inherit",
});

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}

process.exit(result.status ?? 1);
