import { spawn } from "node:child_process";
import { once } from "node:events";
import { chmodSync } from "node:fs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const pollIntervalMs = 100;

export async function startPocketIcServer({ timeoutMs }) {
  const bin = resolve("node_modules/@dfinity/pic/pocket-ic");
  chmodSync(bin, 0o700);
  const deadline = Date.now() + timeoutMs;
  let lastError;

  while (Date.now() < deadline) {
    try {
      return await startOnce(bin, deadline);
    } catch (error) {
      lastError = error;
      if (!isRetryableStartError(error)) {
        throw error;
      }
      await sleep(500);
    }
  }

  throw lastError ?? new Error(`PocketIC did not start within ${timeoutMs}ms`);
}

async function startOnce(bin, deadline) {
  const dir = await mkdtemp(join(tmpdir(), "ic-sqlite-vfs-pocketic-"));
  const portFile = join(dir, "pocketic.port");
  const output = [];
  const child = spawn(bin, ["--port-file", portFile], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (chunk) => appendOutput(output, chunk));
  child.stderr.on("data", (chunk) => appendOutput(output, chunk));
  let startError;
  child.once("error", (error) => {
    startError = error;
  });

  while (Date.now() < deadline) {
    if (startError) {
      await stopChild(child, dir);
      throw startError;
    }
    if (child.exitCode !== null) {
      await stopChild(child, dir);
      throw new Error(
        `PocketIC exited before writing port file: code=${child.exitCode}\n${output.join("")}`,
      );
    }

    const port = await readPort(portFile);
    if (port !== undefined) {
      return {
        getUrl() {
          return `http://127.0.0.1:${port}`;
        },
        async stop() {
          await stopChild(child, dir);
        },
      };
    }
    await sleep(pollIntervalMs);
  }

  await stopChild(child, dir);
  throw new Error(`PocketIC did not start before deadline\n${output.join("")}`);
}

async function readPort(portFile) {
  try {
    const text = await readFile(portFile, "utf8");
    const port = Number.parseInt(text, 10);
    return Number.isNaN(port) ? undefined : port;
  } catch (error) {
    if (error?.code === "ENOENT") {
      return undefined;
    }
    throw error;
  }
}

async function stopChild(child, dir) {
  if (child.exitCode === null && child.signalCode === null) {
    child.kill();
    await once(child, "exit").catch(() => {});
  }
  await rm(dir, { recursive: true, force: true });
}

function appendOutput(output, chunk) {
  output.push(chunk.toString());
  while (output.join("").length > 8_192) {
    output.shift();
  }
}

function isRetryableStartError(error) {
  return error?.message?.includes("Failed to bind PocketIC server to address 127.0.0.1:0");
}
