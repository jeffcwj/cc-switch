import type { AppId, SyncTargetApp } from "@/types";

export interface SyncTargetOption {
  target: SyncTargetApp;
  /** i18n key of the menu label. */
  labelKey: string;
}

/**
 * 卡片可以把供应商复制到哪些应用。
 *
 * OpenCode 能喂给所有下游；Claude Code 和 Claude Desktop 是同一个网关的两种形态，
 * 可以互相复制（只有这两家互为来源，避免出现没人验证过的组合）。
 */
const SYNC_TARGETS: Partial<Record<AppId, SyncTargetOption[]>> = {
  opencode: [
    { target: "dsh", labelKey: "provider.syncToDsh" },
    { target: "codex", labelKey: "provider.syncToCodex" },
    { target: "claude", labelKey: "provider.syncToClaude" },
    { target: "claude-desktop", labelKey: "provider.syncToClaudeDesktop" },
  ],
  claude: [
    { target: "claude-desktop", labelKey: "provider.syncToClaudeDesktop" },
  ],
  "claude-desktop": [{ target: "claude", labelKey: "provider.syncToClaude" }],
};

export function syncTargetsFor(appId: AppId): SyncTargetOption[] {
  return SYNC_TARGETS[appId] ?? [];
}
