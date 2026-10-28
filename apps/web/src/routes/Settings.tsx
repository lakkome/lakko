// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/settings/:section?` — Profile · Privacy · Security · Notifications (desktop
 * only, docs/ux/desktop.md 3.1) · Chat folders · Appearance ·
 * Language · Media · About · Account (docs/ux/client-redesign.md section 5.5; Privacy is
 * docs/ux/who-can-find-me.md). A section list on the
 * left, the section on the right; a narrow screen shows one of the two.
 * `/settings` lands on Profile. `recovery` (the recovery phrase, passkeys,
 * devices, three words, the authenticator app: `settings/RecoveryPage`) is
 * a page under Security rather than a section of its own, and
 * `privacy-groups` (the chosen groups and channels) a page under Privacy. Account is last:
 * it holds the export and the danger zone.
 */
import { createSignal, onMount, Show, type JSX } from "solid-js";
import { A, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import PasskeysSection from "../auth/PasskeysSection";
import { AccountCard, ExportDataCard } from "../auth/AccountSections";
import { NewPasswordForm } from "../auth/NewPasswordForm";
import { PasswordChangeGate } from "../auth/PasswordChangeGate";
import { ChannelsIShareCard } from "../discover/ChannelsIShare";
import { autoplayVideoNotes, setAutoplayVideoNotes } from "../media/media-prefs";
import { useProfiles } from "../profile/ProfileContext";
import { appLicense, appVersion } from "../ui/app-version";
import { Avatar } from "../ui/Avatar";
import { Icon, type IconName } from "../ui/icons";
import { setTheme, useTheme, type Theme } from "../ui/theme";
import { currentLanguage, supportedLanguages, t } from "../l10n";
import { LanguagePicker } from "../l10n/LanguagePicker";
import { WhoCanFindMeList, WhoCanFindMeSection } from "../settings/WhoCanFindMe";
import { RecoveryPage } from "../settings/RecoveryPage";
import { ProtectionCard } from "../auth/protect/ProtectionCard";
import { refreshProtection } from "../auth/protect/protection-state";
import Profile from "./Profile";
import { ChatFoldersSection } from "../chat/folders/ChatFoldersSection";
import { paths } from "./paths";
import { isDesktop } from "../platform";
import { DesktopNotificationsSection, KeyboardShortcutsCard } from "../settings/DesktopSettings";
import { sectionsForBuild, showsShortcutList } from "../settings/desktop-only";

type Section =
  | "profile"
  | "privacy"
  | "security"
  | "notifications"
  | "folders"
  | "appearance"
  | "language"
  | "media"
  | "about"
  | "account";
type Page = Section | "recovery" | "privacy-groups";

// Language sits after Appearance (`docs/ux/localization.md` section 1).
const ALL_SECTIONS: ReadonlyArray<{ id: Section; labelKey: string; icon: IconName }> = [
  { id: "profile", labelKey: "settings.section.profile", icon: "user" },
  { id: "privacy", labelKey: "settings.section.privacy", icon: "eye" },
  { id: "security", labelKey: "settings.section.security", icon: "shield" },
  // docs/ux/desktop.md section 3.1 - desktop only; the web has no
  // Notifications section.
  { id: "notifications", labelKey: "settings.section.notifications", icon: "bell" },
  // docs/ux/chat-folders.md section 4 - between Notifications and Appearance.
  { id: "folders", labelKey: "settings.section.folders", icon: "folder" },
  { id: "appearance", labelKey: "settings.section.appearance", icon: "palette" },
  { id: "language", labelKey: "settings.language.title", icon: "globe" },
  { id: "media", labelKey: "settings.section.media", icon: "video" },
  { id: "about", labelKey: "settings.section.about", icon: "info" },
  { id: "account", labelKey: "settings.section.account", icon: "logout" },
];

/** The sections this build has: Notifications only in the desktop app. */
const SECTIONS = sectionsForBuild(ALL_SECTIONS, isDesktop());

/** The interface language in its own name: the Language row's trailing text. */
function currentLanguageOwnName(): string {
  return supportedLanguages().find((l) => l.code === currentLanguage())?.ownName ?? "";
}

function pageOf(s: string | undefined): Page | null {
  if (s === "recovery") return "recovery";
  if (s === "privacy-groups") return "privacy-groups";
  // The former Data section: its export card now sits under Account.
  if (s === "data") return "account";
  return SECTIONS.some((x) => x.id === s) ? (s as Section) : null;
}

export default function Settings(): JSX.Element {
  const params = useParams<{ section?: string }>();
  const auth = useAuth();
  const profiles = useProfiles();
  // ADR-0075 (alignment M): whenever Settings opens, how the account is
  // protected is read again, which keeps this device's "phrase-protected"
  // flag in step with a phrase saved on another device.
  onMount(() => {
    const s = auth.state();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (s.kind === "session" && token !== null) void refreshProtection(s.account.accountId, token);
  });

  // `/settings` (no section) shows the list alone on a narrow screen and
  // Profile beside it on a wide one.
  const page = (): Page | null => pageOf(params.section);
  const shown = (): Page => page() ?? "profile";
  // The list highlights the section a page belongs to.
  const highlighted = (): Section => {
    const p = shown();
    if (p === "recovery") return "security";
    return p === "privacy-groups" ? "privacy" : p;
  };

  const username = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };
  const displayName = (): string => profiles.ownProfile().displayName ?? "";

  return (
    <div class="lk-settings" data-open={page() === null ? "false" : "true"}>
      <nav class="lk-settings-nav" aria-label={t("nav.settings")}>
        <h1>{t("nav.settings")}</h1>
        <div class="lk-settings-me">
          <Avatar
            label={displayName().length > 0 ? displayName() : username()}
            id={username()}
            size="md"
          />
          <div>
            <div class="lk-settings-me-name">
              {displayName().length > 0 ? displayName() : username().length > 0 ? `@${username()}` : t("settings.me.guest")}
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
            <span>{t(s.labelKey)}</span>
            <Show when={s.id === "language"}>
              <span class="lk-settings-nav-trail">
                <bdi lang={currentLanguage()}>{currentLanguageOwnName()}</bdi>
              </span>
            </Show>
          </A>
        ))}
      </nav>
      <div class="lk-settings-body">
        <div class="lk-page lk-stack">
          <A href={paths.settings} class="ghost small lk-narrow-only" style={{ "align-self": "flex-start" }}>
            <Icon name="back" size={16} />
            {t("nav.settings")}
          </A>
          <Show when={shown() === "profile"}>
            <Profile />
          </Show>
          <Show when={shown() === "privacy"}>
            <WhoCanFindMeSection />
          </Show>
          <Show when={shown() === "privacy-groups"}>
            <WhoCanFindMeList />
          </Show>
          <Show when={shown() === "security"}>
            <SecuritySection />
          </Show>
          <Show when={shown() === "recovery"}>
            <RecoveryPage />
          </Show>
          <Show when={shown() === "notifications"}>
            <DesktopNotificationsSection />
          </Show>
          <Show when={shown() === "folders"}>
            <ChatFoldersSection />
          </Show>
          <Show when={shown() === "appearance"}>
            <AppearanceSection />
          </Show>
          <Show when={shown() === "language"}>
            <LanguageSection />
          </Show>
          <Show when={shown() === "media"}>
            <MediaSection />
          </Show>
          <Show when={shown() === "about"}>
            <AboutSection />
          </Show>
          <Show when={shown() === "account"}>
            <div class="lk-page-head">
              <h1>{t("settings.section.account")}</h1>
              <p>{t("settings.account.lede")}</p>
            </div>
            <ExportDataCard />
            <ProtectionCard />
            <AccountCard />
          </Show>
        </div>
      </div>
    </div>
  );
}

