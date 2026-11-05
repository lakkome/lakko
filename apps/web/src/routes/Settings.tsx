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
import { TEXT_SIZE_LABEL_KEY, TEXT_SIZES, setTextSize, useTextSize } from "../ui/text-size";
import { chooseQuickReaction, quickReactionOf } from "../chat/quick-reaction-register";
import { EmojiPicker } from "../media/EmojiPicker";
import { createSignal, For, onMount, Show, type JSX } from "solid-js";
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
import { WhoCanFindMeContactsList, WhoCanFindMeList, WhoCanFindMeSection } from "../settings/WhoCanFindMe";
import { setShowCityToOwnDevices, showCityToOwnDevices } from "../devices/device-card-runtime";
import { RecoveryPage } from "../settings/RecoveryPage";
import { ProtectionCard } from "../auth/protect/ProtectionCard";
import { refreshProtection } from "../auth/protect/protection-state";
import Profile from "./Profile";
import { OWN_AVATAR_KEY } from "../profile/avatar-runtime";
import { ChatFoldersSection } from "../chat/folders/ChatFoldersSection";
import { TranslationSettingsSection } from "../translation/TranslationSettings";
import { paths } from "./paths";
import { isDesktop } from "../platform";
import {
  AntiCensorshipCard,
  BrowserNotificationsSection,
  DesktopNotificationsSection,
  KeyboardShortcutsCard,
} from "../settings/DesktopSettings";
import { sectionsForBuild, showsShortcutList } from "../settings/desktop-only";
import { addedNotice, badgeVisible } from "../devices/added-notice";
import { DevicesAttention, devicesNeedAttention } from "../devices/DeviceAddedNotice";

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
type Page = Section | "recovery" | "privacy-groups" | "privacy-contacts" | "translation" | "text-size";

// Language sits after Appearance (`docs/ux/localization.md` section 1).
const ALL_SECTIONS: ReadonlyArray<{ id: Section; labelKey: string; icon: IconName }> = [
  { id: "profile", labelKey: "settings.section.profile", icon: "user" },
  { id: "privacy", labelKey: "settings.section.privacy", icon: "eye" },
  { id: "security", labelKey: "settings.section.security", icon: "shield" },
  // docs/ux/desktop.md section 3.1; the browser build's has the reactions
  // switch alone (docs/ux/reactions-and-emoji.md section 12).
  { id: "notifications", labelKey: "settings.section.notifications", icon: "bell" },
  // docs/ux/chat-folders.md section 4 - between Notifications and Appearance.
  { id: "folders", labelKey: "settings.section.folders", icon: "folder" },
  { id: "appearance", labelKey: "settings.section.appearance", icon: "palette" },
  { id: "language", labelKey: "settings.language.title", icon: "globe" },
  { id: "media", labelKey: "settings.section.media", icon: "video" },
  { id: "about", labelKey: "settings.section.about", icon: "info" },
  { id: "account", labelKey: "settings.section.account", icon: "logout" },
];

/** The sections this build has. */
const SECTIONS = sectionsForBuild(ALL_SECTIONS, isDesktop());

/** The interface language in its own name: the Language row's trailing text. */
function currentLanguageOwnName(): string {
  return supportedLanguages().find((l) => l.code === currentLanguage())?.ownName ?? "";
}

function pageOf(s: string | undefined): Page | null {
  if (s === "recovery") return "recovery";
  if (s === "privacy-groups" || s === "privacy-contacts") return s;
  // ADR-0082 / UX section 6 - a sub-screen of Language.
  if (s === "translation") return "translation";
  // docs/ux/text-size.md - a sub-screen of Appearance.
  if (s === "text-size") return "text-size";
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
    if (p === "translation") return "language";
    if (p === "text-size") return "appearance";
    return p === "privacy-groups" || p === "privacy-contacts" ? "privacy" : p;
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
            personKey={OWN_AVATAR_KEY}
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
            // The Devices row lives in Security: on a wide screen Settings
            // opens on Profile, so the section entry carries the badge too.
            aria-label={
              s.id === "security" && devicesNeedAttention()
                ? t("settings.security.attention_a11y")
                : undefined
            }
          >
            <Icon name={s.icon} size={20} />
            <span>{t(s.labelKey)}</span>
            <Show when={s.id === "security"}>
              <DevicesAttention />
            </Show>
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
            <ShowCitySection />
          </Show>
          <Show when={shown() === "privacy-groups"}>
            <WhoCanFindMeList />
          </Show>
          <Show when={shown() === "privacy-contacts"}>
            <WhoCanFindMeContactsList />
          </Show>
          <Show when={shown() === "security"}>
            <SecuritySection />
          </Show>
          <Show when={shown() === "recovery"}>
            <RecoveryPage />
          </Show>
          <Show when={shown() === "notifications"}>
            <Show when={isDesktop()} fallback={<BrowserNotificationsSection />}>
              <DesktopNotificationsSection />
            </Show>
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
          <Show when={shown() === "translation"}>
            <TranslationSettingsSection />
          </Show>
          <Show when={shown() === "text-size"}>
            <TextSizeSection />
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
      <Show when={isDesktop()}>
        <AntiCensorshipCard />
      </Show>
      <section class="lk-card">
        <h2>{t("settings.security.devices_and_recovery")}</h2>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="devices" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">
              {t("settings.devices.title")}
              <DevicesAttention />
            </span>
            <span class="lk-card-row-sub">{t("settings.security.devices.sub")}</span>
          </div>
          <A
            href={paths.devices}
            class="lk-card-row-action"
            aria-label={badgeVisible(addedNotice()) ? t("settings.devices.attention_a11y") : undefined}
          >
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
      {/* docs/ux/text-size.md - below Theme, the current step trailing. */}
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="lines" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.text_size.title")}</span>
          </div>
          <A href={paths.settingsSection("text-size")} class="lk-card-row-action">
            {t(TEXT_SIZE_LABEL_KEY[useTextSize()])}
          </A>
        </div>
      </section>
      <QuickReactionCard />
    </>
  );
}

