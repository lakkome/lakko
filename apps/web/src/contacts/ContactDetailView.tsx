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
 * from the conversation, Message simply returns to it. The one caller that
 * still renders this view inside the chat is the fallback for a one-to-one
 * whose contact record cannot be found (a roster that cannot be read), which
 * is why the props take a key and a name rather than a contact record, and
 * why Back, Rename and Remove are nullable: a chat with someone who is not
 * in the contact list still opens a PERSON - with the two contact-list
 * blocks simply absent - never the group panel.
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
  /** The narrow-screen way back to the list; `null` where there is no list. */
  readonly onBack: (() => void) | null;
  /** Message: open the conversation with this person. Always offered; a
   *  caller that IS the conversation passes the way back to it. */
  readonly onMessage: () => void;
  readonly onCall: (withVideo: boolean) => void;
  /** Set the viewer's own name for this person; blank clears it. `null` when
   *  they are not in the contact list, so there is no name to set. */
  readonly onRename: ((newName: string) => void) | null;
  /** Called after the user confirmed the removal. `null` when they are not
   *  in the contact list, so there is nothing to remove. */
  readonly onRemove: (() => void) | null;
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
}

export function ContactDetailView(props: ContactDetailViewProps): JSX.Element {
  const profiles = useProfiles();
  // Which blocks this screen holds - the shared rule
  // (`chat/conversation-info.ts` -> `crates/lakko-ui-rules`), not this file.
  //
  // Being in the contact list is what the two editable blocks hang on, and the
  // one signal for it is whether the caller handed over a way to remove them.
  const sections = createMemo(() =>
    personInfoSections(uiRules(), {
      isContact: props.onRemove !== null,
      membershipEnded: props.membershipEnded,
    }),
  );
  const resolved = (): ReturnType<typeof profiles.resolve> =>
    profiles.resolve(props.personKeyB64);
  const name = (): string => memberNameOf(resolved());

  const [numberOpen, setNumberOpen] = createSignal(false);
  const [number, setNumber] = createSignal<string | null>(null);
  const [numberBusy, setNumberBusy] = createSignal(false);
  const [numberError, setNumberError] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");

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

  async function ensureNumber(): Promise<void> {
    if (number() !== null) return;
    const my = props.myPubkey();
    if (my === null) {
      setNumberError("Sign in again to see the safety number.");
      return;
    }
    setNumberBusy(true);
    setNumberError(null);
    try {
      setNumber(await computeSafetyNumber(my, base64Decode(props.personKeyB64)));
    } catch (e) {
      setNumberError(e instanceof Error ? e.message : String(e));
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
    props.onRename?.(draft());
    setEditing(false);
  }

  async function confirmRemove(): Promise<void> {
    const ok = await requestConfirm({
      title: "Remove this contact?",
      body:
        "Any chats you share stay as they are; only your contact list changes. " +
        "You can add them again later.",
      confirmLabel: `Remove ${name()}`,
      cancelLabel: "Cancel",
      tone: "danger",
    });
    if (ok) props.onRemove?.();
  }

  const actions = (): ReadonlyArray<{
    label: string;
    icon: IconName;
    disabled: boolean;
    title: string | undefined;
    run: () => void;
  }> => {
    const noChat = props.chatHref === null;
    const callTitle = noChat ? "Send a message first to start a call." : undefined;
    // Message is offered wherever the card was opened from (spec section
    // 2.1); whether it appears at all is the rule's call, not this file's.
    const message =
      !sections().includes("message")
        ? []
        : [
            {
              label: "Message",
              icon: "chat" as IconName,
              disabled: props.busy() || (noChat && !props.canStartChat),
              title:
                noChat && !props.canStartChat
                  ? "This contact was added before chats could be started from here. Ask them to message you."
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
            label: "Call",
            icon: "phone" as IconName,
            disabled: props.busy() || noChat,
            title: callTitle,
            run: () => props.onCall(false),
          },
          {
            label: "Video",
            icon: "video" as IconName,
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
        <Show when={props.onBack}>
          {(back) => (
            <button
              type="button"
              class="ghost small lk-narrow-only"
              style={{ "align-self": "flex-start" }}
              onClick={() => back()()}
            >
              <Icon name="back" size={16} />
              Contacts
            </button>
          )}
        </Show>

        <header class="lk-contact-head">
          <Avatar label={name()} id={props.personKeyB64} size="xl" />
          <h1>{name()}</h1>
          <Show when={resolved().username}>
            {(u) => <div class="lk-contact-username">{u()}</div>}
          </Show>
          <Show when={resolved().theirName}>
            {(their) => <div class="lk-meta">Calls themselves {their()}</div>}
          </Show>
          <Show when={resolved().usernameNotVerified}>
            <div class="lk-meta">Username not verified</div>
          </Show>
        </header>

        {/* An empty row is not a row: once a membership has ended and this
            screen is the conversation's own, there is nothing left to put in
            it. */}
        <Show when={actions().length > 0}>
          <div class="lk-detail-actions" role="group" aria-label="Contact actions">
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
        <section class="lk-card">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="shield" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">Verify safety number</span>
              <span class="lk-card-row-sub">
                Compare it with {name()} in person or on a call. Matching numbers mean nobody is
                between you.
              </span>
            </div>
            <button
              type="button"
              class="ghost small lk-card-row-action"
              style={{ color: "var(--accent)" }}
              aria-expanded={numberOpen()}
              disabled={numberBusy()}
              onClick={() => void toggleNumber()}
            >
              {numberOpen() ? "Hide" : "Show"}
            </button>
          </div>
          <Show when={numberOpen()}>
            <Show
              when={number()}
              fallback={
                <p class="lk-meta lk-center" style={{ margin: 0 }}>
                  {numberBusy() ? "Loading..." : (numberError() ?? "")}
                </p>
              }
            >
              {(n) => (
                <div class="lk-safety-number" aria-label="Safety number">
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

        {/* Only for someone already in the contact list: there is no
            local name to set for a person the viewer has not added. */}
        <Show when={sections().includes("rename-contact")}>
          <section class="lk-card">
            <div class="lk-card-row">
              <span class="lk-card-row-icon">
                <Icon name="pencil" size={18} />
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">Name</span>
                <span class="lk-card-row-sub">
                  {props.localName.length > 0
                    ? "Your name for this contact. Only you see it."
                    : "Shown as they set it. You can give them your own name."}
                </span>
              </div>
              <button
                type="button"
                class="lk-card-row-action"
                onClick={() => (editing() ? setEditing(false) : startEdit())}
              >
                Rename
              </button>
            </div>
            <Show when={editing()}>
              <div class="lk-actions" style={{ "flex-wrap": "nowrap" }}>
                <input
                  type="text"
                  value={draft()}
                  placeholder="Leave blank to use their own name"
                  aria-label="Your name for this contact"
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") saveEdit();
                    if (e.key === "Escape") setEditing(false);
                  }}
                  style={{ flex: 1 }}
                />
                <button type="button" class="primary" onClick={saveEdit}>
                  Save
                </button>
                <button type="button" class="ghost" onClick={() => setEditing(false)}>
                  Cancel
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
                <span class="lk-card-row-title">Remove contact</span>
                <span class="lk-card-row-sub">Takes {name()} off your contact list on this account.</span>
              </div>
              <button
                type="button"
                class="danger lk-card-row-action"
                disabled={props.busy()}
                onClick={() => void confirmRemove()}
              >
                Remove
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
                <span class="lk-card-row-title">{DELETE_CHAT_CONTROL}</span>
                <span class="lk-card-row-sub">
                  Removes this conversation and everything in it from this
                  device. It cannot be undone.
                </span>
              </div>
              <button
                type="button"
                class="danger lk-card-row-action"
                disabled={props.busy()}
                onClick={() => props.onDeleteChat?.()}
              >
                Delete
              </button>
            </div>
          </section>
        </Show>
      </div>
    </article>
  );
}
