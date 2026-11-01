// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Contacts (docs/ux/client-redesign.md section 5.3): a two-pane screen. The
 * list pane (`contacts/ContactListPane.tsx`) holds the search field,
 * requests, sent requests and the alphabetical contacts; the main pane shows
 * the selected contact (`contacts/ContactDetailView.tsx`) or what the add
 * menu opened: the invite links (`contacts/InviteLinksPanel.tsx`). A narrow
 * screen shows one pane at a time.
 *
 * Finding someone new is the search field itself (`search/PeopleSearch.tsx`,
 * `docs/ux/who-can-find-me.md` section 6.1): it asks the network as the
 * person types and Add contact on a result sends the request at once. There
 * is no "Add by username" form any more.
 *
 * This file owns the state and every mutation:
 *
 *   - the **Sent** list: requests sent from search, remembered until the
 *     acceptance lands;
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
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { useLineage } from "../chat/use-lineage";
import { deleteChatConfirmation } from "../chat/delete-chat-confirm";
import {
  loadLeaveRequestedAt,
  membershipEnded as membershipHasEnded,
  membershipState,
  readIsActive,
} from "../chat/membership-ended";
import { ContactsClient } from "../contacts/client";
import type { IncomingContactRequest } from "../contacts/client";
import { ContactDetailView } from "../contacts/ContactDetailView";
import { contactBackTarget, coversOrigin } from "../chat/info-navigation";
import { ContactListPane, type AddMode } from "../contacts/ContactListPane";
import { waitingSentRequests, type SentRequestRecord } from "../contacts/contact-list-model";
import { contactsErrorText } from "../contacts/errors";
import { InviteLinksPanel } from "../contacts/InviteLinksPanel";
import {
  loadAccepted,
  restoreRemovedContact,
  removeAccepted,
  subscribeAcceptedChanged,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { useMailbox } from "../contacts/MailboxContext";
import { forgetSentRequest, loadSentRequests, sentRequestsVersion } from "../contacts/sent-requests";
import { forgetSentIntro } from "../contacts/sent-request-intros";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { onContactRemoved } from "../reach/reach-service";
import { forgetChosenContact } from "../settings/visibility-actions";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { t } from "../l10n";
import { conversationWithContact } from "../contacts/contact-conversation";
import { conversationKindOf } from "../chat/conversation-kind";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { foundOneToOneWith } from "../contacts/found-one-to-one";
import { acceptContactRequest, acceptKeepsRequest } from "../contacts/accept-request";
import {
  loadPendingContacts,
  pendingContacts,
  shownPendingContacts,
  type PendingContact,
} from "../contacts/pending-contacts";
import { createLiveText } from "../l10n/live-text";
import { paths } from "./paths";
import { screenAnchor } from "../ui/screen-anchor";
import { PeopleSearch } from "../search/PeopleSearch";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";

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
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
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
  const membershipEnd = useMembershipEnd();
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
  const sent = createMemo<SentRequestRecord[]>(() => {
    sentRequestsVersion();
    const account = accountIdB64();
    if (account === null) return [];
    const acceptedHexes = new Set<string>();
    const acceptedKeys = new Set<string>();
    for (const c of accepted()) {
      acceptedKeys.add(c.signaturePubkeyB64);
      if (c.accountIdHex !== undefined) acceptedHexes.add(c.accountIdHex);
    }
    return waitingSentRequests(loadSentRequests(account), acceptedHexes, acceptedKeys);
  });
  function onDismissSent(record: SentRequestRecord): void {
    const account = accountIdB64();
    if (account === null) return;
    forgetSentRequest(account, record.recipientAccountIdHex);
    void forgetSentIntro(account, record.recipientAccountIdHex);
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
  ): Promise<string> {
    const founded = await foundOneToOneWith({
      client: props.client,
      identity: requireIdentity(),
      chats,
      profiles,
      contact,
      peerAccountId,
      peerPubkey,
    });
    setAccepted(founded.accepted);
    return paths.chat(base64UrlEncodeNoPad(founded.conversation.group.groupId));
  }

  /** The requests an Accept is running for, and those whose last Accept
   *  failed: they stay listed, with a line, and Accept works again. */
  const [accepting, setAccepting] = createSignal<ReadonlySet<number>>(new Set());
  const [acceptFailed, setAcceptFailed] = createSignal<ReadonlySet<number>>(new Set());
  const withId = (ids: ReadonlySet<number>, id: number): ReadonlySet<number> => new Set(ids).add(id);
  const withoutId = (ids: ReadonlySet<number>, id: number): ReadonlySet<number> => {
    const next = new Set(ids);
    next.delete(id);
    return next;
  };

  async function onAccept(req: IncomingContactRequest): Promise<void> {
    const account = accountIdB64();
    if (account === null || accepting().has(req.messageId)) return;
    setAcceptError(null);
    setAccepting((ids) => withId(ids, req.messageId));
    setAcceptFailed((ids) => withoutId(ids, req.messageId));
    try {
      // The one way a person's Accept runs (`contacts/accept-request.ts`).
      const outcome = await acceptContactRequest(
        {
          client: props.client,
          identity: requireIdentity(),
          accountIdB64: account,
          chats: {
            add: (g) => chats.add(g),
            persistHeld: () => chats.persistSnapshotDurable(account),
          },
          profiles,
        },
        req,
      );
      setAccepted(outcome.accepted);
      if (acceptKeepsRequest(outcome)) {
        setAcceptFailed((ids) => withId(ids, req.messageId));
        return;
      }
      mailbox.removePending(req.messageId);
      if (outcome.status === "legacy") setAcceptError(() => t("contacts.accept.error.old_version"));
    } finally {
      setAccepting((ids) => withoutId(ids, req.messageId));
    }
  }

  // ---- ADR-0079: inviters added through a link, not answered yet ---
  createEffect(() => {
    const account = accountIdB64();
    if (account !== null) void loadPendingContacts(account);
  });
  /** The pending contacts the list shows: those with no one-to-one yet. */
  const pendingShown = createMemo((): PendingContact[] => {
    const own = ownPersonKeyB64(chats.identity());
    const peers = new Set<string>();
    for (const g of chats.groups()) {
      if (conversationKindOf(g.group) !== "chat") continue;
      for (const k of personKeysOf(g.group)) if (k !== own) peers.add(k);
    }
    return shownPendingContacts(pendingContacts(), peers);
  });

  function onDecline(req: IncomingContactRequest): void {
    mailbox.removePending(req.messageId);
  }

  // ---- rename / remove ------------------------------------------
  function onRemoveAccepted(contact: AcceptedContact): void {
    setAccepted(removeAccepted(contact.signaturePubkeyB64));
    // ADR-0060 section 1 — a removed contact keeps the `contact` key we handed
    // them; rotating it (and the `fof` key) makes it stop working at once.
    void onContactRemoved(base64Decode(contact.signaturePubkeyB64));
    // Adding them back brings them in switched off (who-can-find-me 4.1).
    if (contact.accountIdHex !== undefined) {
      void forgetChosenContact(contact.accountIdHex).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[privacy] a removed contact stayed in the chosen list", err);
      });
    }
  }
  /** ADR-0048 §5 — the viewer's LOCAL RENAME of a contact. A blank value
   *  clears it, so the contact's own name shows again. Re-upserts by the
   *  same signature pubkey, so the link + account_id + group id are kept. */
  function onRenameAccepted(contact: AcceptedContact, newName: string): void {
    setAccepted(upsertAccepted({ ...contact, displayName: newName.trim() }));
  }

  // ---- layout: the list pane + the detail / add pane ----------------
  const [selectedKey, setSelectedKey] = createSignal<string | null>(null);
  // The add menu's pick: the invite links.
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
  /**
   * `docs/ux/conversation-info.md` section 3.1 — where the contact screen's
   * back icon returns: the screen that opened it and that it covers (a
   * conversation, a group's info, a search in Chats), recorded in the history
   * entry's state (`chat/info-navigation.ts`). Only for the person that entry
   * names: once another contact is picked from the list beside it, nothing is
   * covered any more.
   */
  const backToOrigin = createMemo((): (() => void) | undefined => {
    const origin = contactBackTarget(location.state);
    if (origin === null || !coversOrigin(origin)) return undefined;
    const slug = params.key;
    if (slug === undefined) return undefined;
    let named: string;
    try {
      named = base64Encode(base64UrlDecodeNoPad(slug));
    } catch {
      return undefined;
    }
    if (selectedKey() !== named) return undefined;
    // The entry was pushed from the origin, so going back through it returns
    // there with the origin's own history intact.
    return () => navigate(-1);
  });
  /** Deselect, and stop the URL naming a person who is no longer shown. */
  function clearSelection(): void {
    setSelectedKey(null);
    if (params.key !== undefined) navigate(paths.contacts, { replace: true });
  }
  const [adding, setAdding] = createSignal<AddMode | null>(null);

  const [filter, setFilter] = createSignal("");
  /** The people in this device's groups (base64 account keys): what the
   *  people half of the search field matches locally beside the contacts. */
  const groupPeopleKeys = createMemo((): readonly string[] => {
    const out = new Set<string>();
    for (const g of chats.groups()) for (const k of personKeysOf(g.group)) out.add(k);
    return [...out];
  });
  const selectedContact = (): AcceptedContact | undefined =>
    accepted().find((c) => c.signaturePubkeyB64 === selectedKey()) ??
    // ADR-0073 R.2 — a contact re-keyed by an accepted reset while its page
    // is open stays the selected one: found by the key it had.
    accepted().find((c) => {
      const key = selectedKey();
      return key !== null && (c.previousSignaturePubkeysB64 ?? []).includes(key);
    });
  /**
   * A person the viewer removed from their contacts whose one-to-one remains
   * (`docs/ux/conversation-info.md` section 2.1, amended 2026-10-24): the
   * conversation's header still opens their card, which offers `Add to
   * contacts`. Read off the live conversation, as the one-to-one rule reads
   * it; never listed.
   */
  const removedPeer = (): AcceptedContact | undefined => {
    const key = selectedKey();
    if (key === null) return undefined;
    const own = ownPersonKeyB64(chats.identity());
    const g = chats
      .groups()
      .find(
        (c) =>
          conversationKindOf(c.group) === "chat" &&
          personKeysOf(c.group).includes(key) &&
          key !== own,
      );
    if (g === undefined) return undefined;
    const accountIdHex = profiles.accountIdHexOf(key);
    return {
      signaturePubkeyB64: key,
      displayName: "",
      acceptedAtMs: 0,
      groupIdB64: g.groupIdB64,
      ...(accountIdHex !== null && accountIdHex !== undefined && accountIdHex.length > 0
        ? { accountIdHex }
        : {}),
    };
  };
  const selected = (): AcceptedContact | undefined => selectedContact() ?? removedPeer();
  const selectedIsContact = (): boolean => selectedContact() !== undefined;
  /** `Add to contacts`: back on the list with the conversation they have. */
  function onAddBack(c: AcceptedContact): void {
    if (c.groupIdB64 === undefined) return;
    restoreRemovedContact({
      signatureKey: c.signaturePubkeyB64,
      accountIdHex: c.accountIdHex ?? null,
      groupIdB64: c.groupIdB64,
      nowMs: Date.now(),
    });
    setAccepted(loadAccepted());
  }

  /** The registry row of the one-to-one with this contact, if this device
   *  holds one. The rule is `contacts/contact-conversation.ts`, shared with
   *  the call screen's `Invite contacts`. */
  const groupWith = (c: AcceptedContact): ActiveGroup | undefined =>
    conversationWithContact({
      contact: c,
      conversations: chats.groups(),
      ownKeyB64: ownPersonKeyB64(chats.identity()),
      peopleOf: (g) => personKeysOf(g.group),
      isOneToOne: (g) => conversationKindOf(g.group) === "chat",
    });

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
    const stillOn = beginDetailAction();
    try {
      // ADR-0073 R.5 — every group of the conversation, the head last.
      const view = lineageView.resolve(g.groupIdB64);
      const held = view?.held ?? [g];
      await membershipEnd.deleteConversation({
        segments: held.map((segment) => ({
          groupIdB64: segment.groupIdB64,
          groupIdHex: syncGroupIdHex(segment.group.groupId),
        })),
        headGroupIdB64: view?.head.groupIdB64 ?? g.groupIdB64,
        conversationHex: view?.conversationHex ?? syncGroupIdHex(g.group.groupId),
      });
      // `clearSelection` navigates and deselects; only for a person still on
      // THIS contact.
      if (stillOn()) clearSelection();
    } catch (err) {
      if (stillOn()) setDetailError(contactsErrorText(err, "chat.error.delete_chat", chatLabelOf(c)));
    } finally {
      if (stillOn()) setDetailBusy(false);
    }
  }

  // ---- detail actions --------------------------------------------
  const [detailBusy, setDetailBusy] = createSignal(false);
  const [detailError, setDetailError] = createLiveText();
  /**
   * The busy flag and the error line are ONE pair for the whole pane, and the
   * pane shows whichever contact is selected: picking another contact from
   * the list changes neither the component nor the URL. So an action belongs
   * to the contact it was started on. `beginDetailAction` marks the pane
   * busy and returns "is that contact still the one shown, and has no later
   * action replaced this one"; an action that is no longer current reports
   * nothing - its late failure is not shown under another person's name, and
   * it never deselects the person now open. What it did (the chat deleted or
   * created) stands either way.
   */
  let detailAction = 0;
  function beginDetailAction(): () => boolean {
    const mine = ++detailAction;
    const forKey = selectedKey();
    const here = screenAnchor(location);
    setDetailBusy(true);
    return () => mine === detailAction && selectedKey() === forKey && here();
  }
  createEffect(
    on(
      selectedKey,
      () => {
        // Another contact: the pane starts clean, whatever is still running
        // for the previous one.
        detailAction += 1;
        setDetailBusy(false);
        setDetailError(null);
      },
      { defer: true },
    ),
  );

  /** Message: open the chat, or create it first when this device has none
   *  and the contact's account id is known. */
  async function onMessage(c: AcceptedContact): Promise<void> {
    const existing = chatWith(c);
    if (existing !== null) {
      navigate(existing);
      return;
    }
    if (c.accountIdHex === undefined) return;
    setDetailError(null);
    // The chat is created and recorded whether or not the person waits; only
    // opening it is addressed to this screen and this contact.
    const stillOn = beginDetailAction();
    try {
      const href = await foundChatWith(
        c,
        hexToBytes(c.accountIdHex),
        base64Decode(c.signaturePubkeyB64),
      );
      if (stillOn()) navigate(href);
    } catch (err) {
      if (stillOn()) {
        setDetailError(
          contactsErrorText(
            err,
            "contacts.error.start_chat",
            memberNameOf(profiles.resolve(c.signaturePubkeyB64)),
          ),
        );
      }
    } finally {
      if (stillOn()) setDetailBusy(false);
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
        people={(listedKeysB64, anyListRow) => (
          // `docs/ux/who-can-find-me.md` section 6.1 - the same field as in
          // Chats; here it matches only people.
          <PeopleSearch
            scope="contacts"
            query={filter}
            groupPeopleKeys={groupPeopleKeys}
            listedB64={listedKeysB64}
            noMatchLine={() => (anyListRow() ? t("contacts.list.no_match") : null)}
          />
        )}
        onAdd={(mode) => {
          clearSelection();
          setAdding(mode);
        }}
        pending={mailbox.pending}
        pendingBusy={mailbox.busy}
        error={() => acceptError() ?? mailbox.error()}
        onAccept={(req) => void onAccept(req)}
        accepting={(req) => accepting().has(req.messageId)}
        acceptFailed={(req) => acceptFailed().has(req.messageId)}
        onDecline={onDecline}
        sent={sent}
        onDismissSent={onDismissSent}
        invited={pendingShown}
        onOpenInvited={(c) => navigate(paths.pendingChat(c.accountIdHex))}
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
            // Nothing selected and nothing opened from the add menu: an
            // empty main pane (the narrow layout shows the list alone).
            <Show when={adding() === "invite"} fallback={<div class="lk-main-center" />}>
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
                  <div class="lk-page-head">
                    <h1>{t("contacts.invite_links.title")}</h1>
                    <p>{t("contacts.invite_links.subtitle")}</p>
                  </div>
                  <InviteLinksPanel />
                </div>
              </article>
            </Show>
          }
        >
          {(c) => (
            <ContactDetailView
              personKeyB64={c().signaturePubkeyB64}
              localName={c().displayName}
              chatHref={chatWith(c())}
              canStartChat={c().accountIdHex !== undefined}
              acceptedAtMs={c().acceptedAtMs}
              busy={detailBusy}
              error={detailError}
              myPubkey={() => chats.identity()?.signaturePublicKey ?? null}
              onBack={clearSelection}
              onBackToOrigin={backToOrigin()}
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
              isContact={selectedIsContact()}
              accountIdHex={c().accountIdHex ?? null}
              onAddContact={() => onAddBack(c())}
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
