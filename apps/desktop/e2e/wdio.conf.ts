import { appExecutable, discard, isolate, logDirectory, stopDaemon } from "./environment";

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
  specs: ["./specs/**/*.e2e.ts"],
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
        captureBackendLogs: false,
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
  // Whatever a flow left running goes with the Daemon, then the temporary directories.
  onComplete: async () => {
    await stopDaemon();
    discard();
  },
};