/**
 * Settings > Security > Password (docs/ux/identity-recovery.md 7.1): the
 * same form the step after a forgot-password route shows. The device proves
 * it holds the account's keys, so only the new password is typed.
 */
function PasswordCard(): JSX.Element {
  const auth = useAuth();
  const [open, setOpen] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  return (
    <Show when={auth.canReplacePassword()}>
      <section class="lk-card">
        <h2>{t("settings.password.title")}</h2>
        <p class="lk-card-desc">{t("settings.password.lede")}</p>
        <Show
          when={open()}
          fallback={
            <>
              <Show when={saved()}>
                <p class="form-info" role="status">
                  {t("recovery.new_password.saved")}
                </p>
              </Show>
              <button
                type="button"
                onClick={() => {
                  setSaved(false);
                  setOpen(true);
                }}
              >
                {t("settings.password.change")}
              </button>
            </>
          }
        >
          {/* UX 17.8: the form opens only after the current password, a
              passkey, or three words of the recovery phrase. */}
          <PasswordChangeGate onSkip={() => setOpen(false)} skipLabel={t("common.cancel")}>
            <NewPasswordForm
              onSaved={() => {
                setSaved(true);
                setOpen(false);
              }}
              onSkip={() => setOpen(false)}
              skipLabel={t("common.cancel")}
            />
          </PasswordChangeGate>
        </Show>
      </section>
    </Show>
  );
}

