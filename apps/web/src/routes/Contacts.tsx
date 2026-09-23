// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Contacts (docs/ux/client-redesign.md section 5.3): a two-pane screen. The
 * list pane (`contacts/ContactListPane.tsx`) holds requests, sent requests
 * and the alphabetical contacts; the main pane shows the selected contact
 * (`contacts/ContactDetailView.tsx`) or what the add menu opened: add by
 * username, the invite links (`contacts/InviteLinksPanel.tsx`) or a call
 * link. A narrow screen shows one pane at a time.
 *
 * This file owns the state and every mutation:
 *
 *   - **send** a request by username (`ContactsClient.sendContactRequest`)
 *     and remember it under Sent until the acceptance lands;
 *   - **accept / decline** incoming requests, held in memory by the
 *     session-wide `<MailboxProvider>` whose long-poll fills them (no
 *     refresh anywhere); Accept also acks and founds the 1x1 chat;
 *   - the **accepted** list (`contacts/local-store.ts`): rename, remove,
 *     and the link to a contact's chat;
 *   - **Message / Call / Video** on the detail: the chat's own entry points,
 *     reached through the 1x1 chat (created here when there is none yet).
 *
 * Identity availability: every flow that needs the user's Identity reads
 * `useAuth().identitySecrets()`. A reload restores those secrets with the
 * session, so a `null` here is transient; the fallback panel offers a real
 * way back to the sign-in form.
 */
