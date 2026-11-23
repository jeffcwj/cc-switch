import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import type {
  ResolvedDirectories,
  SettingsFormState,
} from "@/hooks/useSettings";
import type { Settings } from "@/types";
import { settingsApi } from "@/lib/api";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { DirectoryInput } from "@/components/settings/DirectorySettings";
import {
  DIRECTORY_APP,
  DIRECTORY_APPS,
  type DirectoryAppId,
  sanitizeDir,
} from "@/config/directoryApps";
import { CodexAuthSettings } from "@/components/settings/CodexAuthSettings";
import { APP_DISPLAY_NAME, AppGlyph } from "@/components/shell/AppGlyph";
import {
  SettingsBlock,
  SettingsCard,
  SettingsRow,
  SettingsSwitchRow,
} from "@/components/settings/SettingsLayout";

// Radix 的 Select 不接受空字符串值，用哨兵代表「自动」。
const AUTO_PROFILE = "__auto__";

interface AppConfigSectionProps {
  settings: SettingsFormState;
  savedSettings?: Settings;
  resolvedDirs: ResolvedDirectories;
  isSaving: boolean;
  onAutoSave: (updates: Partial<SettingsFormState>) => Promise<boolean>;
  onDirectoryChange: (app: DirectoryAppId, value?: string) => void;
  onBrowseDirectory: (app: DirectoryAppId) => Promise<void>;
  onResetDirectory: (app: DirectoryAppId) => Promise<void>;
  onSaveDirectories: () => Promise<void>;
}

/**
 * 设置 → 应用配置：每个应用一节，放只属于它的开关和配置目录。
 * 目录改动要点「保存」才写入（各节自己的保存按钮，保存时一并写入所有改过的目录）。
 */
export function AppConfigSection({
  settings,
  savedSettings,
  resolvedDirs,
  isSaving,
  onAutoSave,
  onDirectoryChange,
  onBrowseDirectory,
  onResetDirectory,
  onSaveDirectories,
}: AppConfigSectionProps) {
  const { t } = useTranslation();

  // DSH 的 profile 列表跟着配置目录走：换了目录，可选的 profile 也跟着换。
  const [dshProfiles, setDshProfiles] = useState<string[]>([]);
  useEffect(() => {
    let cancelled = false;
    void settingsApi
      .listDshProfiles()
      .then((profiles) => {
        if (!cancelled) setDshProfiles(profiles);
      })
      .catch(() => {
        if (!cancelled) setDshProfiles([]);
      });
    return () => {
      cancelled = true;
    };
  }, [settings.dshConfigDir]);

  const isDirty = (app: DirectoryAppId) => {
    const { field } = DIRECTORY_APP[app];
    return sanitizeDir(settings[field]) !== sanitizeDir(savedSettings?.[field]);
  };

  const directoryRow = (app: DirectoryAppId) => (
    <SettingsRow
      label={t("settings.appConfig.configDir")}
      help={{
        title: t("settings.appConfig.configDir"),
        body: t("settings.configDirectoryDescription"),
      }}
    >
      <DirectoryInput
        label=""
        value={settings[DIRECTORY_APP[app].field]}
        resolvedValue={resolvedDirs[app]}
        placeholder={t(DIRECTORY_APP[app].placeholderKey)}
        onChange={(value) => onDirectoryChange(app, value)}
        onBrowse={() => onBrowseDirectory(app)}
        onReset={() => onResetDirectory(app)}
      />
    </SettingsRow>
  );

  // DSH 会有好几份 profile（web / desktop / 具体版本目录），CC Switch 只读写其中一份。
  const dshProfileRow = () => {
    const pinned = settings.dshProfile ?? "";
    const options =
      pinned && !dshProfiles.includes(pinned)
        ? [pinned, ...dshProfiles]
        : dshProfiles;
    return (
      <SettingsRow
        label={t("settings.dshProfile")}
        help={{
          title: t("settings.dshProfile"),
          body: t("settings.dshProfileDescription"),
        }}
      >
        <Select
          value={pinned || AUTO_PROFILE}
          onValueChange={(value) =>
            void onAutoSave({
              dshProfile: value === AUTO_PROFILE ? undefined : value,
            })
          }
        >
          <SelectTrigger className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={AUTO_PROFILE}>
              {t("settings.dshProfileAuto")}
            </SelectItem>
            {options.map((profile) => (
              <SelectItem key={profile} value={profile}>
                {profile}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsRow>
    );
  };

  const saveButton = (app: DirectoryAppId) =>
    isDirty(app) ? (
      <Button
        variant="solid"
        size="compact"
        disabled={isSaving}
        onClick={() => void onSaveDirectories()}
      >
        {isSaving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
        {t("common.save")}
      </Button>
    ) : null;

  const blockTitle = (app: DirectoryAppId) => (
    <span className="flex items-center gap-2">
      <AppGlyph app={app} size={16} badgeClassName="bg-app" />
      {APP_DISPLAY_NAME[app]}
    </span>
  );

  return (
    <>
      {DIRECTORY_APPS.map(({ app }) => (
        <SettingsBlock
          key={app}
          id={`app-config-${app}`}
          className="rounded-panel"
          title={blockTitle(app)}
          actions={saveButton(app)}
        >
          <SettingsCard>
            {app === "claude" && (
              <>
                <SettingsSwitchRow
                  label={t("settings.enableClaudePluginIntegration")}
                  help={{
                    title: t("settings.enableClaudePluginIntegration"),
                    body: t(
                      "settings.enableClaudePluginIntegrationDescription",
                    ),
                  }}
                  checked={!!settings.enableClaudePluginIntegration}
                  onCheckedChange={(value) =>
                    void onAutoSave({ enableClaudePluginIntegration: value })
                  }
                />
                <SettingsSwitchRow
                  label={t("settings.skipClaudeOnboarding")}
                  help={{
                    title: t("settings.skipClaudeOnboarding"),
                    body: t("settings.skipClaudeOnboardingDescription"),
                  }}
                  checked={!!settings.skipClaudeOnboarding}
                  onCheckedChange={(value) =>
                    void onAutoSave({ skipClaudeOnboarding: value })
                  }
                />
              </>
            )}
            {app === "codex" && (
              <CodexAuthSettings
                settings={settings}
                onChange={onAutoSave}
                codexConfigDir={savedSettings?.codexConfigDir}
              />
            )}
            {directoryRow(app)}
            {app === "dsh" && dshProfileRow()}
          </SettingsCard>
        </SettingsBlock>
      ))}
    </>
  );
}
