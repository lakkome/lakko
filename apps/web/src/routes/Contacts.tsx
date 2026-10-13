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
import {
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { deleteConversation } from "../chat/delete-chat";
import { useLineage } from "../chat/use-lineage";
import { heldSends, persistHeldSends } from "../chat/held-send-queue";
import { usePins } from "../chat/PinContext";
import { deleteChatConfirmation } from "../chat/delete-chat-confirm";
import {
  loadLeaveRequestedAt,
  membershipEnded as membershipHasEnded,
  membershipState,
  readIsActive,
} from "../chat/membership-ended";
import { ContactsClient } from "../contacts/client";
import type { IncomingContactRequest, SendResult } from "../contacts/client";
import { ContactDetailView } from "../contacts/ContactDetailView";
import { ContactListPane, type AddMode } from "../contacts/ContactListPane";
import { waitingSentRequests, type SentRequestRecord } from "../contacts/contact-list-model";
import { atLabel, contactsErrorText } from "../contacts/errors";
import { InviteLinksPanel } from "../contacts/InviteLinksPanel";
import {
  loadAccepted,
  removeAccepted,
  subscribeAcceptedChanged,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { useMailbox } from "../contacts/MailboxContext";
import {
  forgetSentRequest,
  loadSentRequests,
  recordSentRequest,
} from "../contacts/sent-requests";
import { forgetSentIntro, rememberSentIntro } from "../contacts/sent-request-intros";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { onContactRemoved } from "../reach/reach-service";
import { mayLookUp, usernameReason, type UsernameCheck } from "../profile/rules";
import { createUsernameCheck } from "../profile/username-field";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { t } from "../l10n";
import { DIRECT_CHAT_STAND_IN } from "../chat/group-title";
import { createLiveText } from "../l10n/live-text";
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
  const lineageView = useLineage();
  const pins = usePins();
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
  // A row the one-to-one adoption rule wrote while this route was open
  // (`contacts/OneToOneContactAdoption.tsx`), or any other store write.
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setAccepted(loadAccepted()));
    onCleanup(unsubscribe);
  });

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
    void forgetSentIntro(account, record.recipientAccountIdHex);
    setSentVersion((v) => v + 1);
  }

  // ---- send -----------------------------------------------------
  const [searchInput, setSearchInput] = createSignal("");
  const [introInput, setIntroInput] = createSignal("");
  const [sendBusy, setSendBusy] = createSignal(false);
  const [sendError, setSendError] = createLiveText();
  const [sendNotice, setSendNotice] = createLiveText();
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
        // Kept under the device key, so a re-send after a rotation of the
        // account keys carries the same words (ADR-0074 section 3.5a rule 5).
        void rememberSentIntro(account, syncGroupIdHex(result.recipientAccountId), introInput());
        setSentVersion((v) => v + 1);
      }
      const recipient = `@${result.recipientUsername}`;
      setSendNotice(() => t("contacts.send.done", { name: recipient }));
      setSearchInput("");
      setIntroInput("");
    } catch (err) {
      setSendError(contactsErrorText(err, "contacts.error.send", atLabel(searchInput())));
    } finally {
      setSendBusy(false);
    }
  }

  // ---- incoming requests ----------------------------------------
  // Owned by the session-wide `<MailboxProvider>`: its long-poll loop
  // dispatches requests into this list, acks into the address book and
  // Welcomes into joined groups, whether or not this route is mounted.
  const [acceptError, setAcceptError] = createLiveText();
  /** Found the 1x1 chat with a contact and link it back to the contact
   *  record (ADR-0021 follow-up). Shared by Accept and by Message on a
   *  contact that has no chat yet. */
  async function foundChatWith(
    contact: AcceptedContact,
    peerAccountId: Uint8Array,
    peerPubkey: Uint8Array,
    replyGrantB64: string | null = null,
  ): Promise<string> {
    // ADR-0060 — accepting a request writes the 1:1 Welcome with the
    // requester's `reply` grant; a contact we message later is reached with
    // the grants we keep for them. ADR-0062 — a contact's conversation is a
    // one-to-one for good.
    const result = await props.client.startGroupWith(
      requireIdentity(),
      peerAccountId,
      peerPubkey,
      "chat",
      { replyGrantB64 },
    );
    // ADR-0048 — a 1x1 chat is labelled by the resolver (the peer's name);
    // this label is only the fallback.
    const active = chats.add({ label: DIRECT_CHAT_STAND_IN, group: result.group });
    // ADR-0048 §2 — founding a group is joining it.
    profiles.groupJoined(active.groupIdB64);
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

    // The 1:1 chat (its Welcome) goes FIRST and the ContactAck after it. Both
    // are written with the requester's `reply` grant (ADR-0060 section 6),
    // and the requester closes that key as soon as the ack arrives: an ack
    // sent first let a requester that is online close the key before the
    // Welcome landed, and the Welcome was refused (seen live 2026-09-30).
    // The ack is best-effort and always sent, the chat failing or not.
    const sendAck = async (): Promise<void> => {
      try {
        await props.client.sendContactAck(requireIdentity(), req);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("sendContactAck failed", err);
      }
    };

    // 1.16e3b: Accept-driven group creation. Requires the v2 wire format —
    // surface a clean error for legacy senders.
    if (req.fromAccountId === null) {
      await sendAck();
      setAcceptError(() => t("contacts.accept.error.old_version"));
      return;
    }
    try {
      await foundChatWith(contact, req.fromAccountId, req.fromPubkey, req.replyGrantB64);
    } catch (err) {
      // Don't unwind the acceptance; just surface the error.
      setAcceptError(
        contactsErrorText(
          err,
          "contacts.error.start_chat",
          memberNameOf(profiles.resolve(req.fromPubkey)),
        ),
      );
    }
    await sendAck();
  }

  function onDecline(req: IncomingContactRequest): void {
    mailbox.removePending(req.messageId);
  }

  // ---- rename / remove ------------------------------------------
  function onRemoveAccepted(contact: AcceptedContact): void {
    setAccepted(removeAccepted(contact.signaturePubkeyB64));
    // ADR-0060 section 1 — a removed contact keeps the `contact` key we handed
    // them; rotating it (and the `fof` key) makes it stop working at once.
    void onContactRemoved(base64Decode(contact.signaturePubkeyB64));
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
  // `docs/ux/conversation-info.md` section 3, "Except a person" - a
  // one-to-one's header opens the contact's OWN page, which is this route
  // with the person selected: `/contacts/:key` (`paths.contact`). The key in
  // the path is the base64url form of the stored standard-base64 key.
  // Picking from the list keeps selecting in place (the URL stays
  // `/contacts`); the param is how another route addresses a person.
  const params = useParams<{ readonly key?: string }>();
  createEffect(() => {
    const slug = params.key;
    if (slug === undefined) return;
    try {
      setAdding(null);
      setSelectedKey(base64Encode(base64UrlDecodeNoPad(slug)));
    } catch {
      // Not a key at all: nothing is selected, the list shows as it would
      // at `/contacts`.
      setSelectedKey(null);
    }
  });
  /** Deselect, and stop the URL naming a person who is no longer shown. */
  function clearSelection(): void {
    setSelectedKey(null);
    if (params.key !== undefined) navigate(paths.contacts, { replace: true });
  }
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
    accepted().find((c) => c.signaturePubkeyB64 === selectedKey()) ??
    // ADR-0073 R.2 — a contact re-keyed by an accepted reset while its page
    // is open stays the selected one: found by the key it had.
    accepted().find((c) => {
      const key = selectedKey();
      return key !== null && (c.previousSignaturePubkeysB64 ?? []).includes(key);
    });

  /** The registry row of the 1x1 chat with this contact, if this device
   *  holds one: the id linked at accept when the registry still holds it,
   *  else the two-member roster scan. A linked id whose chat is gone (deleted
   *  here, or never present on this device) is NOT an address: seen live
   *  2026-09-26 as a contact whose Message opened "Conversation not found". */
  const groupWith = (c: AcceptedContact): ActiveGroup | undefined => {
    if (c.groupIdB64 !== undefined) {
      const linked = chats.get(c.groupIdB64);
      if (linked !== undefined) return linked;
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
        return g;
      }
    }
    return undefined;
  };

  /** The 1x1 chat with this contact, if this device holds one - `null`
   *  otherwise, so Message starts a chat instead of opening a dead link. */
  const chatWith = (c: AcceptedContact): string | null => {
    const g = groupWith(c);
    return g === undefined ? null : paths.chat(base64UrlEncodeNoPad(g.group.groupId));
  };

  /**
   * Whether this contact's 1x1 chat has ENDED — the same two reads the
   * conversation's own header makes (`routes/Chat.tsx` `membership`):
   * `isActive` off the live handle decides, the leave record only narrows
   * the copy. `docs/ux/conversation-info.md` section 3 sends a person's
   * header here rather than to a conversation page of its own, so this card
   * is the only place on web an ended one-to-one can be closed from, as it
   * already is on iOS and Android. No chat, or a live one, reads `false`.
   */
  const chatMembershipEnded = (c: AcceptedContact): boolean => {
    const g = groupWith(c);
    if (g === undefined) return false;
    const s = auth.state();
    return membershipHasEnded(
      membershipState({
        isActive: readIsActive(g.group),
        leaveRequestedAtMs:
          s.kind === "session"
            ? loadLeaveRequestedAt(s.account.accountId, g.groupIdB64)
            : undefined,
      }),
    );
  };

  /** The chat's label for the confirmation: the local name when there is
   *  one, else what the roster would show for this person. */
  const chatLabelOf = (c: AcceptedContact): string =>
    c.displayName.trim().length > 0
      ? c.displayName.trim()
      : memberNameOf(profiles.resolve(c.signaturePubkeyB64));

  /**
   * ADR-0045 UX §6.2.4 — `Delete this chat`, for an ended one-to-one. The
   * confirmation is the only protection there is, so it states exactly what
   * is destroyed and names the chat; the removal itself is the shared
   * `chat/delete-chat.ts`, whose registry-row-last order keeps the chat
   * reachable when an earlier step fails.
   */
  async function onDeleteChat(c: AcceptedContact): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const g = groupWith(c);
    if (g === undefined) return;
    if (!(await requestConfirm(deleteChatConfirmation({ chatLabel: chatLabelOf(c) })))) {
      return;
    }
    setDetailError(null);
    setDetailBusy(true);
    try {
      // ADR-0073 R.5 — every group of the conversation, the head last.
      const view = lineageView.resolve(g.groupIdB64);
      const held = view?.held ?? [g];
      await deleteConversation({
        accountIdB64: s.account.accountId,
        segments: held.map((segment) => ({
          groupIdB64: segment.groupIdB64,
          groupIdHex: syncGroupIdHex(segment.group.groupId),
        })),
        headGroupIdB64: view?.head.groupIdB64 ?? g.groupIdB64,
        chats,
        afterSegment: (segment) => pins.forgetConversation(segment.groupIdB64),
      });
      // Nothing held for a conversation that is gone is ever sent.
      const conversationHex = view?.conversationHex ?? syncGroupIdHex(g.group.groupId);
      if (heldSends.take(conversationHex).length > 0) void persistHeldSends(s.account.accountId);
      clearSelection();
    } catch (err) {
      setDetailError(contactsErrorText(err, "chat.error.delete_chat", chatLabelOf(c)));
    } finally {
      setDetailBusy(false);
    }
  }

  // ---- detail actions --------------------------------------------
  const [detailBusy, setDetailBusy] = createSignal(false);
  const [detailError, setDetailError] = createLiveText();
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
      setDetailError(
        contactsErrorText(
          err,
          "contacts.error.start_chat",
          memberNameOf(profiles.resolve(c.signaturePubkeyB64)),
        ),
      );
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
          clearSelection();
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
                  {t("nav.contacts")}
                </button>
                <Show when={adding() === "username" || adding() === null}>
                  <div class="lk-page-head">
                    <h1>{t("contacts.add.title")}</h1>
                    <p>{t("contacts.add.subtitle")}</p>
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
                    <h1>{t("contacts.invite_links.title")}</h1>
                    <p>{t("contacts.invite_links.subtitle")}</p>
                  </div>
                  <InviteLinksPanel />
                </Show>
              </div>
            </article>
          }
        >
          {(c) => (
            <ContactDetailView
              personKeyB64={c().signaturePubkeyB64}
              localName={c().displayName}
              chatHref={chatWith(c())}
              canStartChat={c().accountIdHex !== undefined}
              busy={detailBusy}
              error={detailError}
              myPubkey={() => chats.identity()?.signaturePublicKey ?? null}
              onBack={clearSelection}
              onMessage={() => void onMessage(c())}
              onCall={(withVideo) => onCall(c(), withVideo)}
              onRename={(name) => onRenameAccepted(c(), name)}
              onRemove={() => {
                onRemoveAccepted(c());
                clearSelection();
              }}
              // The membership of this person's 1x1 chat, read off the live
              // handle the same way the conversation's header reads it. It
              // has to be here: a one-to-one's header opens THIS card rather
              // than a conversation page of its own (`conversation-info.md`
              // section 3), so an ended one-to-one that could only be deleted
              // from its header could not be deleted at all — which is how
              // web shipped, while iOS and Android rendered the block. Read
              // again whenever the selection or the registry changes; it is
              // terminal, so a stale `false` lasts only until the card is
              // next opened.
              membershipEnded={chatMembershipEnded(c())}
              onDeleteChat={
                chatMembershipEnded(c()) ? () => void onDeleteChat(c()) : null
              }
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
      <h2>{t("contacts.add.by_username")}</h2>
      <form onSubmit={props.onSubmit} class="lk-stack" style={{ gap: "12px" }}>
        <label>
          {t("contacts.add.username_label")}
          <input
            type="text"
            placeholder={t("contacts.add.username_placeholder")}
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
          {t("contacts.add.intro_label")}
          <textarea
            rows={2}
            placeholder={t("contacts.add.intro_placeholder")}
            value={props.introInput()}
            onInput={(e) => props.setIntroInput(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        <Show when={props.error()}>
          {(m) => (
            <div class="form-error">
              {m()}
            </div>
          )}
        </Show>
        <Show when={props.notice()}>{(m) => <div class="form-info">{m()}</div>}</Show>
        <div class="lk-actions">
          <button
            class="primary"
            type="submit"
            disabled={props.busy() || !mayLookUp(props.searchCheck())}
          >
            <Icon name="send" size={18} />
            {props.busy() ? t("common.sending") : t("contacts.add.send")}
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
        <h2>{t("chat.reauth.title")}</h2>
        <p class="lk-card-desc">{t("contacts.reauth.body")}</p>
        <div class="lk-actions">
          <button class="primary" type="button" onClick={signInAgain}>
            {t("chat.reauth.action")}
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
