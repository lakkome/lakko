// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Profile — the viewer's own profile (ADR-0048, UX spec §4).
 *
 * A name field (one line, at most 64 characters, validated by the core as the
 * user types), a live preview of how others see you (`Alice Petrova ·
 * @alice`), Save, and the username shown read-only. Saving publishes the new
 * name into every group this device is in; contacts see it on their next
 * receipt. There is no per-contact name.
 */
import { createEffect, createSignal, on, Show, type JSX } from "solid-js";

import { openUsernameChange } from "../auth/username-claim-host";
import { useProfiles } from "../profile/ProfileContext";
import { createDisplayNameCheck } from "../profile/username-field";
import { maySaveDisplayName, savedDisplayName } from "../profile/rules";
import { ownPreviewLabel } from "../profile/resolver";
import { t } from "../l10n";


export default function Profile(): JSX.Element {
  const profiles = useProfiles();
  const [name, setName] = createSignal(profiles.ownProfile().displayName ?? "");
  const [busy, setBusy] = createSignal(false);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const check = createDisplayNameCheck(name);
  // Follow the saved name until the user starts editing (a cold load can
  // render before the session is known).
  let edited = false;
  createEffect(
    on(
      () => profiles.ownProfile().displayName,
      (saved) => {
        if (!edited) setName(saved ?? "");
      },
    ),
  );

  const preview = (): string => {
    const c = check();
    const shownName = c.kind === "valid" ? c.name : c.kind === "none" ? null : name().trim();
    return (
      ownPreviewLabel(shownName, profiles.ownUsername()) ??
      t("settings.profile.preview.no_name")
    );
  };

  const unchanged = (): boolean => {
    const c = check();
    return maySaveDisplayName(c) && savedDisplayName(c) === profiles.ownProfile().displayName;
  };

  async function onSave(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setNotice(null);
    setError(null);
    const c = check();
    if (!maySaveDisplayName(c)) return;
    setBusy(true);
    try {
      await profiles.saveOwnDisplayName(savedDisplayName(c));
      setName(profiles.ownProfile().displayName ?? "");
      // The field holds the saved name again: follow it once more, so a name
      // saved on another device of the account replaces it here.
      edited = false;
      // UX spec §4 — the confirmation after a save.
      setNotice(t("settings.profile.saved"));
    } catch (err) {
      console.warn("[profile] save failed", err);
      setError(t("settings.profile.error.generic"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.profile")}</h1>
        <p>{t("settings.profile.lede")}</p>
      </div>
      <section class="lk-card">
        <form onSubmit={(e) => void onSave(e)}>
          <label>
            {t("settings.profile.name.label")}
            <input
              type="text"
              autocomplete="name"
              value={name()}
              placeholder={profiles.ownUsername() ?? ""}
              onInput={(e) => {
                edited = true;
                setName(e.currentTarget.value);
                setNotice(null);
              }}
              disabled={busy()}
              aria-invalid={check().kind === "invalid"}
              aria-describedby="profile-name-reason"
            />
          </label>
          <div id="profile-name-reason" role="status">
            <Show
              when={(() => {
                const c = check();
                return c.kind === "invalid" ? c.reason : null;
              })()}
            >
              {(reason) => (
                <div class="form-error" style={{ "font-size": "0.85rem" }}>
                  {reason()}
                </div>
              )}
            </Show>
          </div>
          <div style={{ "margin-top": "0.75rem" }}>
            <div style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
              {t("settings.profile.preview.label")}
            </div>
            <div style={{ "font-weight": 600 }}>{preview()}</div>
          </div>
          <div style={{ "margin-top": "0.75rem" }}>
            <div style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>{t("auth.username.label")}</div>
            <div>
              <Show
                when={profiles.ownUsername()}
                fallback={
                  // This browser does not know the account's username (a
                  // passkey sign-in with no name typed).
                  <span>{t("settings.profile.username.none")}</span>
                }
              >
                {(u) => (
                  <>
                    <code>@{u()}</code>
                    {/* ADR-0076: the name can be changed from where it is shown. */}{" "}
                    <button type="button" onClick={openUsernameChange}>
                      {t("username.change.title")}
                    </button>
                  </>
                )}
              </Show>
            </div>
          </div>
          <Show when={error()}>
            {(msg) => <div class="form-error">{msg()}</div>}
          </Show>
          <Show when={notice()}>
            {(msg) => (
              <div role="status" style={{ color: "var(--accent)", "font-size": "0.9rem" }}>
                {msg()}
              </div>
            )}
          </Show>
          <button
            class="primary"
            type="submit"
            style={{ "margin-top": "0.75rem" }}
            disabled={busy() || !maySaveDisplayName(check()) || unchanged()}
          >
            {busy() ? t("common.saving") : t("common.save")}
          </button>
        </form>
      </section>
    </>
  );
}
