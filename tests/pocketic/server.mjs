import { spawn } from "node:child_process";
import { once } from "node:events";
import { chmodSync } from "node:fs";
import { appendFile, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const pollIntervalMs = 100;
const maxStartAttempts = 8;

export async function startPocketIcServer({ timeoutMs }) {
  const bin = resolve("node_modules/@dfinity/pic/pocket-ic");
  logServer(`prepare binary ${bin}`);
  chmodSync(bin, 0o700);
  const deadline = Date.now() + timeoutMs;
  let lastError;
  let attempts = 0;

  while (Date.now() < deadline && attempts < maxStartAttempts) {
    attempts += 1;
    try {
      return await startOnce(bin, deadline);
    } catch (error) {
      lastError = error;
      logServer(`start failed: ${error?.message ?? error}`);
      await recordPocketIcStartError(error);
      if (!isRetryableStartError(error)) {
        throw error;
      }
      await sleep(500);
    }
  }

  if (lastError) {
    throw new Error(
      `PocketIC did not start after ${attempts} attempts within ${timeoutMs}ms\n${lastError.message}`,
    );
  }
  throw new Error(`PocketIC did not start within ${timeoutMs}ms`);
}

async function startOnce(bin, deadline) {
  const dir = await mkdtemp(join(tmpdir(), "ic-sqlite-vfs-pocketic-"));
  const portFile = join(dir, "pocketic.port");
  const port = configuredPocketIcPort() ?? (await reservePocketIcPort());
  const args = ["--port", String(port), "--port-file", portFile];
  const output = [];
  logServer(`spawn port=${port} portFile=${portFile} args=${args.join(" ")}`);
  const child = spawn(bin, args, {
    stdio: ["ignore", "pipe", "pipe"],
    env: pocketIcEnv(),
  });
  logServer(`spawned pid=${child.pid ?? "undefined"} port=${port}`);
  await recordPocketIcPid(child.pid);
  await recordPocketIcLifecycle(child.pid, `spawned port=${port} args=${args.join(" ")}`);
  child.stdout.on("data", (chunk) => {
    appendOutput(output, chunk);
    recordPocketIcOutput(child.pid, chunk);
  });
  child.stderr.on("data", (chunk) => {
    appendOutput(output, chunk);
    recordPocketIcOutput(child.pid, chunk);
  });
  let startError;
  child.once("error", (error) => {
    startError = error;
    logServer(`child error pid=${child.pid ?? "undefined"} ${error?.message ?? error}`);
  });
  child.once("exit", (code, signal) => {
    logServer(
      `child exit pid=${child.pid ?? "undefined"} code=${code ?? "null"} signal=${signal ?? "null"}`,
    );
  });

  let nextWaitLogAt = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (startError) {
      await stopChild(child, dir);
      throw startError;
    }
    if (child.exitCode !== null) {
      await stopChild(child, dir);
      throw new Error(
        `PocketIC exited before writing port file: code=${child.exitCode} args=${args.join(
          " ",
        )}\n${output.join("")}`,
      );
    }

    const readyPort = await readPort(portFile);
    if (readyPort !== undefined) {
      logServer(`ready pid=${child.pid ?? "undefined"} port=${readyPort}`);
      await recordPocketIcLifecycle(child.pid, `ready port=${readyPort}`);
      return {
        getUrl() {
          return `http://127.0.0.1:${readyPort}`;
        },
        async stop() {
          await stopChild(child, dir);
        },
      };
    }
    if (Date.now() >= nextWaitLogAt) {
      logServer(`waiting pid=${child.pid ?? "undefined"} portFile=${portFile}`);
      nextWaitLogAt = Date.now() + 5_000;
    }
    await sleep(pollIntervalMs);
  }

  await stopChild(child, dir);
  throw new Error(
    `PocketIC did not start before deadline args=${args.join(" ")}\n${output.join("")}`,
  );
}

async function reservePocketIcPort() {
  return await new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (typeof address !== "object" || address === null) {
        server.close(() => reject(new Error("failed to reserve PocketIC port")));
        return;
      }
      const { port } = address;
      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(port);
      });
    });
  });
}