function SecuritySection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.security")}</h1>
        <p>{t("settings.security.lede")}</p>
      </div>
      <PasswordCard />
      <PasskeysSection />
      <ChannelsIShareCard />
      <section class="lk-card">
        <h2>{t("settings.security.devices_and_recovery")}</h2>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="devices" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.devices.title")}</span>
            <span class="lk-card-row-sub">{t("settings.security.devices.sub")}</span>
          </div>
          <A href={paths.devices} class="lk-card-row-action">
            {t("common.manage")}
          </A>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="lock" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.recovery.title")}</span>
            <span class="lk-card-row-sub">{t("settings.security.recovery.sub")}</span>
          </div>
          <A href={paths.settingsSection("recovery")} class="lk-card-row-action">
            {t("common.manage")}
          </A>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="shield" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.security.safety_numbers.title")}</span>
            <span class="lk-card-row-sub">{t("settings.security.safety_numbers.sub")}</span>
          </div>
        </div>
      </section>
    </>
  );
}

function AppearanceSection(): JSX.Element {
  const options: ReadonlyArray<{ value: Theme; labelKey: string; icon: IconName }> = [
    { value: "system", labelKey: "settings.appearance.theme.system", icon: "monitor" },
    { value: "light", labelKey: "settings.appearance.theme.light", icon: "sun" },
    { value: "dark", labelKey: "settings.appearance.theme.dark", icon: "moon" },
  ];
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.appearance")}</h1>
        <p>{t("settings.appearance.lede")}</p>
      </div>
      <section class="lk-card">
        <h2>{t("settings.appearance.theme")}</h2>
        <div class="lk-segmented" role="radiogroup" aria-label={t("settings.appearance.theme")}>
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
              {t(o.labelKey)}
            </label>
          ))}
        </div>
      </section>
    </>
  );
}

function LanguageSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.language.title")}</h1>
      </div>
      <section class="lk-card">
        <LanguagePicker />
      </section>
    </>
  );
}

function AboutSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.about")}</h1>
        <p>{t("settings.about.lede_web")}</p>
      </div>
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="info" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.about.version")}</span>
            <span class="lk-card-row-sub">{appVersion()}</span>
          </div>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="db" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.about.licence")}</span>
            <span class="lk-card-row-sub">{t("settings.about.licence_body", { licence: appLicense })}</span>
          </div>
        </div>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="lock" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.about.protection.title")}</span>
            <span class="lk-card-row-sub">{t("settings.about.protection.body")}</span>
          </div>
        </div>
      </section>
      <Show when={showsShortcutList(isDesktop())}>
        <KeyboardShortcutsCard />
      </Show>
    </>
  );
}

function MediaSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.media")}</h1>
        <p>{t("settings.media.lede")}</p>
      </div>
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="video" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.media.autoplay_video_notes.title")}</span>
            <span class="lk-card-row-sub">{t("settings.media.autoplay_video_notes.sub")}</span>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={autoplayVideoNotes()}
            class="lk-card-row-action"
            classList={{ primary: autoplayVideoNotes() }}
            onClick={() => setAutoplayVideoNotes(!autoplayVideoNotes())}
          >
            {autoplayVideoNotes() ? t("common.on") : t("common.off")}
          </button>
        </div>
      </section>
    </>
  );
}
