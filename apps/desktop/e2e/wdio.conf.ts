import { appExecutable, discard, isolate, keepLogs, logDirectory, stopDaemon } from "./environment";

// The runner and each worker load this file. The first to do so isolates the run, the others inherit it.
await isolate();

const FIVE_MINUTES = 5 * 60 * 1000;

// Declared apart: the capability types of WebdriverIO do not list the options of the Tauri service.
const tauri = { browserName: "tauri", "tauri:options": { application: appExecutable() } };

/**
 * Drives the built app through the WebDriver server that `tauri-plugin-wdio-webdriver` runs inside it (ADR 0016).
 * The service starts the executable itself, with this process's environment, so the app and the scenes talk to the
 * same isolated Daemon.
 */
export const config: WebdriverIO.Config = {
  runner: "local",
  // The service starts the app once for the whole run, so a flow inherits the window the one before left. The close
  // flow hides it, and a hidden window has the poller ask the Daemon every 30 seconds instead of every 2 to 10, so it
  // runs last and every other flow has a shown window, as a developer has.
  specs: ["./specs/!(close).e2e.ts", "./specs/close.e2e.ts"],
  tsConfigPath: "./tsconfig.json",
  // One window of one app, and one Daemon: the flows run one after the other.
  maxInstances: 1,
  services: [
    [
      "tauri",
      {
        appBinaryPath: appExecutable(),
        driverProvider: "embedded",
        embeddedPort: Number(process.env["TAURI_WEBDRIVER_PORT"]),
        startTimeout: 120_000,
        autoInstallTauriDriver: false,
        // What the app writes on its output goes to a file of the log directory, whatever its level, so that a failed
        // run keeps it (`keepLogs`). Nothing of it reaches the console of the runner.
        captureBackendLogs: true,
        backendLogLevel: "trace",
        captureFrontendLogs: false,
        logDir: logDirectory(),
      },
    ],
  ],
  capabilities: [tauri],
  logLevel: "warn",
  outputDir: logDirectory(),
  waitforTimeout: 30_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 1,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: { ui: "bdd", timeout: FIVE_MINUTES },
  // Whatever a flow left running goes with the Daemon, then the temporary directories. A failed run keeps its logs
  // first, since nothing of the temporary root survives it.
  onComplete: async (exitCode: number) => {
    await stopDaemon();
    if (exitCode !== 0) {
      keepLogs();
    }
    discard();
  },
};
