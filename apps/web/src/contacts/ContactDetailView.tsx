// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The selected contact in the main pane (docs/ux/client-redesign.md section
 * 5.3): avatar, name, @username; Message, Call and Video; the safety number
 * as the one verification surface; Rename; Remove behind the in-page
 * confirmation. No key fingerprint anywhere on it.
 *
 * Call and Video are the chat's own entry points, reached through the
 * 1x1 chat; without a chat on this device they are disabled rather than
 * shown as something they are not.
 */
import { createEffect, createSignal, For, on, Show, type JSX } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon, type IconName } from "../ui/icons";
import type { AcceptedContact } from "./local-store";
import { computeSafetyNumber } from "./safety";
import { safetyNumberRows } from "./safety-number-format";

export interface ContactDetailViewProps {
  readonly contact: AcceptedContact;
  /** The route of the 1x1 chat with this contact on this device, or `null`. */
  readonly chatHref: string | null;
  /** Whether Message can create the chat when there is none yet. */
  readonly canStartChat: boolean;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly myPubkey: () => Uint8Array | null;
  readonly onBack: () => void;
  readonly onMessage: () => void;
  readonly onCall: (withVideo: boolean) => void;
  /** Set the viewer's own name for this contact; blank clears it. */
  readonly onRename: (newName: string) => void;
  /** Called after the user confirmed the removal. */
  readonly onRemove: () => void;
}

export function ContactDetailView(props: ContactDetailViewProps): JSX.Element {
  const profiles = useProfiles();
  const resolved = (): ReturnType<typeof profiles.resolve> =>
    profiles.resolve(props.contact.signaturePubkeyB64);
  const name = (): string => memberNameOf(resolved());

  const [numberOpen, setNumberOpen] = createSignal(false);
  const [number, setNumber] = createSignal<string | null>(null);
  const [numberBusy, setNumberBusy] = createSignal(false);
  const [numberError, setNumberError] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");

  // Another contact selected: its number is a different one.
  createEffect(
    on(
      () => props.contact.signaturePubkeyB64,
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
      setNumber(await computeSafetyNumber(my, base64Decode(props.contact.signaturePubkeyB64)));
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
    setDraft(props.contact.displayName);
    setEditing(true);
  }
  function saveEdit(): void {
    props.onRename(draft());
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
    const callTitle = noChat ? "Send a message first to start a call." : undefined;
    return [
      {
        label: "Message",
        icon: "chat",
        disabled: props.busy() || (noChat && !props.canStartChat),
        title:
          noChat && !props.canStartChat
            ? "This contact was added before chats could be started from here. Ask them to message you."
            : undefined,
        run: props.onMessage,
      },
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
        <button
          type="button"
          class="ghost small lk-narrow-only"
          style={{ "align-self": "flex-start" }}
          onClick={props.onBack}
        >
          <Icon name="back" size={16} />
          Contacts
        </button>

        <header class="lk-contact-head">
          <Avatar label={name()} id={props.contact.signaturePubkeyB64} size="xl" />
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

        <section class="lk-card">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="pencil" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">Name</span>
              <span class="lk-card-row-sub">
                {props.contact.displayName.length > 0
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
      </div>
    </article>
  );
}
