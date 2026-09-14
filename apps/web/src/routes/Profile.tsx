// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/profile` — the viewer's own profile (ADR-0048, UX spec §4).
 *
 * A name field (one line, at most 64 characters, validated by the core as the
 * user types), a live preview of how others see you (`Alice Petrova ·
 * @alice`), Save, and the username shown read-only. Saving publishes the new
 * name into every group this device is in; contacts see it on their next
 * receipt. There is no per-contact name.
 */
import { createEffect, createSignal, on, Show, type JSX } from "solid-js";

import { useProfiles } from "../profile/ProfileContext";
import { createDisplayNameCheck } from "../profile/username-field";
import { maySaveDisplayName, savedDisplayName } from "../profile/rules";
import { ownPreviewLabel } from "../profile/resolver";

/** UX spec §4 — the confirmation after a save. */
export const PROFILE_SAVED_NOTICE = "Your name will update for your contacts.";

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
      "Others see your key fingerprint until you set a name."
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
      setNotice(PROFILE_SAVED_NOTICE);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <article>
      <h1>Profile</h1>
      <section
        style={{
          background: "var(--surface-elevated)",
          border: "1px solid var(--surface-border)",
          "border-radius": "var(--radius-md)",
          padding: "1rem",
          margin: "1rem 0",
          "max-width": "32rem",
        }}
      >
        <form onSubmit={(e) => void onSave(e)}>
          <label>
            Your name
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
              How others see you
            </div>
            <div style={{ "font-weight": 600 }}>{preview()}</div>
          </div>
          <div style={{ "margin-top": "0.75rem" }}>
            <div style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>Username</div>
            <div>
              <Show when={profiles.ownUsername()} fallback={<span>None</span>}>
                {(u) => <code>@{u()}</code>}
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
            {busy() ? "Saving…" : "Save"}
          </button>
        </form>
      </section>
    </article>
  );
}
