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
});
