import { execFile } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
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

/** Where a failed run copies its logs before its temporary root is removed. The CI sets it and uploads the directory. */
const ARTIFACTS = "PROCLY_E2E_ARTIFACTS";

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
  mkdirSync(join(root, "home"));
  process.env[ROOT] = root;
  process.env["PROCLY_STATE_DIR"] = join(root, "state");
  process.env["PROCLY_CONFIG_DIR"] = join(root, "config");
  // The app runs under a home of its own, so that its settings, its Seen set, its window state and the caches of its
  // webview never land in the developer's. Tauri finds the data directories through `HOME`, and the system frameworks
  // that WebKit writes its caches with through `CFFIXED_USER_HOME`.
  process.env["HOME"] = join(root, "home");
  process.env["CFFIXED_USER_HOME"] = join(root, "home");
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
  // The identity and the branch are passed on the command line, so no configuration of the developer is read or
  // written, and the sidebar names the Worktree by the same branch on every machine.
  await run("git", ["init", "--quiet", "--initial-branch=main"], { cwd: path });
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

/** One Service as `procly status --json` reports it. */
interface ReportedService {
  service: string;
  state: string;
  port: number | null;
  started_at: number;
}

/** The Service `name` of the Worktree of `scene` as the CLI reports it, or `null` when the status does not list it. */
export async function reportedService(scene: Scene, name: string): Promise<ReportedService | null> {
  const { stdout } = await procly(["status", "--json"], scene.path);
  const report = JSON.parse(stdout) as {
    repositories: { worktrees: { path: string; services: ReportedService[] }[] }[];
  };
  for (const repository of report.repositories) {
    for (const worktree of repository.worktrees) {
      const found = worktree.services.find((service) => service.service === name);
      if (found !== undefined) {
        return found;
      }
    }
  }
  return null;
}

/** The test server fixture, which a scene also starts by hand to have a process Procly did not start. */
export function testServer(): string {
  return required(TEST_SERVER);
}

/**
 * Copies what a failed run needs to be diagnosed to the directory `PROCLY_E2E_ARTIFACTS` names: the log of the Daemon,
 * the output the Daemon captured for each Service and Job, and the logs of the runner, the output of the app among
 * them. Does nothing when the variable is unset. Called once, by the runner, after the Daemon is stopped and before
 * the temporary root is removed.
 */
export function keepLogs(): void {
  const target = process.env[ARTIFACTS];
  const root = process.env[ROOT];
  if (target === undefined || target === "" || root === undefined) {
    return;
  }
  mkdirSync(target, { recursive: true });
  const kept: [from: string, to: string][] = [
    [join(root, "state", "daemon.log"), "daemon.log"],
    [join(root, "state", "output"), "output"],
    [logDirectory(), "logs"],
  ];
  for (const [from, to] of kept) {
    if (existsSync(from)) {
      cpSync(from, join(target, to), { recursive: true });
    }
  }
}

/** Removes everything the run created. Called once, by the runner, after the Daemon is stopped. */
export function discard(): void {
  const root = process.env[ROOT];
  if (root !== undefined) {
    rmSync(root, { recursive: true, force: true });
  }
}
