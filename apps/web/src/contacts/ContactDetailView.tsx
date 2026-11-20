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
import { accountSuspended } from "../auth/suspension-state";
import { AutoTranslateSwitch } from "../translation/ConversationControls";
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { DELETE_CHAT_CONTROL } from "../chat/membership-ended";
import { personInfoSections, sectionsWhileSuspended } from "../chat/conversation-info";
import { uiRules } from "../chat/ui-rules";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { visibilitySetting } from "../reach/visibility-store";
import { TheirContactsCanFindMeRow } from "../settings/TheirContactsCanFindMeRow";
import { showsTheirContactsSwitch, viewOf } from "../settings/who-can-find-me";
import { Avatar } from "../ui/Avatar";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon, type IconName } from "../ui/icons";
import { t } from "../l10n";
import { namedActionKey } from "../ui/confirm-dialog";
import { createLiveText } from "../l10n/live-text";
import { computeSafetyNumber } from "./safety";
import { messageAction, type MessageAction } from "./message-action";
import { safetyNumberRows } from "./safety-number-format";
import { isBlockedKeyB64 } from "../moderation/block-registry";
import { isSuspendedKeyB64 } from "../moderation/suspension-marks";
import { personMark } from "../moderation/suspension-rules";
import { blockPerson, confirmBlock, unblockPerson } from "../moderation/block-actions";
import { openReport } from "../moderation/report-session";
import { CONVERSATION_CARD } from "../moderation/report-model";

export interface ContactDetailViewProps {
  /** The person's Ed25519 identity key, base64 - who this screen is about. */
  readonly personKeyB64: string;
  /** The viewer's own name for them; empty when they have not renamed them. */
  readonly localName: string;
  /** The route of the 1x1 chat with this person on this device, or `null`. */
  readonly chatHref: string | null;
  /** Whether the person's account id is known: with no chat on this device,
   *  Message starts one when it is, and is unavailable when it is not
   *  (`message-action.ts`). */
  readonly accountKnown: boolean;
  /** `docs/ux/who-can-find-me.md` section 6.4 - the line under Message for a
   *  contact with no chat here: a request standing in Sent, or that nothing
   *  can reach them. `null` for none. */
  readonly messageLine?: () => string | null;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly myPubkey: () => Uint8Array | null;
  /** The narrow-screen way back to the list. */
  readonly onBack: () => void;
  /**
   * `docs/ux/conversation-info.md` section 3.1 — set when this screen was
   * opened from another screen it now covers (a conversation, a group's info,
   * a search result): the back icon is shown at every width and returns
   * there, in place of the narrow-only way back to the list. Absent when
   * nothing is covered (the Contacts list beside it).
   */
  readonly onBackToOrigin?: (() => void) | undefined;
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
   * Whether the person is on this device's contact list. `false` only for a
   * person the viewer removed whose one-to-one remains: the card then offers
   * `Add to contacts` in place of Rename and Remove (amended 2026-10-24).
   */
  readonly isContact: boolean;
  /** The person's account id, the name "Who can find me" has for them, or
   *  `null`: such a contact cannot be chosen, and has no switch there. */
  readonly accountIdHex: string | null;
  /** Put the person back on the contact list. Local; sends nothing. */
  readonly onAddContact: () => void;
  /**
   * Forget this conversation and its transcript (ADR-0045 UX 6.2.4). `null`
   * where this screen is not about a conversation at all, in which case there
   * is nothing to delete; the block is then absent whatever the rule says.
   */
  readonly onDeleteChat: (() => void) | null;
  /** ADR-0082 / UX 3.7 — the one-to-one conversation with this person (its
   *  group id, standard base64), for `Translate automatically`; `null` or
   *  absent when there is none. */
  readonly conversationId?: string | null;
}

