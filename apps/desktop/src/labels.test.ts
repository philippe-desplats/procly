import { describe, expect, it } from "vitest";

import { worktreeLabel, worktreeTooltip } from "./labels";

describe("the label of a Worktree", () => {
  it("is its branch", () => {
    expect(worktreeLabel({ path: "/p/work/acme/web-fix", slug: "web_fix", branch: "fix/login" })).toBe(
      "fix/login",
    );
  });

  it("falls back to the basename of its path, without a detached marker, when there is no branch", () => {
    const detached = { path: "/p/work/acme/web-fix/", slug: "web_fix" };

    expect(worktreeLabel({ ...detached, branch: null })).toBe("web-fix");
    expect(worktreeLabel(detached)).toBe("web-fix");
    expect(worktreeLabel({ ...detached, branch: "" })).toBe("web-fix");
    expect(worktreeLabel({ ...detached, branch: null })).not.toMatch(/detached/i);
  });

  it("shows the branch, the basename and the slug in its tooltip", () => {
    const tooltip = worktreeTooltip({ path: "/p/work/acme/web-fix", slug: "web_fix", branch: "fix/login" });

    expect(tooltip).toBe("Branch: fix/login\nFolder: web-fix\nSlug: web_fix");
  });

  it("says there is no branch in its tooltip rather than leaving the line out", () => {
    expect(worktreeTooltip({ path: "/p/work/acme/web", slug: "web" })).toBe(
      "Branch: none\nFolder: web\nSlug: web",
    );
  });
});
