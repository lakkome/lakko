// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The person screen (docs/ux/client-redesign.md section 5.3): avatar, name,
 * @username; Message, Call and Video; the safety number as the one
 * verification surface; Rename; Remove behind the in-page confirmation. No
 * key fingerprint anywhere on it.
 *
 * Call and Video are the chat's own entry points, reached through the
 * 1x1 chat; without a chat on this device they are disabled rather than
 * shown as something they are not.
 *
 * There is ONE person screen and this is it. It is the main pane on
 * Contacts, and a one-to-one conversation's header opens that same page
 * (`docs/ux/conversation-info.md` sections 2.1 and 3, "Except a person") -
 * it navigates there rather than rendering a second copy over the chat.
 * Until 2026-09-26 it did render one, with Message withheld because the
 * chat was "already open behind it", and a person looking at both saw two
 * cards for one contact.
 *
 * Message, Call and Video are offered wherever the card was opened from;
 * from the conversation, Message simply returns to it. The chat used to keep
 * a second caller for a one-to-one whose contact record could not be found,
 * rendering this view with Rename and Remove withheld; that "not a contact"
 * state is gone, because the other person in a one-to-one IS a contact by
 * construction (`contacts/adopt-one-to-one.ts`), and the Contacts route is
 * now the one caller.
 *
 * Which blocks appear is NOT decided here and no longer decided on this
 * client at all: it is `lakko_ui_rules::info`, reached through
 * `chat/conversation-info.ts` and shared with iOS and Android. This file
 * renders what it is handed.
 *
 * The one state worth naming, because it is what that move fixed: once this
 * device's membership in the conversation has ended (ADR-0045 UX 6.2), Call
 * and Video go and `Delete this chat` takes their place. It used to live in
 * the GROUP panel only, which a one-to-one had just stopped opening - so an
 * ended one-to-one had no way to be closed at all.
 */
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { DELETE_CHAT_CONTROL } from "../chat/membership-ended";
import { personInfoSections } from "../chat/conversation-info";
import { uiRules } from "../chat/ui-rules";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon, type IconName } from "../ui/icons";
import { t } from "../l10n";
import { namedActionKey } from "../ui/confirm-dialog";
import { createLiveText } from "../l10n/live-text";
import { computeSafetyNumber } from "./safety";
import { safetyNumberRows } from "./safety-number-format";

export interface ContactDetailViewProps {
  /** The person's Ed25519 identity key, base64 - who this screen is about. */
  readonly personKeyB64: string;
  /** The viewer's own name for them; empty when they have not renamed them. */
  readonly localName: string;
  /** The route of the 1x1 chat with this person on this device, or `null`. */
  readonly chatHref: string | null;
  /** Whether Message can create the chat when there is none yet. */
  readonly canStartChat: boolean;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly myPubkey: () => Uint8Array | null;
  /** The narrow-screen way back to the list. */
  readonly onBack: () => void;
  /** Message: open the conversation with this person. Always offered; a
   *  caller that IS the conversation passes the way back to it. */
  readonly onMessage: () => void;
  readonly onCall: (withVideo: boolean) => void;
  /** Set the viewer's own name for this person; blank clears it. */
  readonly onRename: (newName: string) => void;
  /** Called after the user confirmed the removal. */
  readonly onRemove: () => void;
  /**
   * Whether this device's membership in the conversation behind this screen
   * has ended (ADR-0045 UX 6.2).
   *
   * Required rather than defaulted, so both callers have to answer it. When
   * it is true, `Call` and `Video` go - the per-call key comes from a group
   * this device is no longer in, so the tiles would assert a live E2EE
   * session there is not - and `Delete this chat` arrives in their place.
   */
  readonly membershipEnded: boolean;
  /**
   * Forget this conversation and its transcript (ADR-0045 UX 6.2.4). `null`
   * where this screen is not about a conversation at all, in which case there
   * is nothing to delete; the block is then absent whatever the rule says.
   */
  readonly onDeleteChat: (() => void) | null;
  /**
   * ADR-0065 phase 2 (UX 16.1 / 16.2) — the contact's account keys were reset
   * and this device has not verified the new safety number. `null` for a
   * `normal` contact. The safety-number section becomes the verification of
   * 16.2 over `pendingKeyB64` (never the old key), the banner sits under the
   * header, and Message / Call / Video are held back until `onConfirm`.
   */
  readonly keyReset: {
    readonly pendingKeyB64: string;
    /** Re-pin the new key: resolves `confirmed`, or `failed` (store write). */
    readonly onConfirm: () => Promise<"confirmed" | "failed">;
  } | null;
}

