import { describe, expect, it } from "vitest";

import { daemonVersion, fixCommands, problemCount, type DoctorDocument } from "./doctor";
import machine from "./fixtures/doctor.json";
import noDaemon from "./fixtures/doctor-no-daemon.json";
import drift from "./fixtures/doctor-release-drift.json";

describe("the problems of a doctor run", () => {
  it("count the failed checks and the warnings, and nothing else", () => {
    expect(problemCount(machine as DoctorDocument)).toBe(4);
    expect(problemCount(noDaemon as DoctorDocument)).toBe(2);
    expect(problemCount(drift as DoctorDocument)).toBe(1);
  });

  it("are none when every check passed or was skipped", () => {
    const healthy: DoctorDocument = {
      ...(machine as DoctorDocument),
      checks: (machine as DoctorDocument).checks.filter(
        (check) => check.status === "passed" || check.status === "skipped",
      ),
    };

    expect(problemCount(healthy)).toBe(0);
  });
});

describe("the release the Daemon runs", () => {
  it("is read from the check of the Daemon when it answers", () => {
    expect(daemonVersion(machine as DoctorDocument)).toBe("0.5.0");
  });

  it("is the one of the Daemon, not of this app, when they differ", () => {
    expect(daemonVersion(drift as DoctorDocument)).toBe("0.4.0");
  });

  it("is unknown when the Daemon did not answer", () => {
    expect(daemonVersion(noDaemon as DoctorDocument)).toBeNull();
  });
});

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

  it("keep the quotes of a fix that moves to a path with a space, as doctor wrote them", () => {
    expect(fixCommands("cd '/p/work/acme/my web' && procly env web", null)).toEqual([
      "(cd '/p/work/acme/my web' && procly env web)",
    ]);
  });
});
