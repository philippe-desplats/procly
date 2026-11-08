// @vitest-environment node
import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { APP_VERSION } from "./app-version";

/** The `version` of the workspace package table, which every crate and the embedded procly inherit. */
const workspace = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(
  readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8"),
)?.[1];

describe("the release this app ships", () => {
  it("is the release of the workspace, so that the screens of a Daemon of another version name the right one", () => {
    expect(workspace).toBeDefined();
    expect(APP_VERSION).toBe(workspace);
  });

  it("is read from the Cargo.toml of the workspace, the one source of the release, never from package.json", () => {
    // The 2 files say the same today, so the value alone cannot tell where the build reads it from.
    const config = readFileSync(new URL("../vite.config.ts", import.meta.url), "utf8");

    expect(config).toContain('new URL("../../Cargo.toml", import.meta.url)');
    expect(config).not.toContain("package.json");
  });
});
