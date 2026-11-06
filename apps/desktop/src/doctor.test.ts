import { describe, expect, it } from "vitest";

import { fixCommands } from "./doctor";

describe("the commands of a fix", () => {
  it("are one per line, and none is blank", () => {
    expect(fixCommands("kill 4812\n\nprocly gc\n", null)).toEqual(["kill 4812", "procly gc"]);
  });

  it("run in the Worktree they are for when they are procly commands", () => {
    expect(fixCommands("procly agent install", "/p/work/acme/web")).toEqual([
      "(cd /p/work/acme/web && procly agent install)",
    ]);
  });

  it("leave a command that is not procly as it is, and every command of the machine", () => {
    expect(fixCommands("npm install --global portless", "/p/work/acme/web")).toEqual([
      "npm install --global portless",
    ]);
    expect(fixCommands("procly unit install", null)).toEqual(["procly unit install"]);
  });

  it("put a path the shell would split in quotes", () => {
    expect(fixCommands("procly worktree hooks", "/p/work/acme/my web")).toEqual([
      "(cd '/p/work/acme/my web' && procly worktree hooks)",
    ]);
    expect(fixCommands("procly worktree hooks", "/p/work/it's")).toEqual([
      "(cd '/p/work/it'\\''s' && procly worktree hooks)",
    ]);
  });

  it("only close in parentheses a fix that already moves to a directory", () => {
    expect(fixCommands("cd /p/work/acme/web && procly env web", null)).toEqual([
      "(cd /p/work/acme/web && procly env web)",
    ]);
    expect(fixCommands("cd /p/work/acme/web && procly env web", "/p/work/acme/other")).toEqual([
      "(cd /p/work/acme/web && procly env web)",
    ]);
  });
});