export function ContactDetailView(props: ContactDetailViewProps): JSX.Element {
  const profiles = useProfiles();
  // Which blocks this screen holds - the shared rule
  // (`chat/conversation-info.ts` -> `crates/lakko-ui-rules`), not this file.
  const sections = createMemo(() =>
    personInfoSections(uiRules(), { membershipEnded: props.membershipEnded }),
  );
  const resolved = (): ReturnType<typeof profiles.resolve> =>
    profiles.resolve(props.personKeyB64);
  const name = (): string => memberNameOf(resolved());
  /** Whether the person has a name to put in a sentence (UX 16.7's
   *  `_unnamed` variants otherwise): a key fingerprint is not one. */
  const named = (): boolean => resolved().source !== "fingerprint";

  const [numberOpen, setNumberOpen] = createSignal(false);
  const [number, setNumber] = createSignal<string | null>(null);
  const [numberBusy, setNumberBusy] = createSignal(false);
  const [numberError, setNumberError] = createLiveText();
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  // UX 16.2 — the reset verification: the checkbox, the confirm in flight,
  // the inline confirmed line (web has no toast), the save failure.
  const [compared, setCompared] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [confirmedLine, setConfirmedLine] = createSignal<string | null>(null);
  const [confirmError, setConfirmError] = createLiveText();
  /** The key the safety number is computed over: the NEW key while a reset
   *  is unverified (UX 16.2), else the contact's key. */
  const safetyKeyB64 = (): string => props.keyReset?.pendingKeyB64 ?? props.personKeyB64;
  let safetyKeyShown: string | null = null;

  // Another person selected: their number is a different one.
  createEffect(
    on(
      () => props.personKeyB64,
      () => {
        setNumber(null);
        setNumberOpen(false);
        setNumberError(null);
        setEditing(false);
        setCompared(false);
        setConfirmedLine(null);
        setConfirmError(null);
      },
      { defer: true },
    ),
  );

  // The key the number is over changed under an open section (a reset
  // noticed while this screen is up): recompute rather than show a number
  // over a key that is no longer the one the section is about.
  createEffect(
    on(
      safetyKeyB64,
      (key) => {
        if (safetyKeyShown !== null && safetyKeyShown !== key) {
          setNumber(null);
          if (numberOpen()) void ensureNumber();
        }
      },
      { defer: true },
    ),
  );

  async function ensureNumber(): Promise<void> {
    if (number() !== null && safetyKeyShown === safetyKeyB64()) return;
    const my = props.myPubkey();
    if (my === null) {
      setNumberError(() => t("contacts.safety.sign_in"));
      return;
    }
    setNumberBusy(true);
    setNumberError(null);
    try {
      const key = safetyKeyB64();
      setNumber(await computeSafetyNumber(my, base64Decode(key)));
      safetyKeyShown = key;
    } catch (e) {
      console.warn("[contacts] safety number failed", e);
      setNumberError(() => t("contacts.safety.error"));
    } finally {
      setNumberBusy(false);
    }
  }

  async function toggleNumber(): Promise<void> {
    const next = !numberOpen();
    setNumberOpen(next);
    if (next) await ensureNumber();
  }

  /** UX 16.2 — the person compared the new number and confirmed it. */
  async function confirmReset(): Promise<void> {
    const reset = props.keyReset;
    if (reset === null || !compared() || confirming()) return;
    setConfirming(true);
    setConfirmError(null);
    const who = name();
    try {
      const result = await reset.onConfirm();
      if (result === "confirmed") {
        // Confirmed: the section returns to its normal shape with the same
        // number (the new key is now the contact's key), and the inline line
        // says so.
        setCompared(false);
        setConfirmedLine(
          named()
            ? t("contacts.safety.reset.done", { name: who })
            : t("contacts.safety.reset.done_unnamed"),
        );
      } else {
        setConfirmError(() => t("contacts.safety.reset.save_failed"));
      }
    } catch {
      setConfirmError(() => t("contacts.safety.reset.save_failed"));
    } finally {
      setConfirming(false);
    }
  }

  /** The `Verify` banner action: scroll to and open the section. */
  let safetySection: HTMLElement | undefined;
  async function openSafetyFromBanner(): Promise<void> {
    safetySection?.scrollIntoView({ block: "start", behavior: "smooth" });
    if (!numberOpen()) await toggleNumber();
  }

  function startEdit(): void {
    setDraft(props.localName);
    setEditing(true);
  }
  function saveEdit(): void {
    props.onRename(draft());
    setEditing(false);
  }

  async function confirmRemove(): Promise<void> {
    const who = name();
    const ok = await requestConfirm({
      title: t("contacts.remove.title"),
      body: t("contacts.remove.body"),
      confirmLabel: namedActionKey("common.remove", "contacts.remove.confirm_named", who),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (ok) props.onRemove();
  }

  const actions = (): ReadonlyArray<{
    label: string;
    icon: IconName;
    disabled: boolean;
    title: string | undefined;
    run: () => void;
  }> => {
    const noChat = props.chatHref === null;
    // ADR-0065 phase 2 (UX 16.1 table): while the contact is
    // `reset-unverified`, Message opens the CLOSED old chat when one exists
    // (and is otherwise held back), and Call / Video are held back — the call
    // keys would come from a chat that is closed. One helper line for all.
    const unverified = props.keyReset !== null;
    const verifyFirst = t("contacts.keys_reset.verify_first_short");
    const callTitle = unverified
      ? verifyFirst
      : noChat
        ? t("contacts.detail.call_needs_chat")
        : undefined;
    // Message is offered wherever the card was opened from (spec section
    // 2.1); whether it appears at all is the rule's call, not this file's.
    const message =
      !sections().includes("message")
        ? []
        : [
            {
              label: t("contacts.detail.message"),
              icon: "chat" as IconName,
              disabled: props.busy() || (noChat && (!props.canStartChat || unverified)),
              title:
                noChat && unverified
                  ? verifyFirst
                  : noChat && !props.canStartChat
                    ? t("contacts.detail.message_unavailable")
                    : undefined,
              run: () => props.onMessage(),
            },
          ];
    // ADR-0045 UX 6.2.8 item 4 - gone once this device's membership has
    // ended, which is what the rule's `call` block records. `exportSecret`
    // is gated on `is_active()` and raises, so the tiles would fail closed
    // anyway; a control that always errors is the shape 6.1.1 rejects, and a
    // call control asserts a live E2EE session there is not.
    const calls = !sections().includes("call")
      ? []
      : [
          {
            label: t("chat.call.voice"),
            icon: "phone" as IconName,
            disabled: props.busy() || noChat || unverified,
            title: callTitle,
            run: () => props.onCall(false),
          },
          {
            label: t("chat.media.video"),
            icon: "video" as IconName,
            disabled: props.busy() || noChat || unverified,
            title: callTitle,
            run: () => props.onCall(true),
          },
        ];
    return [...message, ...calls];
  };

  return (
    <article>
      <div class="lk-page lk-stack lk-contact-detail">
        <button
          type="button"
          class="ghost small lk-narrow-only"
          style={{ "align-self": "flex-start" }}
          onClick={() => props.onBack()}
        >
          <Icon name="back" size={16} />
          {t("nav.contacts")}
        </button>

        <header class="lk-contact-head">
          <Avatar label={name()} id={props.personKeyB64} size="xl" />
          <h1>{name()}</h1>
          <Show when={resolved().username}>
            {(u) => <div class="lk-contact-username">{u()}</div>}
          </Show>
          <Show when={resolved().theirName}>
            {(their) => <div class="lk-meta">{t("contacts.detail.their_name", { name: their() })}</div>}
          </Show>
          <Show when={resolved().usernameNotVerified}>
            <div class="lk-meta">{t("contacts.detail.username_not_verified")}</div>
          </Show>
        </header>

        {/* ADR-0065 phase 2 (UX 16.1) — the banner: the state, with no
            dismiss (it goes when the state does), announced as the first
            element after the header. `Verify` opens the section below. */}
        <Show when={props.keyReset}>
          <section class="lk-card lk-keys-reset-banner" role="status" aria-live="polite">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="shieldAlert" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-sub" style={{ color: "var(--surface-fg)" }}>
                  {named()
                    ? t("contacts.keys_reset.banner", { name: name() })
                    : t("contacts.keys_reset.banner_unnamed")}
                </span>
              </div>
              <button
                type="button"
                class="ghost small lk-card-row-action"
                style={{ color: "var(--accent)" }}
                onClick={() => void openSafetyFromBanner()}
              >
                {t("contacts.keys_reset.banner_action")}
              </button>
            </div>
          </section>
        </Show>
        <Show when={props.keyReset !== null && (actions().some((a) => a.disabled))}>
          <p class="lk-meta" style={{ margin: 0 }}>{t("contacts.keys_reset.verify_first_short")}</p>
        </Show>

        {/* An empty row is not a row: once a membership has ended and this
            screen is the conversation's own, there is nothing left to put in
            it. */}
        <Show when={actions().length > 0}>
          <div class="lk-detail-actions" role="group" aria-label={t("contacts.detail.actions")}>
            <For each={actions()}>
              {(a) => (
                <button
                  type="button"
                  class="lk-action-tile"
                  disabled={a.disabled}
                  title={a.title}
                  onClick={a.run}
                >
                  <Icon name={a.icon} size={22} />
                  <span>{a.label}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show when={props.error()}>{(message) => <div class="form-error">{message()}</div>}</Show>

        <Show when={sections().includes("safety-number")}>
        <section class="lk-card" ref={safetySection}>
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="shield" size={18} />
            </span>
            <div class="lk-card-row-body">
              {/* UX 16.2 — in the `reset-unverified` state the one
                  verification surface reads as the verification of the NEW
                  number; the old number is never shown beside it. */}
              <Show
                when={props.keyReset}
                fallback={
                  <>
                    <span class="lk-card-row-title">{t("contacts.safety.title")}</span>
                    <span class="lk-card-row-sub">{t("contacts.safety.body", { name: name() })}</span>
                  </>
                }
              >
                <span class="lk-card-row-title">{t("contacts.safety.reset.title")}</span>
                <span class="lk-card-row-sub">
                  {named()
                    ? t("contacts.safety.reset.body", { name: name() })
                    : t("contacts.safety.reset.body_unnamed")}
                </span>
              </Show>
            </div>
            <button
              type="button"
              class="ghost small lk-card-row-action"
              style={{ color: "var(--accent)" }}
              aria-expanded={numberOpen()}
              disabled={numberBusy()}
              onClick={() => void toggleNumber()}
            >
              {numberOpen() ? t("chat.attachment.hide") : t("contacts.safety.show")}
            </button>
          </div>
          <Show when={numberOpen()}>
            <Show
              when={number()}
              fallback={
                <p class="lk-meta lk-center" style={{ margin: 0 }}>
                  {numberBusy() ? t("common.loading") : (numberError() ?? "")}
                </p>
              }
            >
              {(n) => (
                <>
                  <div class="lk-safety-number" aria-label={t("contacts.safety.number")}>
                    <For each={safetyNumberRows(n())}>
                      {(row) => (
                        <div class="lk-safety-row">
                          <For each={row}>{(group) => <span>{group}</span>}</For>
                        </div>
                      )}
                    </For>
                  </div>
                  {/* UX 16.2 — the deliberate step: the checkbox and its
                      sentence are one announcement; the warning under it is
                      always visible while the section is open; no second
                      dialog. */}
                  <Show when={props.keyReset}>
                    <div class="lk-stack" style={{ gap: "0.5rem" }}>
                      <label style={{ display: "flex", gap: "0.5rem", "align-items": "flex-start" }}>
                        <input
                          type="checkbox"
                          checked={compared()}
                          disabled={confirming()}
                          onChange={(e) => setCompared(e.currentTarget.checked)}
                        />
                        <span>
                          {named()
                            ? t("contacts.safety.reset.checkbox", { name: name() })
                            : t("contacts.safety.reset.checkbox_unnamed")}
                        </span>
                      </label>
                      <p class="lk-meta" style={{ margin: 0 }}>
                        {named()
                          ? t("contacts.safety.reset.warning", { name: name() })
                          : t("contacts.safety.reset.warning_unnamed")}
                      </p>
                      <button
                        type="button"
                        class="primary"
                        style={{ "align-self": "flex-start" }}
                        disabled={!compared() || confirming()}
                        onClick={() => void confirmReset()}
                      >
                        {confirming()
                          ? t("contacts.safety.reset.confirming")
                          : t("contacts.safety.reset.confirm")}
                      </button>
                      <Show when={confirmError()}>
                        {(m) => <div class="form-error">{m()}</div>}
                      </Show>
                    </div>
                  </Show>
                </>
              )}
            </Show>
          </Show>
          {/* UX 16.2 "Confirmed" — web has no toast: the inline line in the
              section, which returns to its normal shape above it. */}
          <Show when={confirmedLine()}>
            {(line) => (
              <p class="lk-meta" role="status" style={{ margin: 0 }}>
                {line()}
              </p>
            )}
          </Show>
        </section>
        </Show>

        <Show when={sections().includes("rename-contact")}>
          <section class="lk-card">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="pencil" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{t("contacts.rename.title")}</span>
                <span class="lk-card-row-sub">
                  {props.localName.length > 0
                    ? t("contacts.rename.sub_local")
                    : t("contacts.rename.sub_theirs")}
                </span>
              </div>
              <button
                type="button"
                class="lk-card-row-action"
                onClick={() => (editing() ? setEditing(false) : startEdit())}
              >
                {t("contacts.rename.action")}
              </button>
            </div>
            <Show when={editing()}>
              <div class="lk-actions" style={{ "flex-wrap": "nowrap" }}>
                <input
                  type="text"
                  value={draft()}
                  placeholder={t("contacts.rename.placeholder")}
                  aria-label={t("contacts.rename.field")}
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") saveEdit();
                    if (e.key === "Escape") setEditing(false);
                  }}
                  style={{ flex: 1 }}
                />
                <button type="button" class="primary" onClick={saveEdit}>
                  {t("common.save")}
                </button>
                <button type="button" class="ghost" onClick={() => setEditing(false)}>
                  {t("common.cancel")}
                </button>
              </div>
            </Show>
          </section>
        </Show>

        <Show when={sections().includes("remove-contact")}>
          <section class="lk-card danger">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="trash" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{t("contacts.remove.control")}</span>
                <span class="lk-card-row-sub">{t("contacts.remove.sub", { name: name() })}</span>
              </div>
              <button
                type="button"
                class="danger lk-card-row-action"
                disabled={props.busy()}
                onClick={() => void confirmRemove()}
              >
                {t("common.remove")}
              </button>
            </div>
          </section>
        </Show>

        {/* ADR-0045 UX 6.2.4 - once this device's membership has ended, the
            one thing still open is closing the door behind you.

            This block used to exist only in the GROUP panel, and a one-to-one
            stopped opening that panel the moment the header started routing by
            what the conversation IS - so an ended one-to-one had no way to be
            closed at all. The shared rule now answers `delete-chat` for BOTH
            kinds in that state, and this is web's half of it.

            It is the ONE control in this design that destroys user data on
            request: not a claim the UI should stop making, but a transcript
            that exists nowhere else, on one confirmation, with no undo and no
            export. The confirmation is the caller's - the same
            `deleteChatConfirmation` the group panel uses - so there is one
            wording rather than two. */}
        <Show when={sections().includes("delete-chat") && props.onDeleteChat !== null}>
          <section class="lk-card danger">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="trash" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{DELETE_CHAT_CONTROL()}</span>
                <span class="lk-card-row-sub">{t("chat.delete.sub")}</span>
              </div>
              <button
                type="button"
                class="danger lk-card-row-action"
                disabled={props.busy()}
                onClick={() => props.onDeleteChat?.()}
              >
                {t("common.delete")}
              </button>
            </div>
          </section>
        </Show>
      </div>
    </article>
  );
}
