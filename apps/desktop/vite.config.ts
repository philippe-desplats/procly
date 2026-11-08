import { readFileSync } from "node:fs";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

/**
 * The release of the app: the `version` of `[workspace.package]` in the `Cargo.toml` of the workspace, which every
 * crate, the embedded procly and the bundle inherit. A screen names it where it compares it with the one of the Daemon.
 */
const version = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(
  readFileSync(new URL("../../Cargo.toml", import.meta.url), "utf8"),
)?.[1];
if (version === undefined) {
  throw new Error("the Cargo.toml of the workspace declares no version in [workspace.package]");
}

// Tauri loads the built files from disk, and its dev window loads this server on a fixed port.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  define: { __APP_VERSION__: JSON.stringify(version) },
  server: { port: 1420, strictPort: true },
  build: { target: "safari16", outDir: "dist" },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test-setup.ts"],
  },
});
