import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Plus, Trash2 } from "lucide-react";
import type { Settings } from "@/types";
import {
  modelsDevQueryOptions,
  normalizeModelsDevModelId,
  type ModelsDevModel,
  type ModelsDevResponse,
} from "@/lib/modelsDev";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  SettingsBlock,
  SettingsCard,
  SettingsRow,
} from "@/components/settings/SettingsLayout";

type Override = NonNullable<Settings["modelMetadataOverrides"]>[string];

const EFFORT_LEVELS = new Set([
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
]);
const INPUT_MODALITIES = new Set(["text", "image", "audio", "video", "pdf"]);

/** models.dev 的条目转成覆盖值：用户点「添加」时就带着这些默认值。 */
function overrideFromModelsDev(model: ModelsDevModel): Override {
  const override: Override = {};
  if (model.limit?.context) override.contextWindow = model.limit.context;
  if (model.limit?.output) override.maxOutputTokens = model.limit.output;
  const modalities = (model.modalities?.input ?? []).filter((value) =>
    INPUT_MODALITIES.has(value),
  );
  if (modalities.length > 0) override.inputModalities = modalities;
  const efforts = (model.reasoning_options ?? [])
    .flatMap((option) => option.values ?? [])
    .filter((value) => EFFORT_LEVELS.has(value));
  if (efforts.length > 0) override.reasoningEfforts = [...new Set(efforts)];
  if (model.reasoning !== undefined) override.reasoning = model.reasoning;
  return override;
}

interface CatalogRow {
  key: string;
  id: string;
  name: string;
  provider: string;
  override: Override;
}

/** 把 models.dev 的 200+ 家供应商摊平，同一个模型只留第一次出现的那个。 */
function flattenCatalog(catalog: ModelsDevResponse | undefined): CatalogRow[] {
  if (!catalog) return [];
  const seen = new Set<string>();
  const rows: CatalogRow[] = [];
  for (const [providerKey, provider] of Object.entries(catalog)) {
    const providerName = provider.name ?? provider.id ?? providerKey;
    for (const [modelKey, model] of Object.entries(provider.models ?? {})) {
      const id = model.id ?? modelKey;
      const key = normalizeModelsDevModelId(id);
      if (!key || seen.has(key)) continue;
      seen.add(key);
      rows.push({
        key,
        id,
        name: model.name ?? id,
        provider: providerName,
        override: overrideFromModelsDev(model),
      });
    }
  }
  return rows;
}

const toNumber = (value: string): number | undefined => {
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
};

const toList = (value: string): string[] | undefined => {
  const items = value
    .split(/[,，\s]+/)
    .map((item) => item.trim().toLowerCase())
    .filter(Boolean);
  return items.length > 0 ? [...new Set(items)] : undefined;
};

/**
 * 设置 → 模型覆盖：给单个模型定下默认参数。
 *
 * 补全模型信息（「获取模型列表」「更新模型信息」）时先用这里的值，没填的字段
 * 继续按同地址预设和 models.dev 补，所以只覆盖真正想改的那一项就行。
 */