import { createEffect, createMemo, createSignal, on, Show, type JSX } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { recordConversationKind } from "../chat/conversation-kind-store";
import { ContactsClient } from "../contacts/client";
import type { IncomingContactRequest, SendResult } from "../contacts/client";
import { ContactDetailView } from "../contacts/ContactDetailView";
import { ContactListPane, type AddMode } from "../contacts/ContactListPane";
import { waitingSentRequests, type SentRequestRecord } from "../contacts/contact-list-model";
import { atLabel, formatContactsError } from "../contacts/errors";
import { InviteLinksPanel } from "../contacts/InviteLinksPanel";
import {
  loadAccepted,
  removeAccepted,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { useMailbox } from "../contacts/MailboxContext";
import {
  forgetSentRequest,
  loadSentRequests,
  recordSentRequest,
} from "../contacts/sent-requests";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { mayLookUp, usernameReason, type UsernameCheck } from "../profile/rules";
import { createUsernameCheck } from "../profile/username-field";
import { Icon } from "../ui/icons";
import { paths } from "./paths";

export default function Contacts(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  // 1.16e6c2: hand the chat-context wires (canonical Identity +
  // post-mutation snapshot persist) to ContactsClient so every
  // MLS state mutation in `startGroupWith` / `joinGroupFromWelcome`
  // flushes to IDB before the call returns. The closures capture
  // `chats` + `auth` lazily, so they always see the current session.
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
    // ADR-0048 §2 — every outgoing request carries this account's profile.
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  const identity = createMemo(() => auth.identitySecrets());

  return (
    <Show
      when={identity() !== null}
      fallback={
        <article>
          <NeedsReauth />
        </article>
      }
    >
      <ConnectedContacts client={client} />
    </Show>
  );
}

// ----------------------------------------------------------------------
// "Connected" panel — identity secrets are available
// ----------------------------------------------------------------------

function ConnectedContacts(props: { readonly client: ContactsClient }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const navigate = useNavigate();
  // The Show wrapper above guarantees identitySecrets() is non-null on every
  // read inside this subtree, but TS doesn't narrow across component
  // boundaries — a tiny accessor that throws if it does see null keeps the
  // inner code clean.
  const requireIdentity = (): NonNullable<ReturnType<typeof auth.identitySecrets>> => {
    const s = auth.identitySecrets();
    if (s === null) throw new Error("identity secrets unavailable");
    return s;
  };
  const accountIdB64 = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  // ---- accepted -------------------------------------------------
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  const mailbox = useMailbox();
  // A contact ack the mailbox loop upserted while this route was open.
  createEffect(
    on(
      () => mailbox.acceptedVersion(),
      () => setAccepted(loadAccepted()),
      { defer: true },
    ),
  );

  // ---- sent requests --------------------------------------------
  const [sentVersion, setSentVersion] = createSignal(0);
  const sent = createMemo<SentRequestRecord[]>(() => {
    sentVersion();
    const account = accountIdB64();
    if (account === null) return [];
    const acceptedHexes = new Set<string>();
    for (const c of accepted()) {
      if (c.accountIdHex !== undefined) acceptedHexes.add(c.accountIdHex);
    }
    return waitingSentRequests(loadSentRequests(account), acceptedHexes);
  });
  function onDismissSent(record: SentRequestRecord): void {
    const account = accountIdB64();
    if (account === null) return;
    forgetSentRequest(account, record.recipientAccountIdHex);
    setSentVersion((v) => v + 1);
  }

  // ---- send -----------------------------------------------------
  const [searchInput, setSearchInput] = createSignal("");
  const [introInput, setIntroInput] = createSignal("");
  const [sendBusy, setSendBusy] = createSignal(false);
  const [sendError, setSendError] = createSignal<string | null>(null);
  const [sendNotice, setSendNotice] = createSignal<string | null>(null);
  // ADR-0048 §4 — no lookup leaves the device for a name that fails the rule.
  const searchCheck = createUsernameCheck(searchInput);

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setSendError(null);
    setSendNotice(null);
    if (!mayLookUp(searchCheck())) return;
    setSendBusy(true);
    try {
      const introBytes = new TextEncoder().encode(introInput());
      const result: SendResult = await props.client.sendContactRequest(
        requireIdentity(),
        searchInput(),
        introBytes,
      );
      const account = accountIdB64();
      if (account !== null) {
        recordSentRequest(account, {
          recipientUsername: result.recipientUsername,
          recipientAccountIdHex: syncGroupIdHex(result.recipientAccountId),
          sentAtMs: Date.now(),
        });
        setSentVersion((v) => v + 1);
      }
      setSendNotice(`Request sent to @${result.recipientUsername}. It waits under Sent until they accept.`);
      setSearchInput("");
      setIntroInput("");
    } catch (err) {
      setSendError(formatContactsError(err, atLabel(searchInput())));
    } finally {
      setSendBusy(false);
    }
  }

  // ---- incoming requests ----------------------------------------
  // Owned by the session-wide `<MailboxProvider>`: its long-poll loop
  // dispatches requests into this list, acks into the address book and
  // Welcomes into joined groups, whether or not this route is mounted.
  const [acceptError, setAcceptError] = createSignal<string | null>(null);

  /** Found the 1x1 chat with a contact and link it back to the contact
   *  record (ADR-0021 follow-up). Shared by Accept and by Message on a
   *  contact that has no chat yet. */
  async function foundChatWith(
    contact: AcceptedContact,
    peerAccountId: Uint8Array,
    peerPubkey: Uint8Array,
  ): Promise<string> {
    const result = await props.client.startGroupWith(requireIdentity(), peerAccountId, peerPubkey);
    // ADR-0048 — a 1x1 chat is labelled by the resolver (the peer's name);
    // this label is only the fallback.
    const active = chats.add({ label: "Direct chat", group: result.group });
    // ADR-0048 §2 — founding a group is joining it.
    profiles.groupJoined(active.groupIdB64);
    // docs/ux/client-redesign.md section 3 — a contact's conversation is a
    // chat, filed under Chats until a third person joins.
    const account = accountIdB64();
    if (account !== null) recordConversationKind(account, active.groupIdB64, "chat");
    setAccepted(upsertAccepted({ ...contact, groupIdB64: active.groupIdB64 }));
    return paths.chat(base64UrlEncodeNoPad(active.group.groupId));
  }

  async function onAccept(req: IncomingContactRequest): Promise<void> {
    // Always upsert into the accepted list first — the user has explicitly
    // trusted this peer; the group-creation step below is best-effort and a
    // network blip shouldn't undo the acceptance.
    const keyB64 = base64Encode(req.fromPubkey);
    const contact: AcceptedContact = {
      signaturePubkeyB64: keyB64,
      // ADR-0048 — no local rename on accept (keep one the user already set);
      // the name shown comes from the resolver.
      displayName: loadAccepted().find((c) => c.signaturePubkeyB64 === keyB64)?.displayName ?? "",
      acceptedAtMs: Date.now(),
      // ADR-0021 — persist the peer's account_id (hex) when the request
      // carried it, so the SFU group-call roster can resolve names.
      ...(req.fromAccountId ? { accountIdHex: syncGroupIdHex(req.fromAccountId) } : {}),
    };
    setAccepted(upsertAccepted(contact));
    mailbox.removePending(req.messageId);
    setAcceptError(null);

    // 1.9b1: ship a ContactAck back to the original sender's mailbox so
    // their UI can flip from "pending" to "accepted". Best-effort — a
    // failure here doesn't unwind the local accept.
    try {
      await props.client.sendContactAck(requireIdentity(), req);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("sendContactAck failed", err);
    }

    // 1.16e3b: Accept-driven group creation. Requires the v2 wire format —
    // surface a clean error for legacy senders.
    if (req.fromAccountId === null) {
      setAcceptError(
        "This request came from an older version of Lakko. Ask them to update and send it again.",
      );
      return;
    }
    try {
      await foundChatWith(contact, req.fromAccountId, req.fromPubkey);
    } catch (err) {
      // Don't unwind the acceptance; just surface the error.
      setAcceptError(formatContactsError(err, memberNameOf(profiles.resolve(req.fromPubkey))));
    }
  }

  function onDecline(req: IncomingContactRequest): void {
    mailbox.removePending(req.messageId);
  }

  // ---- rename / remove ------------------------------------------
  function onRemoveAccepted(contact: AcceptedContact): void {
    setAccepted(removeAccepted(contact.signaturePubkeyB64));
  }
  /** ADR-0048 §5 — the viewer's LOCAL RENAME of a contact. A blank value
   *  clears it, so the contact's own name shows again. Re-upserts by the
   *  same signature pubkey, so the link + account_id + group id are kept. */
  function onRenameAccepted(contact: AcceptedContact, newName: string): void {
    setAccepted(upsertAccepted({ ...contact, displayName: newName.trim() }));
  }

  // ---- layout: the list pane + the detail / add pane ----------------
  const [selectedKey, setSelectedKey] = createSignal<string | null>(null);
  // The add menu's pick: add by username, or the invite links.
  // `null` = nothing open.
  const location = useLocation();
  const [adding, setAdding] = createSignal<AddMode | null>(null);

  // `docs/ux/navigation.md` section 4 - the Chats search looks a name up in
  // both namespaces; a person lands here, because sending a contact request
  // is this screen's flow and asks for an introduction the search field has
  // no room for. The name travels rather than being typed twice.
  const handedOver = (): string | undefined => {
    const state = location.state as { readonly username?: string } | null;
    return typeof state?.username === "string" ? state.username : undefined;
  };
  createEffect(() => {
    const username = handedOver();
    if (username === undefined) return;
    setAdding("username");
    setSearchInput(username);
  });
  const [filter, setFilter] = createSignal("");
  const selected = (): AcceptedContact | undefined =>
    accepted().find((c) => c.signaturePubkeyB64 === selectedKey());

  /** The 1x1 chat with this contact, if this device holds one: linked at
   *  accept, or found by its two-member roster. */
  const chatWith = (c: AcceptedContact): string | null => {
    if (c.groupIdB64 !== undefined) {
      return paths.chat(base64UrlEncodeNoPad(base64Decode(c.groupIdB64)));
    }
    const own = chats.identity();
    const ownB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    for (const g of chats.groups()) {
      let keys: string[] = [];
      try {
        keys = (g.group.members() as Array<{ signaturePublicKey: Uint8Array }>).map((m) =>
          base64Encode(m.signaturePublicKey),
        );
      } catch {
        continue;
      }
      if (keys.length === 2 && keys.includes(c.signaturePubkeyB64) && keys.includes(ownB64 ?? "")) {
        return paths.chat(base64UrlEncodeNoPad(g.group.groupId));
      }
    }
    return null;
  };

  // ---- detail actions --------------------------------------------
  const [detailBusy, setDetailBusy] = createSignal(false);
  const [detailError, setDetailError] = createSignal<string | null>(null);
  createEffect(on(selectedKey, () => setDetailError(null), { defer: true }));

  /** Message: open the chat, or create it first when this device has none
   *  and the contact's account id is known. */
  async function onMessage(c: AcceptedContact): Promise<void> {
    const existing = chatWith(c);
    if (existing !== null) {
      navigate(existing);
      return;
    }
    if (c.accountIdHex === undefined) return;
    setDetailBusy(true);
    setDetailError(null);
    try {
      const href = await foundChatWith(
        c,
        hexToBytes(c.accountIdHex),
        base64Decode(c.signaturePubkeyB64),
      );
      navigate(href);
    } catch (err) {
      setDetailError(formatContactsError(err, memberNameOf(profiles.resolve(c.signaturePubkeyB64))));
    } finally {
      setDetailBusy(false);
    }
  }

  /** Call / Video: the chat's own call buttons, pressed on arrival. */
  function onCall(c: AcceptedContact, withVideo: boolean): void {
    const href = chatWith(c);
    if (href === null) return;
    navigate(`${href}?call=${withVideo ? "video" : "audio"}`);
  }

  return (
    <div
      class="lk-split"
      data-open={selected() !== undefined || adding() !== null ? "true" : "false"}
    >
      <ContactListPane
        filter={filter}
        onFilter={setFilter}
        onAdd={(mode) => {
          setSelectedKey(null);
          setAdding(mode);
        }}
        pending={mailbox.pending}
        pendingBusy={mailbox.busy}
        error={() => acceptError() ?? mailbox.error()}
        onAccept={(req) => void onAccept(req)}
        onDecline={onDecline}
        sent={sent}
        onDismissSent={onDismissSent}
        contacts={accepted}
        selectedKey={selectedKey}
        onSelect={(key) => {
          setAdding(null);
          setSelectedKey(key);
        }}
      />
      <div class="lk-main">
        <Show
          when={selected()}
          fallback={
            <article>
              <div class="lk-page lk-stack" style={{ "max-width": "560px" }}>
                <button
                  type="button"
                  class="ghost small lk-narrow-only"
                  style={{ "align-self": "flex-start" }}
                  onClick={() => setAdding(null)}
                >
                  <Icon name="back" size={16} />
                  Contacts
                </button>
                <Show when={adding() === "username" || adding() === null}>
                  <div class="lk-page-head">
                    <h1>Add a contact</h1>
                    <p>Requests are end-to-end encrypted. Lakko's servers cannot read them.</p>
                  </div>
                  <SendPanel
                    searchCheck={searchCheck}
                    searchInput={searchInput}
                    setSearchInput={setSearchInput}
                    introInput={introInput}
                    setIntroInput={setIntroInput}
                    busy={sendBusy}
                    error={sendError}
                    notice={sendNotice}
                    onSubmit={(e) => void onSend(e)}
                  />
                </Show>
                <Show when={adding() === "invite"}>
                  <div class="lk-page-head">
                    <h1>Invite links</h1>
                    <p>Share a link with someone you want to hear from.</p>
                  </div>
                  <InviteLinksPanel />
                </Show>
              </div>
            </article>
          }
        >
          {(c) => (
            <ContactDetailView
              contact={c()}
              chatHref={chatWith(c())}
              canStartChat={c().accountIdHex !== undefined}
              busy={detailBusy}
              error={detailError}
              myPubkey={() => chats.identity()?.signaturePublicKey ?? null}
              onBack={() => setSelectedKey(null)}
              onMessage={() => void onMessage(c())}
              onCall={(withVideo) => onCall(c(), withVideo)}
              onRename={(name) => onRenameAccepted(c(), name)}
              onRemove={() => {
                onRemoveAccepted(c());
                setSelectedKey(null);
              }}
            />
          )}
        </Show>
      </div>
    </div>
  );
}

