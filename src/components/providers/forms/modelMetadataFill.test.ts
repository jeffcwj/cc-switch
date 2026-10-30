import { describe, expect, it } from "vitest";
import type { KnownModelMetadata } from "@/lib/modelMetadata";
import { fillOpenCodeModel } from "./modelMetadataFill";

const gpt56: KnownModelMetadata = {
  contextWindow: 272_000,
  maxOutputTokens: 128_000,
  reasoning: true,
  reasoningEfforts: ["none", "low", "medium", "high", "xhigh", "max"],
  inputModalities: ["text", "image", "pdf"],
  outputModalities: ["text"],
};

describe("fillOpenCodeModel", () => {
  it("keeps DSH within the modalities and levels it can actually load", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", modalities: { input: ["text", "pdf"] } },
      gpt56,
      "dsh",
    );

    expect(filled.modalities).toEqual({
      input: ["text", "image"],
      output: ["text"],
    });
    expect(filled.reasoningEfforts).toEqual({
      off: null,
      low: "low",
      medium: "medium",
      high: "high",
      xhigh: "xhigh",
      max: "max",
    });
    expect(filled.variants).toBeUndefined();
  });

  it("rescues an efforts map that only offers off", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", reasoningEfforts: { off: null } },
      gpt56,
      "dsh",
    );

    expect(Object.keys(filled.reasoningEfforts as object)).toEqual(
      expect.arrayContaining(["low", "high", "max"]),
    );
  });

  it("adds thinking levels when the model switches reasoning on", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", reasoningEfforts: false },
      gpt56,
      "dsh",
    );

    expect(filled.reasoningEfforts).toMatchObject({ off: null, high: "high" });
  });

  it("leaves an explicit level value alone while filling the gaps", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", reasoningEfforts: { off: null, high: "custom" } },
      gpt56,
      "dsh",
    );

    expect(filled.reasoningEfforts).toMatchObject({
      high: "custom",
      low: "low",
    });
  });

  it("keeps OpenCode's own dialect", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol" },
      gpt56,
      "opencode",
    );

    expect(filled.variants).toMatchObject({ low: { reasoningEffort: "low" } });
    expect(filled.reasoningEfforts).toBeUndefined();
    expect(filled.modalities).toEqual({
      input: ["text", "image", "pdf"],
      output: ["text"],
    });
  });
});