/** `docs/ux/text-size.md` - the three steps as one choice, a preview above
 *  them drawn at the chosen step. A pick applies at once (no Save). */
function TextSizeSection(): JSX.Element {
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.text_size.title")}</h1>
      </div>
      <section class="lk-card lk-text-size-preview" aria-hidden="true">
        <ul class="lk-text-size-preview-list">
          <li class="lk-bubble-row incoming run-start run-end">
            <div class="lk-bubble incoming run-start run-end">
              <div class="lk-bubble-text" dir="auto">{t("settings.text_size.preview_incoming")}</div>
              <div class="lk-reactions">
                <span class="lk-reaction">
                  <span class="lk-reaction-emoji">{"\u{1F44D}"}</span>
                  <span class="lk-reaction-count">2</span>
                </span>
              </div>
            </div>
          </li>
          <li class="lk-bubble-row outgoing run-start run-end">
            <div class="lk-bubble outgoing run-start run-end">
              <div class="lk-bubble-text" dir="auto">{t("settings.text_size.preview_outgoing")}</div>
            </div>
          </li>
        </ul>
      </section>
      <section class="lk-card">
        <div class="lk-lang-list" role="radiogroup" aria-label={t("settings.text_size.title")}>
          <For each={TEXT_SIZES}>
            {(size) => (
              <button
                type="button"
                role="radio"
                class="lk-lang-row"
                aria-checked={useTextSize() === size}
                onClick={() => setTextSize(size)}
              >
                <span class="lk-lang-text">
                  <span class="lk-lang-title">{t(TEXT_SIZE_LABEL_KEY[size])}</span>
                </span>
                <span class="lk-lang-mark" aria-hidden="true">
                  <Show when={useTextSize() === size}>
                    <Icon name="check" size={20} />
                  </Show>
                </span>
              </button>
            )}
          </For>
        </div>
      </section>
    </>
  );
}

/** `docs/ux/reactions-and-emoji.md` section 6 (D4) — Settings > Appearance >
 *  Quick reaction: the current emoji; a tap opens the picker in reaction
 *  mode, a pick saves (to the account, D3) and closes. */
function QuickReactionCard(): JSX.Element {
  const auth = useAuth();
  const [open, setOpen] = createSignal(false);
  const account = (): string | null => {
    const st = auth.state();
    return st.kind === "session" ? st.account.accountId : null;
  };
  return (
    <section class="lk-card" style={{ position: "relative" }}>
      <div class="lk-card-row">
        <span class="lk-card-row-icon" aria-hidden="true">
          {quickReactionOf(account())}
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.appearance.quick_reaction")}</span>
          <span class="lk-card-row-sub">{t("settings.appearance.quick_reaction_sub")}</span>
        </div>
        <button
          type="button"
          class="lk-card-row-action"
          data-picker-toggle
          aria-label={t("settings.appearance.quick_reaction_change")}
          disabled={account() === null}
          onClick={() => setOpen(!open())}
        >
          {quickReactionOf(account())}
        </button>
      </div>
      <Show when={open()}>
        <EmojiPicker
          mode="reaction"
          onEmoji={(emoji) => {
            const a = account();
            if (a !== null) chooseQuickReaction(a, emoji);
          }}
          onClose={() => setOpen(false)}
          sessionToken={() => null}
          align="right"
        />
      </Show>
    </section>
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
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="globe" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.translation.title")}</span>
          </div>
          <A href={paths.settingsSection("translation")} class="lk-card-row-action">
            {t("common.manage")}
          </A>
        </div>
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
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="globe" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-sub">{t("settings.about.city_data")}</span>
          </div>
        </div>
      </section>
      <Show when={showsShortcutList(isDesktop())}>
        <KeyboardShortcutsCard />
      </Show>
    </>
  );
}

/** Settings > Privacy: "Show my city to my other devices" (ADR-0080; per
 *  device, on by default). */
function ShowCitySection(): JSX.Element {
  return (
    <section class="lk-card">
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="devices" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.privacy.show_city")}</span>
          <span class="lk-card-row-sub">{t("settings.privacy.show_city.sub")}</span>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={showCityToOwnDevices()}
          class="lk-card-row-action"
          classList={{ primary: showCityToOwnDevices() }}
          onClick={() => setShowCityToOwnDevices(!showCityToOwnDevices())}
        >
          {showCityToOwnDevices() ? t("common.on") : t("common.off")}
        </button>
      </div>
    </section>
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