// ----------------------------------------------------------------------
// Add by username
// ----------------------------------------------------------------------

interface SendPanelProps {
  readonly searchCheck: () => UsernameCheck;
  readonly searchInput: () => string;
  readonly setSearchInput: (v: string) => void;
  readonly introInput: () => string;
  readonly setIntroInput: (v: string) => void;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly notice: () => string | null;
  readonly onSubmit: (e: SubmitEvent) => void;
}

function SendPanel(props: SendPanelProps): JSX.Element {
  return (
    <section class="lk-card">
      <h2>Add by username</h2>
      <form onSubmit={props.onSubmit} class="lk-stack" style={{ gap: "12px" }}>
        <label>
          Username
          <input
            type="text"
            placeholder="@alice"
            autocomplete="off"
            spellcheck={false}
            required
            value={props.searchInput()}
            onInput={(e) => props.setSearchInput(e.currentTarget.value)}
            disabled={props.busy()}
            aria-invalid={props.searchCheck().kind === "invalid"}
          />
        </label>
        <Show when={usernameReason(props.searchCheck())}>
          {(reason) => (
            <div class="form-error" role="status">
              {reason()}
            </div>
          )}
        </Show>
        <label>
          Message (optional)
          <textarea
            rows={2}
            placeholder="Hi, this is Alice from the conference."
            value={props.introInput()}
            onInput={(e) => props.setIntroInput(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        <Show when={props.error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
        <Show when={props.notice()}>{(m) => <div class="form-info">{m()}</div>}</Show>
        <div class="lk-actions">
          <button
            class="primary"
            type="submit"
            disabled={props.busy() || !mayLookUp(props.searchCheck())}
          >
            <Icon name="send" size={18} />
            {props.busy() ? "Sending..." : "Send"}
          </button>
        </div>
      </form>
    </section>
  );
}

function NeedsReauth(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  // `/login` is an anonymous-only route: linking to it while signed in
  // bounces straight back home. Sign out first, then go there — the same
  // sequence the sidebar's Log out uses.
  function signInAgain(): void {
    auth.logout();
    navigate(paths.login, { replace: true });
  }
  return (
    <div class="lk-page lk-stack" style={{ "max-width": "560px" }}>
      <section class="lk-card">
        <h2>Sign in again</h2>
        <p class="lk-card-desc">
          The keys this session needs for contacts are not available. Sign in again to use
          Contacts.
        </p>
        <div class="lk-actions">
          <button class="primary" type="button" onClick={signInAgain}>
            Sign in again
          </button>
        </div>
      </section>
    </div>
  );
}

/** Lower-case unseparated hex to bytes (the `accountIdHex` shape). */
function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
