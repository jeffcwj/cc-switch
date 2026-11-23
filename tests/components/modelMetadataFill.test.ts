import { describe, expect, it } from "vitest";

import {
  fillCodexCatalogModel,
  fillHermesModel,
  fillKimiModel,
  fillOpenClawModel,
  fillOpenCodeModel,
  fillWorkBuddyModel,
  piPresetThinkingFor,
  piSendsReasoningEffort,
  piThinkingLevelMapFromEfforts,
} from "@/components/providers/forms/modelMetadataFill";
import type { KnownModelMetadata } from "@/lib/modelMetadata";

const metadata: KnownModelMetadata = {
  contextWindow: 262144,
  maxOutputTokens: 32768,
  reasoning: true,
  reasoningEfforts: ["max", "low", "turbo", "high"],
  inputModalities: ["text", "image", "video"],
  outputModalities: ["text"],
  cost: { input: 1, output: 4 },
};

const CODEX_LEVELS = ["none", "low", "medium", "high", "xhigh", "max"];

describe("fillCodexCatalogModel", () => {
  it("fills blank fields in Codex's level order", () => {
    expect(
      fillCodexCatalogModel(
        { model: "kimi-k2.6", contextWindow: "" },
        metadata,
        CODEX_LEVELS,
      ),
    ).toEqual({
      model: "kimi-k2.6",
      contextWindow: "262144",
      reasoningLevels: ["low", "high", "max"],
      inputModalities: ["text", "image"],
    });
  });

  it("never overwrites what the user already set", () => {
    const row = {
      model: "kimi-k2.6",
      contextWindow: "128000",
      reasoningLevels: ["high"],
      inputModalities: ["text"],
    };
    expect(fillCodexCatalogModel(row, metadata, CODEX_LEVELS)).toEqual(row);
  });

  it("never turns image input off for a text-only lookup", () => {
    expect(
      fillCodexCatalogModel(
        { model: "deepseek-v4.1-flash-1017", contextWindow: "" },
        { contextWindow: 1_000_000, inputModalities: ["text"] },
        CODEX_LEVELS,
      ),
    ).toEqual({
      model: "deepseek-v4.1-flash-1017",
      contextWindow: "1000000",
    });
  });
});

describe("fillOpenClawModel", () => {
  it("fills numbers and cost, and upgrades the default text-only input", () => {
    expect(
      fillOpenClawModel({ id: "m", name: "m", input: ["text"] }, metadata),
    ).toEqual({
      id: "m",
      name: "m",
      contextWindow: 262144,
      maxTokens: 32768,
      reasoning: true,
      input: ["text", "image"],
      cost: { input: 1, output: 4 },
    });
  });

  it("keeps user values and never downgrades capabilities", () => {
    const model = {
      id: "m",
      name: "m",
      contextWindow: 1000,
      maxTokens: 10,
      reasoning: true,
      input: ["text", "image"],
      cost: { input: 9, output: 9 },
    };
    expect(
      fillOpenClawModel(model, {
        ...metadata,
        reasoning: false,
        inputModalities: ["text"],
      }),
    ).toEqual(model);
  });
});

describe("fillHermesModel", () => {
  it("fills context_length and supports_vision only when blank", () => {
    expect(fillHermesModel({ id: "m" }, metadata)).toEqual({
      id: "m",
      context_length: 262144,
      supports_vision: true,
    });
    expect(
      fillHermesModel(
        { id: "m", context_length: 1, supports_vision: true },
        metadata,
      ),
    ).toEqual({ id: "m", context_length: 1, supports_vision: true });
  });

  it("only fills supports_vision towards true and keeps an explicit false", () => {
    expect(fillHermesModel({ id: "m" }, { inputModalities: ["text"] })).toEqual(
      { id: "m" },
    );
    expect(
      fillHermesModel({ id: "m", supports_vision: false }, metadata),
    ).toEqual({ id: "m", context_length: 262144, supports_vision: false });
  });
});

