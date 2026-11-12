// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop-only parts of Settings (`docs/ux/desktop.md` sections 3.1 and
 * 5): the Notifications section with the two switches the host keeps on this
 * computer, and the keyboard shortcut list under About. Settings renders
 * neither outside the desktop app.
 */
import { reactionNotificationsOn, setReactionNotificationsOn } from "../chat/reaction-notify-pref";
import { createEffect, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import {
  getHostSettings,
  getHostTunnel,
  setHostSettings,
  setHostTunnel,
  type HostSettings,
  type HostTunnelPhase,
  type HostTunnelState,
} from "../platform/host";
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
    icon: "laptop",
    titleKey: "settings.desktop.open_at_login.title",
    subKey: null,
  },
];

/** How often the state is read while the host is setting the tunnel up. */
const ARMING_POLL_MS = 1000;

/** The status line of each phase (`docs/ux/desktop.md` section 9). */
export function tunnelStatusKeys(phase: HostTunnelPhase): { title: string; sub: string } {
  switch (phase) {
    case "disabled":
      return { title: "common.off", sub: "settings.anti_censorship.status.direct" };
    case "unconfigured":
      return {
        title: "settings.anti_censorship.status.unconfigured",
        sub: "settings.anti_censorship.status.unconfigured.sub",
      };
    case "arming":
      return { title: "common.connecting", sub: "settings.anti_censorship.status.connecting.sub" };
    case "armed":
      return {
        title: "settings.anti_censorship.status.active",
        sub: "settings.anti_censorship.status.active.sub",
      };
    case "unavailable":
      return {
        title: "settings.anti_censorship.status.unavailable",
        sub: "settings.anti_censorship.status.unavailable.sub",
      };
  }
}

/** Settings > Security > Anti-censorship, on the desktop. */
export function AntiCensorshipCard(): JSX.Element {
  const [state, setState] = createSignal<HostTunnelState | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);

  const read = (): void => {
    getHostTunnel()
      .then(setState)
      .catch(() => setFailed(true));
  };
  onMount(read);
  createEffect(() => {
    if (state()?.phase !== "arming") return;
    const timer = setInterval(read, ARMING_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });

  async function toggle(): Promise<void> {
    const cur = state();
    if (cur === null || busy()) return;
    setBusy(true);
    setFailed(false);
    try {
      setState(await setHostTunnel(!cur.enabled));
    } catch {
      // The switch stays where it was: the host did not take the change.
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }

  const status = (): { title: string; sub: string } | null => {
    const cur = state();
    return cur === null ? null : tunnelStatusKeys(cur.phase);
  };

  return (
    <section class="lk-card">
      <h2>{t("settings.anti_censorship.title")}</h2>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="network-shield" size={20} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.anti_censorship.toggle")}</span>
          <span class="lk-card-row-sub">{t("settings.anti_censorship.footer")}</span>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={state()?.enabled === true}
          class="lk-card-row-action"
          classList={{ primary: state()?.enabled === true }}
          disabled={state() === null || busy()}
          onClick={() => void toggle()}
        >
          {t(state()?.enabled === true ? "common.on" : "common.off")}
        </button>
      </div>
      <Show when={status()}>
        {(s) => (
          <div class="lk-card-row" aria-live="polite">
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t(s().title)}</span>
              <span class="lk-card-row-sub">{t(s().sub)}</span>
            </div>
          </div>
        )}
      </Show>
      <Show when={failed()}>
        <p class="form-error" role="alert">
          {t("settings.anti_censorship.status.unavailable.sub")}
        </p>
      </Show>
    </section>
  );
}

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
                    <Icon name={s.icon} size={20} />
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
        <ReactionNotificationsRow />
      </section>
    </>
  );
}

/** `docs/ux/reactions-and-emoji.md` section 12 — this device's filter, in
 *  both builds. */
export function ReactionNotificationsRow(): JSX.Element {
  return (
    <div class="lk-card-row">
      <span class="lk-card-row-icon">
        <Icon name="face-smiling" size={20} />
      </span>
      <div class="lk-card-row-body">
        <span class="lk-card-row-title">{t("settings.notifications.reactions")}</span>
        <span class="lk-card-row-sub">{t("settings.notifications.reactions_sub")}</span>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={reactionNotificationsOn()}
        class="lk-card-row-action"
        classList={{ primary: reactionNotificationsOn() }}
        onClick={() => setReactionNotificationsOn(!reactionNotificationsOn())}
      >
        {reactionNotificationsOn() ? t("common.on") : t("common.off")}
      </button>
    </div>
  );
}

/** The browser build's Notifications section: the reactions switch (what
 *  the system shows is the browser's own setting). */
export function BrowserNotificationsSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.notifications")}</h1>
      </div>
      <section class="lk-card">
        <ReactionNotificationsRow />
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