function configuredPocketIcPort() {
  const value = process.env.IC_SQLITE_VFS_POCKETIC_PORT;
  if (value === undefined || value === "") {
    return undefined;
  }
  const port = Number.parseInt(value, 10);
  if (!Number.isInteger(port) || port <= 0 || port > 65_535) {
    throw new Error(`invalid IC_SQLITE_VFS_POCKETIC_PORT: ${value}`);
  }
  return port;
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
    await recordPocketIcLifecycle(child.pid, "stopping SIGTERM");
    await killProcessTree(child.pid, "SIGTERM");
    const stopped = await waitForExit(child, 2_000);
    if (!stopped) {
      await recordPocketIcLifecycle(child.pid, "stopping SIGKILL");
      await killProcessTree(child.pid, "SIGKILL");
      await waitForExit(child, 2_000);
    }
  }
  await recordPocketIcLifecycle(
    child.pid,
    `stopped exit=${child.exitCode ?? "null"} signal=${child.signalCode ?? "null"}`,
  );
  await rm(dir, { recursive: true, force: true });
}

async function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return true;
  }
  const timeout = sleep(timeoutMs).then(() => false);
  const exited = once(child, "exit")
    .then(() => true)
    .catch(() => true);
  return Promise.race([exited, timeout]);
}

async function killProcessTree(pid, signal) {
  if (pid === undefined) {
    return;
  }
  for (const childPid of await childPids(pid)) {
    await killProcessTree(childPid, signal);
  }
  try {
    process.kill(pid, signal);
  } catch (error) {
    if (error?.code !== "ESRCH" && error?.code !== "EPERM") {
      throw error;
    }
  }
}

async function childPids(pid) {
  const pgrep = spawn("pgrep", ["-P", String(pid)], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  let output = "";
  pgrep.stdout.on("data", (chunk) => {
    output += chunk.toString();
  });
  await once(pgrep, "close").catch(() => {});
  return output
    .split(/\s+/)
    .filter(Boolean)
    .map((value) => Number.parseInt(value, 10))
    .filter((value) => !Number.isNaN(value));
}

function appendOutput(output, chunk) {
  output.push(chunk.toString());
  while (output.join("").length > 8_192) {
    output.shift();
  }
}

function logServer(message) {
  console.error(`[pocketic:server] ${message}`);
}

function isRetryableStartError(error) {
  return error?.message?.includes("Failed to bind PocketIC server to address 127.0.0.1:");
}

async function recordPocketIcPid(pid) {
  const runDir = process.env.IC_SQLITE_VFS_POCKETIC_RUN_DIR;
  if (!runDir || pid === undefined) {
    return;
  }
  await mkdir(runDir, { recursive: true });
  await appendFile(join(runDir, "pocketic.pids"), `${pid}\n`);
}

function recordPocketIcOutput(pid, chunk) {
  const runDir = process.env.IC_SQLITE_VFS_POCKETIC_RUN_DIR;
  if (!runDir || pid === undefined) {
    return;
  }
  appendFile(join(runDir, `pocketic-${pid}.log`), chunk).catch(() => {});
}

async function recordPocketIcLifecycle(pid, message) {
  const runDir = process.env.IC_SQLITE_VFS_POCKETIC_RUN_DIR;
  if (!runDir || pid === undefined) {
    return;
  }
  await mkdir(runDir, { recursive: true });
  await appendFile(join(runDir, `pocketic-${pid}.log`), `[server] ${message}\n`);
}

async function recordPocketIcStartError(error) {
  const runDir = process.env.IC_SQLITE_VFS_POCKETIC_RUN_DIR;
  if (!runDir) {
    return;
  }
  await appendFile(join(runDir, "pocketic-start-errors.log"), `${error?.message ?? error}\n---\n`);
}

function pocketIcEnv() {
  const env = { ...process.env };
  delete env.IC_SQLITE_VFS_POCKETIC_RUN_DIR;
  return env;
}
