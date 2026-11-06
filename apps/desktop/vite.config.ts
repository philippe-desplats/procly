import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Tauri loads the built files from disk, and its dev window loads this server on a fixed port.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "safari16", outDir: "dist" },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test-setup.ts"],
  },
});
