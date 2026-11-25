import { describe, expect, it } from "vitest";
import { syncTargetsFor } from "@/lib/syncTargets";

describe("syncTargetsFor", () => {
  it("offers every other app for an OpenCode provider", () => {
    expect(syncTargetsFor("opencode").map((option) => option.target)).toEqual([
      "dsh",
      "codex",
      "claude",
      "claude-desktop",
    ]);
  });

  it("lets Claude Code and Claude Desktop feed each other", () => {
    expect(syncTargetsFor("claude").map((option) => option.target)).toEqual([
      "claude-desktop",
    ]);
    expect(
      syncTargetsFor("claude-desktop").map((option) => option.target),
    ).toEqual(["claude"]);
  });

  it("keeps apps without a counterpart menu free of sync entries", () => {
    expect(syncTargetsFor("codex")).toEqual([]);
    expect(syncTargetsFor("dsh")).toEqual([]);
  });
});