describe("fillOpenCodeModel", () => {
  it("fills missing limits, modalities and reasoning", () => {
    expect(fillOpenCodeModel({ name: "m" }, metadata)).toEqual({
      name: "m",
      limit: { context: 262144, output: 32768 },
      reasoning: true,
      modalities: { input: ["text", "image", "video"], output: ["text"] },
      // models.dev 的档位名字直接搬过来，`turbo` 这种 OpenCode 不认的丢掉。
      variants: {
        low: { reasoningEffort: "low" },
        high: { reasoningEffort: "high" },
        max: { reasoningEffort: "max" },
      },
    });
  });

  it("only fills reasoning towards true and keeps an explicit value", () => {
    expect(fillOpenCodeModel({ name: "m" }, { reasoning: false })).toEqual({
      name: "m",
    });
    expect(
      fillOpenCodeModel({ name: "m", reasoning: false }, { reasoning: true }),
    ).toEqual({ name: "m", reasoning: false });
  });

  it("only fills the missing half of limit", () => {
    expect(
      fillOpenCodeModel(
        {
          name: "m",
          limit: { context: 1000 },
          modalities: { input: ["text"] },
        },
        metadata,
      ),
    ).toEqual({
      name: "m",
      limit: { context: 1000, output: 32768 },
      // 已经写下的 `["text"]` 补上 models.dev 还有的 image/video，不是原样跳过。
      modalities: { input: ["text", "image", "video"], output: ["text"] },
      reasoning: true,
      variants: {
        low: { reasoningEffort: "low" },
        high: { reasoningEffort: "high" },
        max: { reasoningEffort: "max" },
      },
    });
  });

  it("leaves the model untouched when nothing is known", () => {
    expect(fillOpenCodeModel({ name: "m" }, {})).toEqual({ name: "m" });
  });
});

describe("fillOpenCodeModel — DSH dialect", () => {
  const dsh56: KnownModelMetadata = {
    contextWindow: 272_000,
    maxOutputTokens: 128_000,
    reasoning: true,
    reasoningEfforts: ["none", "low", "medium", "high", "xhigh", "max"],
    inputModalities: ["text", "image", "pdf"],
    outputModalities: ["text"],
  };

  it("keeps DSH within the modalities and levels it can actually load", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", modalities: { input: ["text", "pdf"] } },
      dsh56,
      "dsh",
    );

    // DSH 的 MODALITIES 只有 text / image，pdf 写进去整份配置会被拒。
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
      dsh56,
      "dsh",
    );

    expect(Object.keys(filled.reasoningEfforts as object)).toEqual(
      expect.arrayContaining(["low", "high", "max"]),
    );
  });

  it("adds thinking levels when the model switches reasoning on", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", reasoningEfforts: false },
      dsh56,
      "dsh",
    );

    expect(filled.reasoningEfforts).toMatchObject({ off: null, high: "high" });
  });

  it("leaves an explicit level value alone while filling the gaps", () => {
    const filled = fillOpenCodeModel(
      { name: "gpt-5.6-sol", reasoningEfforts: { off: null, high: "custom" } },
      dsh56,
      "dsh",
    );

    expect(filled.reasoningEfforts).toMatchObject({
      high: "custom",
      low: "low",
    });
  });
});

describe("fillKimiModel", () => {
  it("replaces a placeholder window and adds supported capabilities", () => {
    expect(
      fillKimiModel(
        { model: "m", max_context_size: 1, capabilities: ["tool_use"] },
        metadata,
        { replaceContext: true },
      ),
    ).toEqual({
      model: "m",
      max_context_size: 262144,
      capabilities: ["tool_use", "image_in", "thinking"],
    });
  });

  it("keeps a window the user set and never removes capabilities", () => {
    const model = {
      model: "m",
      max_context_size: 1000,
      capabilities: ["thinking", "video_in"],
    };
    expect(
      fillKimiModel(
        model,
        { contextWindow: 9, inputModalities: ["text"], reasoning: false },
        { replaceContext: false },
      ),
    ).toEqual(model);
  });
});

describe("piThinkingLevelMapFromEfforts", () => {
  it("follows Pi's own models.dev rule", () => {
    expect(piThinkingLevelMapFromEfforts(["high", "max"])).toEqual({
      off: null,
      minimal: null,
      low: null,
      medium: null,
      high: "high",
      xhigh: null,
      max: "max",
    });
    expect(piThinkingLevelMapFromEfforts(["none", "low"])).toMatchObject({
      off: "none",
      low: "low",
      high: null,
    });
  });

  it("generates nothing without a Pi level", () => {
    expect(piThinkingLevelMapFromEfforts(undefined)).toBeUndefined();
    expect(piThinkingLevelMapFromEfforts(["default"])).toBeUndefined();
  });
});

