// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/settings/:section?` — Profile · Security · Appearance · Data · Account
 * (design system §4). A section list on the left, the section on the right;
 * a narrow screen shows one of the two. `/settings` lands on Profile.
 */
import { Show, type JSX } from "solid-js";
import { A, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import PasskeysSection from "../auth/PasskeysSection";
import { AccountCard, ExportDataCard } from "../auth/AccountSections";
import { autoplayVideoNotes, setAutoplayVideoNotes } from "../media/media-prefs";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon, type IconName } from "../ui/icons";
import { setTheme, useTheme, type Theme } from "../ui/theme";
import Profile from "./Profile";
import { paths } from "./paths";

type Section = "profile" | "security" | "appearance" | "data" | "account";

const SECTIONS: ReadonlyArray<{ id: Section; label: string; icon: IconName }> = [
  { id: "profile", label: "Profile", icon: "user" },
  { id: "security", label: "Security", icon: "shield" },
  { id: "appearance", label: "Appearance", icon: "palette" },
  { id: "data", label: "Data", icon: "db" },
  { id: "account", label: "Account", icon: "logout" },
];

function isSection(s: string | undefined): s is Section {
  return SECTIONS.some((x) => x.id === s);
}

export default function Settings(): JSX.Element {
  const params = useParams<{ section?: string }>();
  const auth = useAuth();
  const profiles = useProfiles();

  // `/settings` (no section) shows the list alone on a narrow screen and
  // Profile beside it on a wide one.
  const section = (): Section | null => (isSection(params.section) ? params.section : null);
  const shown = (): Section => section() ?? "profile";

  const username = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };
  const displayName = (): string => profiles.ownProfile().displayName ?? "";

  return (
    <div class="lk-settings" data-open={section() === null ? "false" : "true"}>
      <nav class="lk-settings-nav" aria-label="Settings">
        <h1>Settings</h1>
        <div class="lk-settings-me">
          <Avatar
            label={displayName().length > 0 ? displayName() : username()}
            id={username()}
            size="md"
          />
          <div>
            <div class="lk-settings-me-name">
              {displayName().length > 0 ? displayName() : username().length > 0 ? `@${username()}` : "Guest"}
            </div>
            <Show when={displayName().length > 0 && username().length > 0}>
              <div class="lk-settings-me-user">@{username()}</div>
            </Show>
          </div>
        </div>
        {SECTIONS.map((s) => (
          <A
            href={paths.settingsSection(s.id)}
            classList={{ active: shown() === s.id && section() !== null }}
            activeClass=""
            inactiveClass=""
          >
            <Icon name={s.icon} size={20} />
            <span>{s.label}</span>
          </A>
        ))}
      </nav>
      <div class="lk-settings-body">
        <div class="lk-page lk-stack">
          <Show when={shown() === "profile"}>
            <Profile />
          </Show>
          <Show when={shown() === "security"}>
            <SecuritySection />
          </Show>
          <Show when={shown() === "appearance"}>
            <AppearanceSection />
          </Show>
          <Show when={shown() === "data"}>
            <div class="lk-page-head">
              <h1>Data</h1>
              <p>What Lakko's servers hold, and how to take it with you.</p>
            </div>
            <ExportDataCard />
          </Show>
          <Show when={shown() === "account"}>
            <div class="lk-page-head">
              <h1>Account</h1>
              <p>Signing out, this device, and deleting your account.</p>
            </div>
            <AccountCard />
          </Show>
        </div>
      </div>
    </div>
  );
}

function SecuritySection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>Security</h1>
        <p>How you sign in and which devices can read your messages.</p>
      </div>
      <PasskeysSection />
      <section class="lk-card">
        <h2>Devices and recovery</h2>
        <p class="lk-card-desc">
          Every linked device holds its own keys and can read your messages.
          Two-step verification and a recovery contact are set up on the same
          page.
        </p>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="devices" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Linked devices</span>
            <span class="lk-card-row-sub">See, link and remove devices.</span>
          </div>
          <A href={paths.devices} class="lk-card-row-action">
            Manage
          </A>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="lock" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Two-step verification and recovery contact</span>
            <span class="lk-card-row-sub">
              Extra ways to get back into your account if you lose your password.
            </span>
          </div>
          <A href={paths.devices} class="lk-card-row-action">
            Set up
          </A>
        </div>
      </section>
    </>
  );
}

function AppearanceSection(): JSX.Element {
  const options: ReadonlyArray<{ value: Theme; label: string; icon: IconName }> = [
    { value: "system", label: "System", icon: "monitor" },
    { value: "light", label: "Light", icon: "sun" },
    { value: "dark", label: "Dark", icon: "moon" },
  ];
  return (
    <>
      <div class="lk-page-head">
        <h1>Appearance</h1>
        <p>Theme and media playback on this device.</p>
      </div>
      <section class="lk-card">
        <h2>Theme</h2>
        <div class="lk-segmented" role="radiogroup" aria-label="Theme">
          {options.map((o) => (
            <label>
              <input
                type="radio"
                name="theme"
                value={o.value}
                checked={useTheme() === o.value}
                onChange={() => setTheme(o.value)}
              />
              <Icon name={o.icon} size={16} />
              {o.label}
            </label>
          ))}
        </div>
      </section>
      <section class="lk-card">
        <h2>Media</h2>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="video" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Autoplay video notes</span>
            <span class="lk-card-row-sub">
              Incoming video notes start playing, muted, as they scroll into view.
              Off by default because autoplay downloads media.
            </span>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={autoplayVideoNotes()}
            class="lk-card-row-action"
            classList={{ primary: autoplayVideoNotes() }}
            onClick={() => setAutoplayVideoNotes(!autoplayVideoNotes())}
          >
            {autoplayVideoNotes() ? "On" : "Off"}
          </button>
        </div>
      </section>
    </>
  );
}