export function ContactDetailView(props: ContactDetailViewProps): JSX.Element {
  const profiles = useProfiles();
  /** ADR-0099 section 6 - the block register's answer for this person. */
  const blocked = (): boolean => isBlockedKeyB64(props.personKeyB64);
  const [blockBusy, setBlockBusy] = createSignal(false);
  const [blockError, setBlockError] = createLiveText();
  async function onBlock(): Promise<void> {
    if (!(await confirmBlock(name()))) return;
    setBlockBusy(true);
    setBlockError(null);
    try {
      await blockPerson(props.personKeyB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[block] not written", err instanceof Error ? err.name : "unknown");
      setBlockError(() => t("common.error.unavailable"));
    } finally {
      setBlockBusy(false);
    }
  }
  async function onUnblock(): Promise<void> {
    setBlockBusy(true);
    setBlockError(null);
    try {
      await unblockPerson(props.personKeyB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[block] not written", err instanceof Error ? err.name : "unknown");
      setBlockError(() => t("common.error.unavailable"));
    } finally {
      setBlockBusy(false);
    }
  }
  /** Report from the card (UX 2.1): messages the reporter picks, optional,
   *  from the one-to-one when there is one. */
  function onReport(): void {
    const conversation = props.conversationId ?? null;
    openReport({
      target: { kind: "person", accountKeyB64: props.personKeyB64, name: name() },
      conversationKind: CONVERSATION_CARD,
      pickFrom:
        conversation === null
          ? null
          : { kind: "chat", groupIdB64: conversation, authorKeyB64: props.personKeyB64 },
    });
  }
  const contactsOfContactsOn = (): boolean => {
    const s = visibilitySetting();
    return s !== null && props.accountIdHex !== null && showsTheirContactsSwitch(viewOf(s));
  };
  // Which blocks this screen holds - the shared rule
  // (`chat/conversation-info.ts` -> `crates/lakko-ui-rules`), not this file.
  const sections = createMemo(() =>
    // ADR-0099 step 3: a suspended account adds no contact and calls nobody.
    sectionsWhileSuspended(
      personInfoSections(uiRules(), {
        membershipEnded: props.membershipEnded,
        isContact: props.isContact,
        contactsOfContactsOn: contactsOfContactsOn(),
        isBlocked: blocked(),
      }),
      accountSuspended(),
    ),
  );
  const resolved = (): ReturnType<typeof profiles.resolve> =>
    profiles.resolve(props.personKeyB64);
  const name = (): string => memberNameOf(resolved(), "a contact card");
  const [numberOpen, setNumberOpen] = createSignal(false);
  const [number, setNumber] = createSignal<string | null>(null);
  const [numberBusy, setNumberBusy] = createSignal(false);
  const [numberError, setNumberError] = createLiveText();
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  /** The key the safety number is computed over: the contact's CURRENT key.
   *  After an accepted reset (ADR-0073 R.2) the row is re-keyed, so this is
   *  the new key and the section shows the current number with its ordinary
   *  copy; it never says the number changed. */
  const safetyKeyB64 = (): string => props.personKeyB64;
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

  const messageState = (): MessageAction =>
    messageAction({ hasChat: props.chatHref !== null, accountKnown: props.accountKnown });
  const unavailableLine = (): string | null =>
    messageState() === "unavailable" ? t("contacts.detail.message_unavailable") : null;

  const actions = (): ReadonlyArray<{
    label: string;
    icon: IconName;
    disabled: boolean;
    title: string | undefined;
    run: () => void;
  }> => {
    const noChat = props.chatHref === null;
    const callTitle = noChat ? t("contacts.detail.call_needs_chat") : undefined;
    // Message is offered wherever the card was opened from (spec section
    // 2.1); whether it appears at all is the rule's call, not this file's.
    const message =
      !sections().includes("message")
        ? []
        : [
            {
              // Progress while Message founds the one-to-one.
              label:
                props.busy() && messageState() === "start"
                  ? t("common.creating")
                  : t("contacts.detail.message"),
              icon: "chat-fill" as IconName,
              disabled: props.busy() || messageState() === "unavailable",
              title: unavailableLine() ?? undefined,
              run: () => props.onMessage(),
            },
          ];
    // ADR-0045 UX 6.2.8 item 4 - gone once this device's membership has
    // ended, which is what the rule's `call` block records. `exportSecret`
    // is gated on `is_active()` and raises, so the tiles would fail closed
    // anyway; a control that always errors is the shape 6.1.1 rejects, and a
    // call control asserts a live E2EE session there is not.
    // ADR-0099 step 3: a suspended account places no call.
    const calls = !sections().includes("call") || accountSuspended()
      ? []
      : [
          {
            label: t("chat.call.voice"),
            icon: "phone-fill" as IconName,
            disabled: props.busy() || noChat,
            title: callTitle,
            run: () => props.onCall(false),
          },
          {
            label: t("chat.media.video"),
            icon: "video-fill" as IconName,
            disabled: props.busy() || noChat,
            title: callTitle,
            run: () => props.onCall(true),
          },
        ];
    return [...message, ...calls];
  };

  return (
    <article>
      <div class="lk-page lk-stack lk-contact-detail">
        <Show
          when={props.onBackToOrigin}
          fallback={
            <button
              type="button"
              class="ghost small lk-narrow-only"
              style={{ "align-self": "flex-start" }}
              onClick={() => props.onBack()}
            >
              <Icon name="chevron-back" size={20} />
              {t("nav.contacts")}
            </button>
          }
        >
          {(back) => (
            <button
              type="button"
              class="lk-icon-button"
              style={{ "align-self": "flex-start" }}
              aria-label={t("common.back")}
              title={t("common.back")}
              onClick={() => back()()}
            >
              <Icon name="chevron-back" size={24} />
            </button>
          )}
        </Show>

        <header class="lk-contact-head">
          <Avatar label={name()} id={props.personKeyB64} size="xl" personKey={props.personKeyB64} playVideo />
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
          {/* ADR-0099 UX 1.4 - a muted line under a blocked person's name. */}
          <Show when={blocked()}>
            <div class="lk-meta">{t("block.card_line")}</div>
          </Show>
          {/* ADR-0099 step 4, UX 3 - a muted line under a suspended person's
              name; a block's line wins. */}
          <Show when={personMark(blocked(), isSuspendedKeyB64(props.personKeyB64)) === "suspended"}>
            <div class="lk-meta">{t("suspended.card_line")}</div>
          </Show>
        </header>

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
                  <Icon name={a.icon} size={24} />
                  <span>{a.label}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show when={sections().includes("message") ? (unavailableLine() ?? props.messageLine?.() ?? null) : null}>
          {(line) => (
            <p class="lk-meta" role="status">
              {line()}
            </p>
          )}
        </Show>
        <Show when={props.error()}>{(message) => <div class="form-error">{message()}</div>}</Show>

        <Show when={sections().includes("safety-number")}>
        <section class="lk-card">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="shield-check" size={20} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("contacts.safety.title")}</span>
              <span class="lk-card-row-sub">{t("contacts.safety.body", { name: name() })}</span>
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
                <div class="lk-safety-number" aria-label={t("contacts.safety.number")}>
                  <For each={safetyNumberRows(n())}>
                    {(row) => (
                      <div class="lk-safety-row">
                        <For each={row}>{(group) => <span>{group}</span>}</For>
                      </div>
                    )}
                  </For>
                </div>
              )}
            </Show>
          </Show>
        </section>
        </Show>

        <Show when={sections().includes("their-contacts-can-find-me") ? props.accountIdHex : null}>
          {(hex) => <TheirContactsCanFindMeRow accountIdHex={hex()} />}
        </Show>

        <Show when={sections().includes("rename-contact")}>
          <section class="lk-card">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="pencil" size={20} />
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

        {/* UX 3.7 — a setting of this device only, for the one-to-one with
            this person. */}
        <Show when={props.conversationId}>
          {(conversation) => <AutoTranslateSwitch conversation={conversation()} />}
        </Show>

        <Show when={sections().includes("add-contact")}>
          <section class="lk-card">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="person-plus" size={20} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{t("contacts.add_back.control")}</span>
                <span class="lk-card-row-sub">{t("contacts.add_back.sub", { name: name() })}</span>
              </div>
              <button
                type="button"
                class="lk-card-row-action"
                disabled={props.busy()}
                onClick={() => props.onAddContact()}
              >
                {t("contacts.add_back.action")}
              </button>
            </div>
          </section>
        </Show>

        {/* ADR-0099 UX 1.1 / 2.1 - the danger block: Report, then Block or
            Unblock, then Remove contact (the shared rule's order). */}
        <Show when={sections().includes("report")}>
          <section class="lk-card danger">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="flag" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{t("report.action")}</span>
              </div>
              <button type="button" class="danger lk-card-row-action" onClick={onReport}>
                {t("report.action")}
              </button>
            </div>
          </section>
        </Show>
        <Show when={sections().includes("block") || sections().includes("unblock")}>
          <section class="lk-card danger">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="shield-exclamation" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">
                  {sections().includes("unblock") ? t("block.unblock") : t("block.action")}
                </span>
              </div>
              <button
                type="button"
                class="danger lk-card-row-action"
                disabled={blockBusy()}
                onClick={() => void (sections().includes("unblock") ? onUnblock() : onBlock())}
              >
                {blockBusy()
                  ? t("common.saving")
                  : sections().includes("unblock")
                    ? t("block.unblock")
                    : t("block.action")}
              </button>
            </div>
            <Show when={blockError()}>{(m) => <div class="form-error">{m()}</div>}</Show>
          </section>
        </Show>
        <Show when={sections().includes("remove-contact")}>
          <section class="lk-card danger">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="person-minus" size={20} />
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
                <Icon name="trash" size={20} />
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
