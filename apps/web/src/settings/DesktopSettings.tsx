// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop-only parts of Settings (`docs/ux/desktop.md` sections 3.1 and
 * 5): the Notifications section with the two switches the host keeps on this
 * computer, and the keyboard shortcut list under About. Settings renders
 * neither outside the desktop app.
 */
import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { getHostSettings, setHostSettings, type HostSettings } from "../platform/host";
import { isMacPlatform, shortcutKeys } from "../platform/shortcuts";
import { Icon, type IconName } from "../ui/icons";

type SwitchKey = keyof HostSettings;

const SWITCHES: ReadonlyArray<{
  key: SwitchKey;
  icon: IconName;
  titleKey: string;
  subKey: string | null;
}> = [
  {
    key: "keepRunning",
    icon: "bell",
    titleKey: "settings.desktop.keep_running.title",
    subKey: "settings.desktop.keep_running.sub",
  },
  {
    key: "openAtLogin",
    icon: "monitor",
    titleKey: "settings.desktop.open_at_login.title",
    subKey: null,
  },
];

/** Settings > Notifications on the desktop. */
export function DesktopNotificationsSection(): JSX.Element {
  const [settings, setSettings] = createSignal<HostSettings | null>(null);
  const [busy, setBusy] = createSignal<SwitchKey | null>(null);
  const [failed, setFailed] = createSignal<SwitchKey | "read" | null>(null);

  onMount(() => {
    getHostSettings()
      .then(setSettings)
      .catch(() => setFailed("read"));
  });

  async function toggle(key: SwitchKey): Promise<void> {
    const cur = settings();
    if (cur === null || busy() !== null) return;
    setBusy(key);
    setFailed(null);
    try {
      setSettings(await setHostSettings({ [key]: !cur[key] }));
    } catch {
      // The switch stays where it was: the host did not take the change.
      setFailed(key);
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.notifications")}</h1>
        <p>{t("settings.desktop.lede")}</p>
      </div>
      <section class="lk-card">
        <For each={SWITCHES}>
          {(s) => {
            const on = (): boolean => settings()?.[s.key] ?? false;
            return (
              <>
                <div class="lk-card-row">
                  <span class="lk-card-row-icon">
                    <Icon name={s.icon} size={18} />
                  </span>
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{t(s.titleKey)}</span>
                    <Show when={s.subKey}>
                      {(k) => <span class="lk-card-row-sub">{t(k())}</span>}
                    </Show>
                  </div>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={on()}
                    class="lk-card-row-action"
                    classList={{ primary: on() }}
                    disabled={settings() === null || busy() !== null}
                    onClick={() => void toggle(s.key)}
                  >
                    {on() ? t("common.on") : t("common.off")}
                  </button>
                </div>
                <Show when={failed() === s.key}>
                  <p class="form-error" role="alert">
                    {t("settings.desktop.save_failed")}
                  </p>
                </Show>
              </>
            );
          }}
        </For>
        <Show when={failed() === "read"}>
          <p class="form-error" role="alert">
            {t("settings.desktop.read_failed")}
          </p>
        </Show>
      </section>
    </>
  );
}

/** The shortcut list, `docs/ux/desktop.md` section 5, as this build has it. */
export function shortcutRows(mac: boolean): ReadonlyArray<{ keys: string; labelKey: string }> {
  const { mod, alt } = shortcutKeys(mac);
  const rows = [
    { keys: `${mod}+K`, labelKey: "settings.about.shortcuts.search" },
    { keys: `${mod}+N`, labelKey: "settings.about.shortcuts.new_chat" },
    { keys: `${mod}+Shift+1 … ${mod}+Shift+5`, labelKey: "settings.about.shortcuts.destinations" },
    { keys: `${mod}+1 … ${mod}+9`, labelKey: "settings.about.shortcuts.folders" },
    { keys: `${alt}+↑ / ${alt}+↓`, labelKey: "settings.about.shortcuts.previous_next" },
    { keys: `${mod}+,`, labelKey: "settings.about.shortcuts.settings" },
    { keys: "Esc", labelKey: "settings.about.shortcuts.escape" },
    { keys: "Enter / Shift+Enter", labelKey: "settings.about.shortcuts.send" },
    { keys: "↑", labelKey: "settings.about.shortcuts.edit_last" },
    { keys: `${mod}+W`, labelKey: "settings.about.shortcuts.close_window" },
  ];
  // Quit is Cmd+Q on a Mac; elsewhere it is in the tray menu, not a key.
  if (mac) rows.push({ keys: `${mod}+Q`, labelKey: "settings.about.shortcuts.quit" });
  return rows;
}

/** Settings > About > Keyboard shortcuts, on the desktop. */
export function KeyboardShortcutsCard(): JSX.Element {
  const rows = shortcutRows(isMacPlatform());
  return (
    <section class="lk-card">
      <h2>{t("settings.about.shortcuts.title")}</h2>
      <For each={rows}>
        {(r) => (
          <div class="lk-card-row">
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t(r.labelKey)}</span>
              <span class="lk-card-row-sub">
                <kbd>{r.keys}</kbd>
              </span>
            </div>
          </div>
        )}
      </For>
    </section>
  );
}
