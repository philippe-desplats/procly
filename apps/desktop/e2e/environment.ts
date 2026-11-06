import { execFile } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { promisify } from "node:util";

const run = promisify(execFile);

/** The directory that holds everything one run of the end-to-end tests creates, set once for the runner and its workers. */
const ROOT = "PROCLY_E2E_ROOT";

/** The executable of the app, built by `make e2e` with the `e2e` feature. */
const APP = "PROCLY_E2E_APP";

/** The test server fixture a scene runs as a Service. */
const TEST_SERVER = "PROCLY_E2E_TEST_SERVER";

function required(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") {
    throw new Error(`${name} is not set, run the tests with \`make e2e\``);
  }
  return value;
}

/** A port nothing listens on right now. */
export async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        if (address === null || typeof address === "string") {
          reject(new Error("the server has no port"));
        } else {
          resolve(address.port);
        }
      });
    });
  });
}

/**
 * Points the Daemon, and the app that talks to it, at temporary directories and the WebDriver server at a free port.
 * The runner and its workers both load the configuration, so the first one sets the variables and the others inherit
 * them, which is how the app and the CLI scenes reach the same Daemon. The state directory is short on purpose: a
 * Unix socket path is limited to 104 bytes on macOS.
 */
export async function isolate(): Promise<void> {
  if (process.env[ROOT] !== undefined) {
    return;
  }
  const root = mkdtempSync(join(tmpdir(), "pcl-e2e-"));
  mkdirSync(join(root, "state"));
  mkdirSync(join(root, "config"));
  mkdirSync(join(root, "work"));
  process.env[ROOT] = root;
  process.env["PROCLY_STATE_DIR"] = join(root, "state");
  process.env["PROCLY_CONFIG_DIR"] = join(root, "config");
  process.env["TAURI_WEBDRIVER_PORT"] = String(await freePort());
}

/** Where the runner keeps its own logs, outside the repository. */
export function logDirectory(): string {
  return join(required(ROOT), "logs");
}

/** The executable of the app. */
export function appExecutable(): string {
  return required(APP);
}

/** The `procly` the app embeds, which sits beside its executable. The scenes use it too, never the one on the `PATH`. */
export function sidecar(): string {
  return join(dirname(appExecutable()), "procly");
}

/** The outcome of a command, whatever its exit code. */
export interface Outcome {
  code: number;
  stdout: string;
  stderr: string;
}

async function outcome(file: string, args: string[], cwd: string): Promise<Outcome> {
  try {
    const { stdout, stderr } = await run(file, args, { cwd, env: process.env });
    return { code: 0, stdout, stderr };
  } catch (error) {
    const failed = error as { code?: unknown; stdout?: string; stderr?: string };
    return {
      code: typeof failed.code === "number" ? failed.code : 1,
      stdout: failed.stdout ?? "",
      stderr: failed.stderr ?? "",
    };
  }
}

/** Runs the embedded `procly` against the isolated Daemon: this is how a scene is set. */
export async function procly(args: string[], cwd: string = required(ROOT)): Promise<Outcome> {
  return outcome(sidecar(), args, cwd);
}

/** Stops the Daemon of the isolated state directory, which stops every Service it supervises, and tells whether it did. */
export async function stopDaemon(): Promise<boolean> {
  const { code } = await procly(["daemon", "stop", "--json"]);
  return code === 0;
}

/** A Service the Daemon supervises in a Worktree of a repository of its own. */
export interface Scene {
  /** The Worktree, a git repository the `procly.toml` sits in. */
  path: string;
  service: string;
  port: number;
}

/** Writes a repository whose `procly.toml` declares one Service, the test server, on a port nothing listens on. */
export async function writeScene(name: string, service: string): Promise<Scene> {
  const path = join(required(ROOT), "work", name);
  const port = await freePort();
  mkdirSync(path);
  // The identity is passed on the command line, so no configuration of the developer is read or written.
  await run("git", ["init", "--quiet"], { cwd: path });
  await run(
    "git",
    [
      "-c",
      "user.name=e2e",
      "-c",
      "user.email=e2e@example.com",
      "commit",
      "--quiet",
      "--allow-empty",
      "-m",
      "initial",
    ],
    { cwd: path },
  );
  const config = `[services.${service}]\nrun = '${required(TEST_SERVER)} --port {ports.http}'\nports = { http = ${port} }\n`;
  writeFileSync(join(path, "procly.toml"), config);
  return { path, service, port };
}

/** Removes everything the run created. Called once, by the runner, after the Daemon is stopped. */
export function discard(): void {
  const root = process.env[ROOT];
  if (root !== undefined) {
    rmSync(root, { recursive: true, force: true });
  }
}