export function ModelOverridesSection({
  settings,
  onAutoSave,
}: {
  settings: Settings;
  onAutoSave: (updates: Partial<Settings>) => void | Promise<boolean>;
}) {
  const { t } = useTranslation();
  const { data: catalog, isLoading } = useQuery(modelsDevQueryOptions);
  const saved = settings.modelMetadataOverrides ?? {};
  const [draft, setDraft] = useState<Record<string, Override>>(saved);
  const [query, setQuery] = useState("");
  const [browsing, setBrowsing] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => {
    setDraft(settings.modelMetadataOverrides ?? {});
  }, [settings.modelMetadataOverrides]);
  useEffect(() => () => clearTimeout(timer.current), []);

  const commit = (next: Record<string, Override>) => {
    setDraft(next);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      void onAutoSave({ modelMetadataOverrides: next });
    }, 400);
  };

  const patch = (key: string, changes: Override) => {
    const next = { ...draft };
    const merged = { ...next[key], ...changes };
    for (const field of Object.keys(merged) as (keyof Override)[]) {
      if (merged[field] === undefined) delete merged[field];
    }
    next[key] = merged;
    commit(next);
  };

  const rows = useMemo(() => flattenCatalog(catalog), [catalog]);
  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!browsing && !needle) return [];
    if (!needle) {
      return [...rows].sort((a, b) => a.id.localeCompare(b.id)).slice(0, 30);
    }
    return rows
      .filter(
        (row) =>
          row.id.toLowerCase().includes(needle) ||
          row.name.toLowerCase().includes(needle) ||
          row.provider.toLowerCase().includes(needle),
      )
      .slice(0, 30);
  }, [browsing, query, rows]);

  const add = (row: CatalogRow) => {
    setQuery("");
    setBrowsing(false);
    if (draft[row.key]) return;
    commit({ ...draft, [row.key]: { ...row.override } });
  };

  const remove = (key: string) => {
    const next = { ...draft };
    delete next[key];
    commit(next);
  };

  const keys = Object.keys(draft).sort();

  return (
    <SettingsBlock
      title={t("settings.modelOverrides.title", {
        defaultValue: "模型覆盖",
      })}
      help={{
        title: t("settings.modelOverrides.title", {
          defaultValue: "模型覆盖",
        }),
        body: t("settings.modelOverrides.help", {
          defaultValue:
            "给单个模型定下默认参数。补全模型信息时先用这里填的值，没填的字段继续按同地址预设和 models.dev 补，所以只填想改的那一项就行。键是模型 id，不区分大小写和 vendor/ 前缀。",
        }),
      }}
      actions={
        <Button
          variant="quiet"
          size="sm"
          disabled={isLoading}
          onClick={() => setBrowsing((open) => !open)}
        >
          {t("settings.modelOverrides.browse", { defaultValue: "浏览模型库" })}
        </Button>
      }
    >
      <SettingsCard>
        <SettingsRow
          label={t("settings.modelOverrides.search", {
            defaultValue: "从 models.dev 添加",
          })}
          description={t("settings.modelOverrides.searchHint", {
            defaultValue:
              "输入模型 id、名字或供应商筛选；添加后可以逐项改，改完自动保存。",
          })}
        >
          <div className="space-y-2">
            <Input
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setBrowsing(true);
              }}
              placeholder={t("settings.modelOverrides.searchPlaceholder", {
                defaultValue: "例如 gpt-5.6-sol",
              })}
            />
            {(browsing || query.trim().length > 0) && (
              <div className="max-h-64 space-y-1 overflow-y-auto">
                {matches.length === 0 && (
                  <div className="px-1 py-2 text-caption text-fg-3">
                    {isLoading
                      ? t("settings.modelOverrides.loading", {
                          defaultValue: "正在读取模型库…",
                        })
                      : t("settings.modelOverrides.noMatch", {
                          defaultValue: "没有匹配的模型",
                        })}
                  </div>
                )}
                {matches.map((row) => (
                  <button
                    key={row.key}
                    type="button"
                    onClick={() => add(row)}
                    className="flex w-full items-center gap-2 rounded-control px-2 py-1.5 text-start hover:bg-subtle"
                  >
                    <Plus className="h-3.5 w-3.5 shrink-0 text-fg-3" />
                    <span className="min-w-0 flex-1 truncate text-body text-fg-1">
                      {row.id}
                    </span>
                    <span className="shrink-0 text-caption text-fg-3">
                      {row.provider}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </SettingsRow>
      </SettingsCard>

      {keys.length === 0 ? (
        <div className="rounded-panel border border-dashed border-border px-4 py-6 text-center text-caption text-fg-3">
          {t("settings.modelOverrides.empty", {
            defaultValue: "还没有覆盖任何模型。",
          })}
        </div>
      ) : (
        <div className="space-y-3">
          {keys.map((key) => {
            const entry = draft[key];
            return (
              <div
                key={key}
                className="rounded-panel border border-border bg-surface"
              >
                <div className="flex items-center gap-2 border-b border-border px-4 py-2.5">
                  <span className="min-w-0 flex-1 truncate font-mono text-body text-fg-1">
                    {key}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t("settings.modelOverrides.remove", {
                      defaultValue: "删除",
                    })}
                    onClick={() => remove(key)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
                <div className="grid grid-cols-2 gap-3 px-4 py-3">
                  <label className="space-y-1">
                    <span className="block text-caption text-fg-2">
                      {t("settings.modelOverrides.contextWindow", {
                        defaultValue: "上下文窗口",
                      })}
                    </span>
                    <Input
                      inputMode="numeric"
                      value={entry.contextWindow?.toString() ?? ""}
                      placeholder="272000"
                      onChange={(event) =>
                        patch(key, {
                          contextWindow: toNumber(event.target.value),
                        })
                      }
                    />
                  </label>
                  <label className="space-y-1">
                    <span className="block text-caption text-fg-2">
                      {t("settings.modelOverrides.maxOutputTokens", {
                        defaultValue: "最大输出",
                      })}
                    </span>
                    <Input
                      inputMode="numeric"
                      value={entry.maxOutputTokens?.toString() ?? ""}
                      placeholder="128000"
                      onChange={(event) =>
                        patch(key, {
                          maxOutputTokens: toNumber(event.target.value),
                        })
                      }
                    />
                  </label>
                  <label className="space-y-1">
                    <span className="block text-caption text-fg-2">
                      {t("settings.modelOverrides.inputModalities", {
                        defaultValue: "输入模态",
                      })}
                    </span>
                    <Input
                      value={entry.inputModalities?.join(", ") ?? ""}
                      placeholder="text, image"
                      onChange={(event) =>
                        patch(key, {
                          inputModalities: toList(event.target.value),
                        })
                      }
                    />
                  </label>
                  <label className="space-y-1">
                    <span className="block text-caption text-fg-2">
                      {t("settings.modelOverrides.reasoningEfforts", {
                        defaultValue: "思考档位",
                      })}
                    </span>
                    <Input
                      value={entry.reasoningEfforts?.join(", ") ?? ""}
                      placeholder="low, medium, high"
                      onChange={(event) =>
                        patch(key, {
                          reasoningEfforts: toList(event.target.value),
                        })
                      }
                    />
                  </label>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </SettingsBlock>
  );
}