describe("piSendsReasoningEffort", () => {
  const base = {
    api: "openai-completions",
    baseUrl: "https://relay.example.com/v1",
    providerId: "relay",
    compat: {},
  };

  it("accepts Responses and plain Chat Completions endpoints", () => {
    expect(piSendsReasoningEffort({ ...base, api: "openai-responses" })).toBe(
      true,
    );
    expect(piSendsReasoningEffort(base)).toBe(true);
  });

  it("accepts Pi's native Mistral protocol", () => {
    expect(
      piSendsReasoningEffort({
        ...base,
        api: "mistral-conversations",
        baseUrl: "https://api.mistral.ai",
      }),
    ).toBe(true);
  });

  it("rejects protocols and endpoints with their own thinking format", () => {
    expect(piSendsReasoningEffort({ ...base, api: "anthropic-messages" })).toBe(
      false,
    );
    for (const baseUrl of [
      "https://api.deepseek.com",
      "https://open.bigmodel.cn/api/paas/v4",
      "https://openrouter.ai/api/v1",
      "https://api.moonshot.cn/v1",
    ]) {
      expect(piSendsReasoningEffort({ ...base, baseUrl })).toBe(false);
    }
    expect(piSendsReasoningEffort({ ...base, providerId: "zai" })).toBe(false);
  });

  it("lets explicit compat override the detection", () => {
    expect(
      piSendsReasoningEffort({
        ...base,
        baseUrl: "https://api.deepseek.com",
        compat: { thinkingFormat: "openai" },
      }),
    ).toBe(true);
    expect(
      piSendsReasoningEffort({
        ...base,
        compat: { supportsReasoningEffort: false },
      }),
    ).toBe(false);
  });
});

describe("piPresetThinkingFor", () => {
  const piThinking = {
    thinkingLevelMap: { low: "low", high: "high" },
    api: "openai-completions",
    baseUrl: "https://api.moonshot.cn/v1",
    compat: { supportsReasoningEffort: true, thinkingFormat: "openai" },
  };
  const effective = {
    api: "openai-completions",
    baseUrl: "https://api.moonshot.cn/v1",
    providerId: "my-moonshot",
    compat: {},
  };

  it("returns the map with the compat it still needs", () => {
    expect(
      piPresetThinkingFor(piThinking, {
        ...effective,
        compat: { thinkingFormat: "openai" },
      }),
    ).toEqual({
      map: { low: "low", high: "high" },
      missingCompat: { supportsReasoningEffort: true },
    });
  });

  it("skips the map under another protocol or conflicting compat", () => {
    expect(
      piPresetThinkingFor(piThinking, {
        ...effective,
        api: "anthropic-messages",
      }),
    ).toBeNull();
    expect(
      piPresetThinkingFor(piThinking, {
        ...effective,
        compat: { supportsReasoningEffort: false },
      }),
    ).toBeNull();
    expect(piPresetThinkingFor(undefined, effective)).toBeNull();
  });

  it("checks the behavior a preset relies on through Pi's own detection", () => {
    // OpenCode Go 的预设没写 compat，靠 Pi 按地址探测出会发 reasoning_effort。
    const relying = {
      thinkingLevelMap: { high: "high", max: "max" },
      api: "openai-completions",
      baseUrl: "https://opencode.ai/zen/go/v1",
    };
    const goEndpoint = {
      ...effective,
      baseUrl: "https://opencode.ai/zen/go/v1",
    };
    expect(piPresetThinkingFor(relying, goEndpoint)?.map).toEqual({
      high: "high",
      max: "max",
    });
    expect(
      piPresetThinkingFor(relying, {
        ...goEndpoint,
        compat: { supportsReasoningEffort: false },
      }),
    ).toBeNull();
  });
});

describe("fillWorkBuddyModel", () => {
  const url = "https://api.example.test/v1";
  const efforts = {
    reasoning: {
      supportedEfforts: ["low", "high", "max"],
      defaultEffort: "high",
      canDisableThinking: false,
    },
    thinkingLevelMap: {
      off: null,
      minimal: null,
      low: "low",
      medium: null,
      high: "high",
      xhigh: null,
      max: "max",
    },
  };

  it("fills blank limits and only turns capabilities on", () => {
    expect(
      fillWorkBuddyModel(
        { id: "m", supportsToolCall: true, supportsImages: false },
        metadata,
        url,
      ),
    ).toEqual({
      id: "m",
      supportsToolCall: true,
      supportsImages: true,
      supportsReasoning: true,
      maxInputTokens: 262144,
      maxOutputTokens: 32768,
      ...efforts,
    });
    expect(
      fillWorkBuddyModel(
        { id: "m", maxInputTokens: 1000, supportsReasoning: true },
        { reasoning: false, contextWindow: 2000 },
        url,
      ),
    ).toEqual({ id: "m", maxInputTokens: 1000, supportsReasoning: true });
  });

  it("derives effort levels only where they reach the upstream", () => {
    // Moonshot 不认 reasoning_effort（Pi 的探测规则），不列强度
    expect(
      fillWorkBuddyModel({ id: "m" }, metadata, "https://api.moonshot.cn/v1"),
    ).not.toHaveProperty("reasoning");
    // 已有的强度或映射不动
    const kept = {
      id: "m",
      supportsReasoning: true,
      thinkingLevelMap: { max: "max" },
    };
    expect(fillWorkBuddyModel(kept, metadata, url)).toEqual({
      ...kept,
      supportsImages: true,
      maxInputTokens: 262144,
      maxOutputTokens: 32768,
    });
  });
});
