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
 * Contacts, and it is what a one-to-one conversation's header opens
 * (`docs/ux/conversation-info.md` section 2.1) - a second one would be a
 * second thing to keep in step, which is how the two drift apart.
 *
 * That second caller is why the props below take a key and a name rather
 * than a contact record, and why Back, Message, Rename and Remove are all
 * nullable. Message is the way INTO the conversation, so it is absent when
 * the conversation is already open behind this screen. The
 * other person in a one-to-one is not necessarily in the viewer's contact
 * list, and a chat with someone who is not still opens a PERSON - with the
 * two contact-list blocks simply absent - never the group panel. Which
 * blocks appear is decided in `chat/conversation-info.ts`, where it can be
 * tested; this file renders what it is handed.
 */
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { personInfoSections } from "../chat/conversation-info";
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
  /** The way INTO the conversation. `null` where the conversation is
   *  already open behind this screen, and the block is then absent. */
  readonly onMessage: (() => void) | null;
  readonly onCall: (withVideo: boolean) => void;
  /** Set the viewer's own name for this person; blank clears it. `null` when
   *  they are not in the contact list, so there is no name to set. */
  readonly onRename: ((newName: string) => void) | null;
  /** Called after the user confirmed the removal. `null` when they are not
   *  in the contact list, so there is nothing to remove. */
  readonly onRemove: (() => void) | null;
}

export function ContactDetailView(props: ContactDetailViewProps): JSX.Element {
  const profiles = useProfiles();
  // Which blocks this screen holds (`chat/conversation-info.ts`). Being in
  // the contact list is what the two editable blocks hang on, and the one
  // signal for it is whether the caller handed over a way to remove them.
  const sections = createMemo(() =>
    personInfoSections({ isContact: props.onRemove !== null }),
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
    // Message is the way INTO the conversation, so it is absent when the
    // conversation is already open behind this screen.
    const open = props.onMessage;
    const message =
      open === null
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
              run: open,
            },
          ];
    return [
      ...message,
      {
        label: "Call",
        icon: "phone",
        disabled: props.busy() || noChat,
        title: callTitle,
        run: () => props.onCall(false),
      },
      {
        label: "Video",
        icon: "video",
        disabled: props.busy() || noChat,
        title: callTitle,
        run: () => props.onCall(true),
      },
    ];
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
        <Show when={sections().includes("contact-name")}>
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
      </div>
    </article>
  );
}
