// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/settings/:section?` — Profile · Security · Appearance · Media · About ·
 * Account (docs/ux/client-redesign.md section 5.5). A section list on the
 * left, the section on the right; a narrow screen shows one of the two.
 * `/settings` lands on Profile. `recovery` (authenticator, email or phone) is
 * a page under Security rather than a section of its own. Account is last:
 * it holds the export and the danger zone.
 */
import { Show, type JSX } from "solid-js";
import { A, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import PasskeysSection from "../auth/PasskeysSection";
import { AccountCard, ExportDataCard } from "../auth/AccountSections";
import { ContactEnrollView, TotpEnrollView } from "../auth/RecoveryFactors";
import { autoplayVideoNotes, setAutoplayVideoNotes } from "../media/media-prefs";
import { useProfiles } from "../profile/ProfileContext";
import { appLicense, appVersion } from "../ui/app-version";
import { Avatar } from "../ui/Avatar";
import { Icon, type IconName } from "../ui/icons";
import { setTheme, useTheme, type Theme } from "../ui/theme";
import Profile from "./Profile";
import { paths } from "./paths";

type Section = "profile" | "security" | "appearance" | "media" | "about" | "account";
type Page = Section | "recovery";

const SECTIONS: ReadonlyArray<{ id: Section; label: string; icon: IconName }> = [
  { id: "profile", label: "Profile", icon: "user" },
  { id: "security", label: "Security", icon: "shield" },
  { id: "appearance", label: "Appearance", icon: "palette" },
  { id: "media", label: "Media", icon: "video" },
  { id: "about", label: "About", icon: "info" },
  { id: "account", label: "Account", icon: "logout" },
];

function pageOf(s: string | undefined): Page | null {
  if (s === "recovery") return "recovery";
  // The former Data section: its export card now sits under Account.
  if (s === "data") return "account";
  return SECTIONS.some((x) => x.id === s) ? (s as Section) : null;
}

export default function Settings(): JSX.Element {
  const params = useParams<{ section?: string }>();
  const auth = useAuth();
  const profiles = useProfiles();

  // `/settings` (no section) shows the list alone on a narrow screen and
  // Profile beside it on a wide one.
  const page = (): Page | null => pageOf(params.section);
  const shown = (): Page => page() ?? "profile";
  // The list highlights the section a page belongs to.
  const highlighted = (): Section => {
    const p = shown();
    return p === "recovery" ? "security" : p;
  };

  const username = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };
  const displayName = (): string => profiles.ownProfile().displayName ?? "";

  return (
    <div class="lk-settings" data-open={page() === null ? "false" : "true"}>
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
            classList={{ active: highlighted() === s.id && page() !== null }}
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
          <A href={paths.settings} class="ghost small lk-narrow-only" style={{ "align-self": "flex-start" }}>
            <Icon name="back" size={16} />
            Settings
          </A>
          <Show when={shown() === "profile"}>
            <Profile />
          </Show>
          <Show when={shown() === "security"}>
            <SecuritySection />
          </Show>
          <Show when={shown() === "recovery"}>
            <RecoverySection />
          </Show>
          <Show when={shown() === "appearance"}>
            <AppearanceSection />
          </Show>
          <Show when={shown() === "media"}>
            <MediaSection />
          </Show>
          <Show when={shown() === "about"}>
            <AboutSection />
          </Show>
          <Show when={shown() === "account"}>
            <div class="lk-page-head">
              <h1>Account</h1>
              <p>Your data, signing out, this device, and deleting your account.</p>
            </div>
            <ExportDataCard />
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
        <p>How you sign in, which devices can read your messages, and how you get back in.</p>
      </div>
      <PasskeysSection />
      <section class="lk-card">
        <h2>Devices and recovery</h2>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="devices" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Devices</span>
            <span class="lk-card-row-sub">See, add and remove the devices on this account.</span>
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
            <span class="lk-card-row-title">Recovery</span>
            <span class="lk-card-row-sub">
              An authenticator app, an email address or a phone number that gets you back in.
            </span>
          </div>
          <A href={paths.settingsSection("recovery")} class="lk-card-row-action">
            Set up
          </A>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="shield" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Safety numbers</span>
            <span class="lk-card-row-sub">
              Each contact has a safety number. Compare it in person or on a call; matching
              numbers mean nobody is in between. Find it in the contact's details.
            </span>
          </div>
        </div>
      </section>
    </>
  );
}

function RecoverySection(): JSX.Element {
  return (
    <>
      <A href={paths.settingsSection("security")} class="ghost small lk-wide-only" style={{ "align-self": "flex-start" }}>
        <Icon name="back" size={16} />
        Security
      </A>
      <div class="lk-page-head">
        <h1>Recovery</h1>
        <p>
          Your recovery phrase restores everything. These get you back into the account when
          you have lost your password; your messages then come from the phrase or from another
          of your devices.
        </p>
      </div>
      <section class="lk-card">
        <h2>Authenticator app</h2>
        <TotpEnrollView />
      </section>
      <section class="lk-card">
        <h2>Email or phone</h2>
        <ContactEnrollView />
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
        <p>The theme on this device.</p>
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
    </>
  );
}

function AboutSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>About</h1>
        <p>Lakko on the web.</p>
      </div>
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="info" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Version</span>
            <span class="lk-card-row-sub">{appVersion()}</span>
          </div>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="db" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">Licence</span>
            <span class="lk-card-row-sub">
              {appLicense}. The source code is free to read, use and improve.
            </span>
          </div>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="lock" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">How Lakko protects you</span>
            <span class="lk-card-row-sub">
              Messages, calls and contact requests are end-to-end encrypted: only the people in a
              conversation can read them, never Lakko's servers. Your password never leaves your
              device, and each contact has a safety number you can compare in person.
            </span>
          </div>
        </div>
      </section>
    </>
  );
}

function MediaSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>Media</h1>
        <p>Playback on this device.</p>
      </div>
      <section class="lk-card">
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
