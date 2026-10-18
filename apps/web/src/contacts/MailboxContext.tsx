// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The identity mailbox, received automatically for the whole signed-in
 * session.
 *
 * The mailbox is where everything addressed to this account rather than to a
 * group arrives: contact requests, contact acks, and the MLS Welcomes that
 * turn somebody else's "start a group with you" into a chat on this device.
 * It used to be fetched only when the user pressed Refresh on the Contacts
 * route, and the dispatch lived inside that route component — so a Welcome
 * sent to a user who was reading a chat, or who never opened Contacts, sat
 * on the DS indefinitely.
 *
 * This provider owns both halves now:
 *
 *   * a long-poll loop (`ds/long-poll-loop.ts`) that runs while a session
 *     with identity secrets exists, paused while the tab is hidden (the web
 *     push service worker covers the background, as the native clients' push
 *     does), stopped on sign-out;
 *   * the dispatch — requests into the shared pending list the Contacts route
 *     renders, acks into the local address book, Welcomes into joined groups —
 *     behind `mailbox-poller.ts`, which is what keeps the loop and the Refresh
 *     button from dispatching the same batch twice.
 *
 * The account cursor and the pending requests are persisted together per
 * account by `inbox-store.ts` (raw rows, re-decoded on load), so a request
 * the cursor has already passed survives a reload until Accept or Decline.
 *
 * After every Welcome attempt, joined or not, the account's KeyPackage is
 * republished: processing a Welcome consumes the KeyPackage it was built on,
 * and a failed attempt may have consumed it too. See
 * `chat/keypackage-publisher.ts`.
 */
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  Show,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { useGroupNames } from "../chat/GroupNameContext";
import { groupIdHex } from "../devices/sync/state";
import { isDsNetworkFailure } from "../ds/client";
import { journalHex } from "../ds/access";
import { installAddressRecheck } from "../reach/mailbox-write";
import { reachSession, reachSessionSignal } from "../reach/reach-service";
import { DiscoveryClient } from "../discovery/client";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { waitingSentRequests } from "./contact-list-model";
import { forgetInviteLinkSecret } from "./invite-link-secrets";
import { listMintedLinks } from "./minted-links";
import {
  deleteLedgerRowOfRecord,
  owedRowDeletions,
  revokeLinksRevokedElsewhere,
  revokeMintedLink,
  setOwedRowDeletion,
  type OwnLinkRevokeOutcome,
} from "./own-links";
import { forgetSentRequest, loadSentRequests, recordSentRequest } from "./sent-requests";
import { forgetSentIntro, sentIntroFor } from "./sent-request-intros";
import {
  installSpentRunners,
  requestsToResend,
  spentFailureOutcome,
  localLinkSpent,
  readSpendOwed,
  runOwedSpendRound,
  type SpentOutcome,
} from "./rotation-spent";
import type { Group as WasmGroupHandle, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import {
  createWakeLatch,
  LONG_POLL_WAIT_SECS,
  runLongPollLoop,
  sleepCancellable,
} from "../ds/long-poll-loop";
import { base64Decode, base64Encode } from "../auth/account-id";
import {
  ContactsClient,
  ContactsError,
  SupersessionRefused,
  type IncomingContactRequest,
  type MailboxRow,
  type IncomingMailboxEvent,
} from "./client";
import { useProfiles } from "../profile/ProfileContext";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import {
  accountKeysOf,
  loadAccepted,
  readPin,
  upsertAccepted,
  type AcceptedContact,
} from "./local-store";
import {
  establishedContactOfAccount,
  knownContactRequestVerdict,
  pinKeysOf,
  type KnownRequestCheck,
} from "./known-contact-request";
import { subscribeResetAccepted } from "./reset-events";
import { readVerifiedBinding } from "./verified-binding";
import {
  decodeNumberByKey,
  predecessorCursorStorageKey,
  predecessorMailboxEvents,
  readablePredecessorKeys,
  readPredecessorAdoptedAtMs,
  stampPredecessorAdoption,
} from "./address-resolver";
import { createPendingInbox, type FetchedRequest, type PendingInbox } from "./inbox-store";
import { createMailboxPoller, type MailboxPoller } from "./mailbox-poller";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { conversationKindOf } from "../chat/conversation-kind";
import { personKeysOf } from "../chat/person-key";
import { heldSends, persistHeldSends } from "../chat/held-send-queue";
import { DIRECT_CHAT_STAND_IN } from "../chat/group-title";
import { upsertAccepted as upsertAcceptedContact, type AcceptedContact as AcceptedContactRow } from "./local-store";
import {
  handOverPendingContact,
  loadPendingContacts,
  moveHeldToConversation,
  pendingContacts,
} from "./pending-contacts";
import {
  type LinkClaimRecord as LinkClaimRecordHandle,
  type ObservedClaim,
  claimsToDecide,
  runLinkBuild,
  welcomeOrder,
  linkClaimRecord,
  linkRequestKeyHex,
  linkRequestStep,
  shownCardAnswered,
} from "./link-claims";
import { loadWasm } from "../wasm";

/** Copy for a Welcome this device could not open. */
export const JOIN_FAILURE_MESSAGE = (): string => t("contacts.mailbox.join_failed");

export interface MailboxApi {
  /** Contact requests awaiting Accept / Decline, oldest first. Kept across
   *  reloads as the raw (sealed) mailbox rows — the intro plaintext never
   *  goes to localStorage. */
  readonly pending: Accessor<readonly IncomingContactRequest[]>;
  /** A user-initiated `refresh()` is in flight. */
  readonly busy: Accessor<boolean>;
  /** The last user-initiated `refresh()` failure, or `null`. Background
   *  failures are retried with backoff and not surfaced here. */
  readonly error: Accessor<string | null>;
  /** Bumped whenever the dispatch changed the local address book (an ack
   *  landed), so a view holding a copy of it knows to reload. */
  readonly acceptedVersion: Accessor<number>;
  /** Non-null when a Welcome could not be opened, until dismissed. */
  readonly joinFailure: Accessor<string | null>;
  dismissJoinFailure(): void;
  /** Fetch the mailbox now (no long poll). */
  refresh(): Promise<void>;
  /** Drop a request from the pending list for good (after Accept or
   *  Decline). */
  removePending(messageId: number): void;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function concatBytes(parts: readonly Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** The claim decision's flat inputs: ids, and the authors' leaf keys
 *  concatenated in the same order. */
function claimInputs(claims: readonly ObservedClaim[]): { ids: BigUint64Array; authors: Uint8Array } {
  return {
    ids: BigUint64Array.from(claims.map((c) => BigInt(c.id))),
    authors: concatBytes(claims.map((c) => hexToBytes(c.author))),
  };
}

function readPredecessorMailboxCursor(accountIdB64: string, keyHex: string): number {
  try {
    const raw = globalThis.localStorage?.getItem(predecessorCursorStorageKey(accountIdB64)) ?? null;
    return decodeNumberByKey(raw)[keyHex] ?? 0;
  } catch {
    return 0;
  }
}

function writePredecessorMailboxCursor(accountIdB64: string, keyHex: string, cursor: number): void {
  // A failed write throws, which holds the cursor: the rows come back.
  const storage = globalThis.localStorage;
  if (storage === undefined) return;
  const held = decodeNumberByKey(storage.getItem(predecessorCursorStorageKey(accountIdB64)));
  held[keyHex] = cursor;
  storage.setItem(predecessorCursorStorageKey(accountIdB64), JSON.stringify(held));
}

/** The own predecessor keys whose mailbox is still read (ADR-0074 section
 *  3.5a rule 4), each inside its OWN 30-day window. A key with no time
 *  stored (this device adopted before the time was kept) starts its window
 *  now. */
function predecessorKeysStillRead(accountIdB64: string, keysHex: readonly string[]): string[] {
  const now = Date.now();
  const readable = readablePredecessorKeys(
    keysHex,
    (keyHex) => readPredecessorAdoptedAtMs(accountIdB64, keyHex),
    now,
  );
  for (const keyHex of readable) stampPredecessorAdoption(accountIdB64, keyHex, now);
  return readable;
}

const MailboxContext = createContext<MailboxApi>();

export function MailboxProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const sync = useDeviceGroupSync();
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    // ADR-0050: requests, acks and the account mailbox are the person's;
    // groups are this device's (ADR-0078: there is no device mailbox).
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  // ADR-0074 section 3.5a rule 6 - every write to a person's mailbox that is
  // refused re-checks that person's address once, through this client's
  // directory fetch and fail-closed gate.
  installAddressRecheck((accountKey) => client.recheckAddress(accountKey));
  onCleanup(() => installAddressRecheck(null));

  // ADR-0074 section 3.5a rule 5 - what was granted before a rotation of
  // this account's keys is spent. The two owed steps of the rotation record
  // (`devices/key-rotation.ts`: `links`, `requests`) run here, where the
  // directory client and the contacts client are.
  const spentDiscovery = new DiscoveryClient();
  installSpentRunners({
    links: async (rotatedAtMs) => {
      const secrets = auth.identitySecrets();
      if (secrets === null) return null;
      const accountIdB64 = base64Encode(secrets.accountId);
      // ADR-0078 section 10: no ledger row names its owner, so there is no
      // owner list to walk. Every device revokes the links IT minted before
      // the keys changed, each with the link's own revocation seed from its
      // record; a link with no record here is another device's to revoke (or
      // lives until it expires).
      const outcomes: SpentOutcome[] = [];
      for (const link of await listMintedLinks(accountIdB64)) {
        if (!localLinkSpent(link.mintedAtMs, rotatedAtMs)) continue;
        let outcome: OwnLinkRevokeOutcome;
        try {
          outcome = await revokeMintedLink(spentDiscovery, accountIdB64, link);
        } catch (err) {
          outcomes.push(spentFailureOutcome(err));
          continue;
        }
        outcomes.push(outcome === "unreachable" ? "unreachable" : "done");
        if (outcome !== "unreachable" && link.kind === "contact") {
          forgetInviteLinkSecret(base64UrlEncodeNoPad(secrets.accountId), link.blobIdSlug);
        }
      }
      return outcomes;
    },
    requests: async (rotatedAtMs) => {
      const secrets = auth.identitySecrets();
      if (secrets === null || chats.accountIdentity() === null) return null;
      // A reply grant is registered on the mailbox of the session's key: the
      // reach session of the NEW key must be the one running.
      const session = reachSession();
      const own = chats.accountIdentity();
      if (session === null || own === null) return null;
      if (session.accountKeyHex !== journalHex(own.signaturePublicKey)) return null;
      const accountIdB64 = base64Encode(secrets.accountId);
      const accepted = new Set(
        loadAccepted(accountIdB64)
          .map((c) => c.accountIdHex)
          .filter((hex): hex is string => hex !== undefined),
      );
      const waiting = waitingSentRequests(loadSentRequests(accountIdB64), accepted);
      const outcomes: SpentOutcome[] = [];
      for (const record of requestsToResend(waiting, rotatedAtMs)) {
        let outcome: SpentOutcome;
        try {
          outcome = await client.resendContactRequestAfterRotation(
            secrets,
            hexToBytes(record.recipientAccountIdHex),
            // The words the person wrote, kept under the device key.
            new TextEncoder().encode(await sentIntroFor(accountIdB64, record.recipientAccountIdHex)),
          );
        } catch (err) {
          outcome = spentFailureOutcome(err);
        }
        outcomes.push(outcome);
        // The stored entry is replaced: it now stands for the new request.
        if (outcome === "done") recordSentRequest(accountIdB64, { ...record, sentAtMs: Date.now() });
      }
      return outcomes;
    },
  });
  onCleanup(() => installSpentRunners(null));

  // The spend round on a KEPT device (rule 5): a device that adopted keys
  // rotated elsewhere owes the two steps over its own links and requests.
  // Tried at each session start, once the reach session of the new key
  // runs; what is still owed stays in the marker for the next start.
  createEffect(() => {
    const session = reachSessionSignal();
    const own = chats.accountIdentity();
    const secrets = auth.identitySecrets();
    if (session === null || own === null || secrets === null) return;
    const accountIdB64 = base64Encode(secrets.accountId);
    if (session.accountKeyHex !== journalHex(own.signaturePublicKey)) return;
    if (readSpendOwed(accountIdB64) === null) return;
    void runOwedSpendRound(accountIdB64);
  });

  const [pending, setPending] = createSignal<readonly IncomingContactRequest[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();
  const [acceptedVersion, setAcceptedVersion] = createSignal(0);
  const [joinFailure, setJoinFailure] = createLiveText();

  // ADR-0079 section 4 (reworked 2026-10-17) - the device that revokes a
  // link deletes its ledger row itself with the seed the synced record
  // carries (`InviteLinksPanel`). Re-run on every change of the register:
  // a deletion this device owes (the ledger was unreachable) is tried again,
  // and the minting device drops its own local record of a link revoked
  // elsewhere (deleting the row again, which the ledger answers as gone).
  createEffect(() => {
    const s = auth.state();
    if (s.kind !== "session") return;
    const account = s.account.accountId;
    const revokedRecords = sync.linkRecords().filter((r) => r.revoked);
    if (revokedRecords.length === 0) return;
    const owed = owedRowDeletions(account);
    void (async () => {
      for (const r of revokedRecords) {
        if (!owed.has(r.linkIdHex)) continue;
        try {
          if ((await deleteLedgerRowOfRecord(spentDiscovery, r)) !== "unreachable") {
            setOwedRowDeletion(account, r.linkIdHex, false);
          }
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("owed ledger row deletion failed", err instanceof Error ? err.name : "");
        }
      }
      await revokeLinksRevokedElsewhere(
        spentDiscovery,
        account,
        new Set(revokedRecords.map((r) => r.linkIdHex)),
      );
    })();
  });

  // ADR-0079 section 5 - a pending contact whose one-to-one already exists
  // on this device (its Welcome was joined before the pending record was
  // read, or on another tab): hand its messages over now. Re-run whenever
  // the pending list or the conversations change; a contact with nothing to
  // hand over is a no-op.
  createEffect(() => {
    const s = auth.state();
    if (s.kind !== "session") return;
    const account = s.account.accountId;
    void loadPendingContacts(account);
    const waiting = pendingContacts();
    if (waiting.length === 0) return;
    const keys = new Set(waiting.map((p) => p.signaturePubkeyB64));
    for (const g of chats.groups()) {
      if (conversationKindOf(g.group) !== "chat") continue;
      if (!personKeysOf(g.group).some((k) => keys.has(k))) continue;
      void handOverToConversation(account, g.groupIdB64);
    }
  });

  // The signed-in account's mailbox cursor + pending requests, persisted
  // together (`inbox-store.ts`). `null` while signed out.
  let inbox: PendingInbox | null = null;
  /** What a poll is started for: the account and its inbox, replaced as one
   *  value on every `start` / `stop`. A poll captures it and everything it
   *  does afterwards goes through the captured one (`mailbox-poller.ts`). */
  interface MailboxScope {
    readonly accountIdB64: string;
    readonly inbox: PendingInbox;
  }
  let scope: MailboxScope | null = null;
  // Welcome rows already joined in this page lifetime. A batch whose cursor
  // write was HELD (the post-join persist failed) is fetched again; without
  // this, the second pass would try to open a Welcome whose KeyPackage the
  // first pass consumed, and report a failure for a group that did join.
  const joinedWelcomeIds = new Set<number>();

  let activeAccount: string | null = null;
  let loopAbort: AbortController | null = null;
  const resume = createWakeLatch();

  /** Requests from existing contacts with no verdict yet (UX 16.6: "the
   *  request waits, unshown, for the next check"), with the raw row each
   *  came in. The mailbox cursor has moved past them, so the rows are also
   *  kept by the inbox store (`holdWaiting`) and decoded again after a
   *  reload: a request whose verdict was inconclusive is not lost with the
   *  page. Bounded by the store. */
  const waitingKnownRequests: { request: IncomingContactRequest; row: MailboxRow }[] = [];

  /**
   * Returns `true` when `request` came from an account that is already a
   * contact and was dealt with here (answered, dropped or kept waiting):
   * the caller then lists nothing. `false` for a stranger's request.
   */
  async function handleKnownContactRequest(
    request: IncomingContactRequest,
    accountIdB64: string,
    row: MailboxRow,
  ): Promise<boolean> {
    const fromAccountId = request.fromAccountId;
    if (fromAccountId === null) return false;
    // "Already a contact" is the pin or a verified binding, read BEFORE the
    // check below (which may write a first pin): a row whose account id came
    // only from the peer's own profile establishes nothing, so a member who
    // claimed this account's id cannot make its request vanish.
    const fromAccountIdB64 = base64Encode(fromAccountId);
    const known = establishedContactOfAccount(loadAccepted(accountIdB64), groupIdHex(fromAccountId), {
      pinKeysB64: pinKeysOf(accountKeysOf(fromAccountIdB64, accountIdB64)),
      bindingKeyB64: readVerifiedBinding(fromAccountIdB64, accountIdB64)?.keyB64 ?? null,
    });
    if (known === undefined) return false;
    let check: KnownRequestCheck;
    try {
      const outcome = await client.checkContactIdentity(fromAccountId);
      check = outcome === "current" ? "current" : "inconclusive";
    } catch (err) {
      check = err instanceof SupersessionRefused ? "refused" : "inconclusive";
    }
    const verdict = knownContactRequestVerdict({
      senderIsContact: true,
      check,
      requestKeyB64: base64Encode(request.fromPubkey),
      trustedKeyB64: readPin(base64Encode(fromAccountId), accountIdB64)?.pinnedSignaturePubkeyB64 ?? null,
    });
    if (verdict === "answer") {
      // The ack is the whole of the answer on this path: the reset the check
      // just accepted (or accepted earlier) is what moves the conversation.
      try {
        await client.sendContactAck(auth.identitySecrets(), request);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("silent ack to an existing contact failed", err instanceof Error ? err.name : "");
      }
    } else if (verdict === "wait") {
      // Kept on disk first: the cursor is about to move past this row.
      const kept = inbox?.holdWaiting(row) ?? false;
      if (kept && !waitingKnownRequests.some((w) => w.request.messageId === request.messageId)) {
        waitingKnownRequests.push({ request, row });
      }
      return true;
    }
    // Answered or dropped: it waits no longer.
    inbox?.dropWaiting(request.messageId);
    return true;
  }

  /** Judge again the requests that were waiting for a verdict. A sender who
   *  is no longer a contact by then is an ordinary request and is listed. */
  async function retryWaitingKnownRequests(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session" || waitingKnownRequests.length === 0) return;
    const again = waitingKnownRequests.splice(0, waitingKnownRequests.length);
    for (const w of again) {
      if (await handleKnownContactRequest(w.request, s.account.accountId, w.row)) continue;
      inbox?.dropWaiting(w.request.messageId);
      try {
        inbox?.addFetched([{ request: w.request, row: w.row }]);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: a waiting request could not be listed", err);
      }
    }
  }

  /** After a reload: the waiting rows kept on disk, decoded through the
   *  poller's own verify-and-open path and judged again. */
  async function restoreWaitingKnownRequests(target: PendingInbox): Promise<void> {
    const rows = target.waitingRows();
    if (rows.length === 0) return;
    const decoded = await client.decodePendingRequestRows(auth.identitySecrets(), rows);
    if (inbox !== target) return;
    rows.forEach((row, i) => {
      const request = decoded[i] ?? null;
      if (request === null) {
        target.dropWaiting(row.messageId);
        return;
      }
      if (!waitingKnownRequests.some((w) => w.request.messageId === request.messageId)) {
        waitingKnownRequests.push({ request, row });
      }
    });
    await retryWaitingKnownRequests();
  }
  const unsubscribeResetAccepted = subscribeResetAccepted(() => {
    void retryWaitingKnownRequests();
  });
  onCleanup(unsubscribeResetAccepted);

  /**
   * ADR-0079 section 3 - the winning device's accept of a request sent
   * through a live invite link: the ordinary accept path, then one use of
   * the link counted in the synced register (with the requester's key, so
   * another device of the account treats the same request as answered).
   * `false` when the accept did not complete; the won claim stays recorded
   * and the next pass tries again.
   */
  /** Groups founded for a link request in this page and not yet held. */
  const foundedLinkGroups = new Map<string, WasmGroupHandle>();

  /**
   * The winning device's accept of a request sent through one of this
   * account's live invite links (ADR-0079 section 3, order of 2026-10-17,
   * `runLinkBuild`): its own founding carried on; else a link-founded note
   * from another device answered by writing the note's Welcome to the
   * requester (never founding); else found in ONE commit with every other
   * device of this account, record the founding, publish the note, and only
   * then write the Welcome to this account's own mailbox and then the
   * requester's. `false` when it did not complete; every record stays, and
   * the next pass takes it up where it stopped.
   */
  async function acceptThroughLink(
    request: IncomingContactRequest,
    key: string,
    claims: LinkClaimRecordHandle,
    accountIdB64: string,
  ): Promise<boolean> {
    const identity = auth.identitySecrets();
    const answer = request.linkAnswer;
    const fromAccountId = request.fromAccountId;
    if (identity === null || answer === null || fromAccountId === null) return false;
    const keyB64 = base64Encode(request.fromPubkey);
    const contact: AcceptedContactRow = {
      signaturePubkeyB64: keyB64,
      displayName: loadAccepted(accountIdB64).find((c) => c.signaturePubkeyB64 === keyB64)?.displayName ?? "",
      acceptedAtMs: Date.now(),
      accountIdHex: groupIdHex(fromAccountId),
    };
    try {
      return await runLinkBuild({
        founded: () => claims.own(key)?.founded ?? null,
        note: () => claims.note(key),
        prepare: async () => {
          const prepared = await client.prepareGroupWith(identity, fromAccountId, "chat", request.fromPubkey);
          const groupIdB64 = base64Encode(prepared.group.groupId);
          foundedLinkGroups.set(groupIdB64, prepared.group);
          return {
            groupIdB64,
            welcomeB64: base64Encode(prepared.welcome),
            targets: welcomeOrder(
              prepared.targets.map((t) => ({ accountKeyB64: base64Encode(t.accountKey), isRecipient: t.isRecipient })),
            ),
            notePosted: false,
            delivered: false,
          };
        },
        record: (founded) => claims.setFounded(key, founded),
        postNote: async () => {
          // Published once (a sealed note already recorded is not published
          // again), then judged at its row: it counts only when sealed at the
          // journal's epoch there, so every device can read it.
          const own = claims.own(key);
          if (own?.noteConfirmed === true) return true;
          const founded = own?.founded ?? null;
          if (founded === null) return false;
          if (own?.noteWire == null) {
            await sync.postLinkFounded(
              answer.linkId,
              request.fromPubkey,
              base64Decode(founded.groupIdB64),
              base64Decode(founded.welcomeB64),
              (wireHex) => claims.setNoteWire(key, wireHex),
            );
          }
          if ((await sync.catchUpJournal()) !== "read") return false;
          if (claims.own(key)?.noteConfirmed === true) return true;
          // Read past it and it did not count (straddled by a commit) or no
          // row carried it: posted again at the next pass.
          claims.dropUnlandedNote(key);
          return false;
        },
        hold: async (founded) => {
          let held = chats.get(founded.groupIdB64);
          if (held === undefined) {
            let group = foundedLinkGroups.get(founded.groupIdB64);
            if (group === undefined) {
              const device = chats.identity();
              if (device === null) return null;
              const wasm = await loadWasm();
              // A restart between the founding and the delivery: the group
              // is in the MLS snapshot the creation persisted.
              const loaded = wasm.Group.load(device, base64Decode(founded.groupIdB64));
              if (loaded === undefined) return null;
              group = loaded;
            }
            foundedLinkGroups.delete(founded.groupIdB64);
            held = chats.add({ label: DIRECT_CHAT_STAND_IN, group });
            profiles.groupJoined(held.groupIdB64);
            await chats.persistSnapshotDurable(accountIdB64);
          }
          return held.groupIdB64;
        },
        deliver: async (founded) => {
          await client.deliverPreparedWelcomes(
            base64Decode(founded.groupIdB64),
            base64Decode(founded.welcomeB64),
            founded.targets.map((t) => ({ accountKey: base64Decode(t.accountKeyB64), isRecipient: t.isRecipient })),
            { replyGrantB64: request.replyGrantB64 },
          );
        },
        forward: async (note) => {
          // The founding device may have written this account's mailbox and
          // left before writing the requester's: the note's Welcome is the
          // same bytes for every added leaf, and a duplicate is ignored.
          await client.deliverPreparedWelcomes(
            base64Decode(note.groupIdB64),
            base64Decode(note.welcomeB64),
            [{ accountKey: request.fromPubkey, isRecipient: true }],
            { replyGrantB64: request.replyGrantB64 },
          );
          return chats.get(note.groupIdB64)?.groupIdB64 ?? null;
        },
        finish: async (groupIdB64) => {
          upsertAcceptedContact(groupIdB64 === null ? contact : { ...contact, groupIdB64 }, accountIdB64);
          try {
            await client.sendContactAck(identity, request);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("sendContactAck failed", err instanceof Error ? err.name : "");
          }
          await sync.recordLinkUse(answer.linkId, request.fromPubkey);
          setAcceptedVersion(acceptedVersion() + 1);
        },
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "invite-link accept did not complete; retried at the next pass",
        err instanceof Error ? err.name : "",
      );
      return false;
    }
  }

  /** One decision pass at a time (C-62 sub-invariant 5): the read of the
   *  claim record, the decision and the write of the outcome never
   *  interleave with another pass on this page. */
  let linkPass: Promise<void> = Promise.resolve();

  /**
   * ADR-0079 (reworked 2026-10-17) - decide every request this inbox holds
   * that answers one of the account's invite links ({@link linkRequestStep}):
   * no timer; a request waits, kept and unshown, until the synced state is
   * loaded, and only the device whose claim is first in the DeviceGroup
   * journal builds the one-to-one. Run after every mailbox pass, when the
   * synced state loads, and when a claim from another device arrives.
   */
  function decideLinkRequests(): Promise<void> {
    const run = async (): Promise<void> => {
      const polled = scope;
      if (polled === null) return;
      const target = polled.inbox;
      const accountIdB64 = polled.accountIdB64;
      const claims = linkClaimRecord(accountIdB64);
      const current = (): boolean => scope === polled;
      const wasm = await loadWasm();
      // Whether a device in the CURRENT roster claimed the request (a claim
      // only from a device since removed answers nothing). `false` while
      // the roster is not known.
      const answeredElsewhere = (key: string): boolean => {
        const seen = claims.seen(key);
        const roster = sync.linkClaimRoster();
        if (seen.length === 0 || roster === null) return false;
        const { ids, authors } = claimInputs(seen);
        return wasm.linkClaimAnsweredElsewhere(ids, authors, concatBytes(roster.roster));
      };
      // A card already shown (dead link here) for a request a roster device
      // claimed: answered, the card goes.
      for (const req of target.deadLinkRequests()) {
        const answer = req.linkAnswer;
        if (answer === null) continue;
        const key = linkRequestKeyHex(answer.linkId, req.fromPubkey);
        if (shownCardAnswered(claims.own(key), answeredElsewhere(key))) target.remove(req.messageId);
      }
      for (const req of target.undecidedLinkRequests()) {
        if (!current()) return;
        const answer = req.linkAnswer;
        if (answer === null) {
          target.remove(req.messageId);
          continue;
        }
        const key = linkRequestKeyHex(answer.linkId, req.fromPubkey);
        let step = linkRequestStep({
          own: claims.own(key),
          answeredElsewhere: answeredElsewhere(key),
          verdict: sync.linkAnswerVerdictNow(answer.linkId, answer.linkSecret, req.fromPubkey),
        });
        try {
          if (step === "wait") continue;
          if (step === "drop") {
            claims.forgetOwn(key);
            target.remove(req.messageId);
            continue;
          }
          if (step === "card-dead") {
            target.markDeadLink(req.messageId);
            continue;
          }
          if (step === "post-claim") {
            // The sealed claim is recorded before the publish: whether or
            // not the publish answers, the receive loop recognises its row
            // by those bytes and files its id as this device's.
            await sync.postLinkClaim(answer.linkId, req.fromPubkey, (wireHex) => claims.addOwnWire(key, wireHex));
            step = "decide";
          }
          if (step === "decide" || step === "recheck") {
            if (step === "decide") {
              // Every row up to (and past) this device's claims is applied
              // once a pass that started after them has read the journal to
              // its head. A wait that times out or finds the device offline
              // keeps the request undecided for the next pass.
              if ((await sync.catchUpJournal()) !== "read" || !current()) continue;
            }
            // A re-check reads no journal: every claim of this device is
            // known, a later claim cannot be earlier, and only the roster
            // can have changed.
            const roster = sync.linkClaimRoster();
            if (roster === null) continue;
            // A sealed claim no row carried did not land.
            claims.dropUnlanded(key);
            const { ids, authors } = claimInputs(
              claimsToDecide(claims.own(key), claims.seen(key), bytesToHex(roster.ownKey)),
            );
            const decision = wasm.linkClaimOutcome(roster.ownKey, ids, authors, concatBytes(roster.roster));
            const won = decision.won;
            const lost = decision.lost;
            const winnerId = Number(decision.messageId);
            decision.free();
            if (lost) {
              // Another device's claim is first: it answers the request.
              // Kept, unshown, until the register says it was answered; if
              // that device is removed first, a re-check hands it on.
              claims.setSettled(key);
              continue;
            }
            if (!won) {
              // No claim from the roster landed: post again next pass.
              claims.forgetOwn(key);
              continue;
            }
            claims.setWon(key, winnerId);
            step = "build";
          }
          if (step === "build") {
            if (await acceptThroughLink(req, key, claims, accountIdB64)) {
              if (!current()) return;
              target.remove(req.messageId);
              claims.forgetOwn(key);
            }
          }
        } catch (err) {
          // A publish or a write that failed: the request stays undecided
          // and the next pass picks it up where its record says.
          // eslint-disable-next-line no-console
          console.warn("link request not decided this pass", err instanceof Error ? err.name : "");
        }
      }
    };
    linkPass = linkPass.then(run, run);
    return linkPass;
  }

  // Decide the waiting link requests when the synced state loads, when a
  // claim from another device arrives, and when the link register moves.
  createEffect(() => {
    sync.ready();
    sync.linkClaimsVersion();
    sync.linkRecords();
    void untrack(() => decideLinkRequests());
  });

  /**
   * ADR-0079 section 5 - move the messages written to a pending contact into
   * the held-send queue of the one-to-one just joined, then drop the pending
   * record. Never throws.
   */
  async function handOverToConversation(accountIdB64: string, groupIdB64: string): Promise<void> {
    const active = chats.groups().find((g) => g.groupIdB64 === groupIdB64);
    if (active === undefined) return;
    const conversationHex = groupIdHex(active.group.groupId);
    try {
      await handOverPendingContact(
        accountIdB64,
        {
          oneToOne: conversationKindOf(active.group) === "chat",
          personKeysB64: personKeysOf(active.group),
        },
        async (contact) => {
          // The ONE held-send queue (ADR-0073 R.13b): what was written is
          // kept under the inviter's account id until now; moved, in order,
          // to the conversation's slot, from which the ordinary flush sends
          // it (a failed send keeps the rest for the next attempt).
          moveHeldToConversation(heldSends, contact, conversationHex, Date.now());
          await persistHeldSends(accountIdB64);
          return true;
        },
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("pending contact hand-over failed", err instanceof Error ? err.name : "");
    }
  }

  async function dispatch(
    events: readonly IncomingMailboxEvent[],
    polled: MailboxScope | null,
  ): Promise<void> {
    if (polled === null) throw new Error("mailbox dispatch without an inbox");
    // The account the batch was FETCHED for, not whoever is signed in when
    // the dispatch runs. A throw holds that account's cursor.
    const accountIdB64 = polled.accountIdB64;
    const target = polled.inbox;
    const current = (): boolean => scope === polled;
    const requireCurrent = (): void => {
      if (!current()) throw new Error("mailbox dispatch: the session it was polled for ended");
    };
    requireCurrent();
    const fresh: FetchedRequest[] = [];
    await retryWaitingKnownRequests();
    let welcomeAttempted = false;
    let joined = false;
    const joinedGroupIds: string[] = [];
    let failed = false;
    try {
      for (const ev of events) {
        if (ev.kind === "contact-request") {
          // ADR-0048 — the sender's profile, filed under the key the request
          // signature authenticates. Filed even for a request already listed:
          // the store keeps only a strictly newer one.
          if (ev.request.profile !== null) {
            profiles.ingest(base64Encode(ev.request.fromPubkey), ev.request.profile);
          }
          // ADR-0073 R.7 case 1 / UX 16.6 - a request from an account that is
          // ALREADY a contact is never a request row: the R.2 check runs on
          // the sender's account, and the request is answered silently (the
          // conversation continues by itself) or dropped.
          if (await handleKnownContactRequest(ev.request, accountIdB64, ev.row)) continue;
          // ADR-0079 section 3 (reworked 2026-10-17) - a request sent
          // through one of this account's invite links is kept undecided
          // (stored, not listed) and decided after this batch by
          // `decideLinkRequests`: accepted with no card by the one device
          // whose claim wins, answered, or a card with the dead-link note.
          if (ev.request.linkAnswer !== null) {
            fresh.push({ request: ev.request, row: ev.row, undecidedLink: true });
            continue;
          }
          // ADR-0074 section 3.5a rule 5 - one request per person: a request
          // from the proven successor of a pending request's key replaces
          // it, silently; a stale one from a superseded key is dropped.
          const known = [...target.pending(), ...fresh.map((f) => f.request)];
          const verdict = await client.pendingRequestsSupersededBy(ev.request, known);
          if (verdict.dropIncoming) continue;
          const replaced = new Set(verdict.replaces);
          for (let i = fresh.length - 1; i >= 0; i--) {
            if (replaced.has(fresh[i]?.request.messageId ?? -1)) fresh.splice(i, 1);
          }
          fresh.push({ request: ev.request, row: ev.row, replaces: verdict.replaces });
        } else if (ev.kind === "welcome") {
          if (joinedWelcomeIds.has(ev.welcome.messageId)) continue;
          welcomeAttempted = true;
          try {
            const group = await client.joinGroupFromWelcome(
              auth.identitySecrets(),
              ev.welcome.welcomeBytes,
            );
            // Signed out during the join: the chat list is not this
            // account's any more. Not added, not marked joined; the cursor
            // is held (the catch below is skipped by the rethrow).
            if (!current()) throw new StaleMailboxScope();
            // ADR-0051 — a name this device already holds for the group (a
            // re-join, a second Welcome) titles it at once; otherwise the
            // stand-in the title resolver reads as "unnamed".
            const active = chats.add({
              label:
                groupNames.nameOf(base64Encode(group.groupId)) ??
                `Joined group (Welcome ${ev.welcome.messageId})`,
              group,
            });
            joinedWelcomeIds.add(ev.welcome.messageId);
            joinedGroupIds.push(active.groupIdB64);
            joined = true;
          } catch (err) {
            if (err instanceof StaleMailboxScope) throw err;
            // ADR-0060 — every device of the account reads the one account
            // mailbox, so a Welcome here may add ANOTHER device of ours, or be
            // a channel co-publisher's (opened by that flow under the account
            // identity). Neither holds a KeyPackage of this device, and that
            // is not a failed join the user needs to hear about.
            if (!isWelcomeForAnotherLeaf(err)) failed = true;
            // eslint-disable-next-line no-console
            console.warn("joinGroupFromWelcome failed", ev.welcome.messageId, err);
          }
        } else {
          // 1.9b1: contact-ack — the recipient of our ContactRequest pressed
          // Accept. The signature is verified, so even a stale ack is a
          // legitimate accept by the holder of `accepterPubkey`.
          const accepter: AcceptedContact = {
            signaturePubkeyB64: base64Encode(ev.ack.accepterPubkey),
            x25519PubkeyB64: base64Encode(ev.ack.accepterX25519Pubkey),
            // ADR-0048 — the name comes from the resolver; a re-delivered ack
            // must not wipe a rename the user already gave this contact.
            displayName:
              loadAccepted(accountIdB64).find(
                (c) => c.signaturePubkeyB64 === base64Encode(ev.ack.accepterPubkey),
              )?.displayName ?? "",
            acceptedAtMs: Date.now(),
            // The peer's account_id (hex), kept with the contact (founding a
            // one-to-one, the backup).
            accountIdHex: groupIdHex(ev.ack.accepterAccountId),
          };
          // Durable, and keyed by the captured account: recorded even when
          // the session has ended since.
          upsertAccepted(accepter, accountIdB64);
          // Answered: the sent record and the intro kept for a re-send go
          // (durable, keyed by the captured account).
          const answeredHex = groupIdHex(ev.ack.accepterAccountId);
          forgetSentRequest(accountIdB64, answeredHex);
          void forgetSentIntro(accountIdB64, answeredHex);
          if (current()) setAcceptedVersion(acceptedVersion() + 1);
        }
      }
      // Persisted with the cursor it was fetched under; a failed write throws
      // here, which holds the cursor so the rows are fetched again.
      if (fresh.length > 0) target.addFetched(fresh);
      if (joined) {
        // `persistSnapshotDurable` writes the identity that is live NOW under
        // the id it is given: after a sign-out that pairing would be wrong.
        requireCurrent();
        // The joined group lives only in memory until a snapshot lands, and
        // the Welcome cannot be opened a second time in this page lifetime.
        // A failed persist throws, which HOLDS the mailbox cursor: after a
        // reload the durable snapshot still holds the unconsumed KeyPackage,
        // so the re-fetched Welcome can be opened then.
        await chats.persistSnapshotDurable(accountIdB64);
        requireCurrent();
        // ADR-0048 §2 — publish this device's profile into every group it just
        // joined, once the join is durable. The members already there cannot
        // learn the newcomer's name any other way.
        for (const groupIdB64 of joinedGroupIds) profiles.groupJoined(groupIdB64);
        // ADR-0079 section 5 - a one-to-one with a person this account added
        // through their link: what was written to them before their app
        // answered goes out now, in order, through the held-send queue.
        for (const groupIdB64 of joinedGroupIds) await handOverToConversation(accountIdB64, groupIdB64);
      }
      if (fresh.some((f) => f.undecidedLink === true)) void decideLinkRequests();
    } finally {
      // `stop()` cleared the banner when the session ended; a late failure of
      // the ended session must not put it back on the login screen or in the
      // next account's session.
      if (failed && current()) setJoinFailure(JOIN_FAILURE_MESSAGE);
      if (welcomeAttempted && current()) {
        void auth.republishKeyPackage().catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("KeyPackage republish after Welcome failed", err);
        });
      }
    }
  }

  const poller: MailboxPoller = createMailboxPoller<IncomingMailboxEvent, MailboxScope | null>({
    scope: () => scope,
    readCursor: (polled) => polled?.inbox.cursor() ?? 0,
    writeCursor: (next, polled) => {
      // One write for the cursor and the pending rows; a failure throws and
      // leaves the in-memory cursor where it was. Through the inbox the batch
      // was fetched for; a disposed one refuses the write, which holds it.
      if (polled === null) throw new Error("mailbox cursor write without an inbox");
      polled.inbox.advanceCursor(next);
    },
    fetch: async (after, waitSecs, signal) =>
      client.pollMailbox(auth.identitySecrets(), after, waitSecs, signal),
    dispatch,
  });

  // ADR-0074 section 3.5 (Stage A) — after a proven key rotation the
  // conversation leaves stay bound to the account key of before it, and a
  // peer that has not observed the rotation addresses the mailbox of the
  // key it knows: a PREDECESSOR's. So those mailboxes are read too, each
  // under its own identity and with its own cursor, one poll after the
  // other, never concurrently.
  //
  // Section 3.5a: own predecessors are a CHAIN (after a second rotation the
  // key held in between has a mailbox as well), each read inside its own
  // 30-day window; and a predecessor mailbox carries WELCOMES ONLY. A
  // contact request or an ack that reaches one is not acted on: its reply
  // grant and its seal name keys the holder of the old seed has too (rule
  // 5), and the sender re-sends from, or to, the current key.
  let polledPredecessor: WasmIdentity | null = null;
  const predecessorClient = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => polledPredecessor,
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });
  const predecessorPollers = new Map<string, MailboxPoller>();
  function predecessorPollerFor(keyHex: string): MailboxPoller {
    const held = predecessorPollers.get(keyHex);
    if (held !== undefined) return held;
    const made: MailboxPoller = createMailboxPoller<IncomingMailboxEvent>({
      readCursor: () =>
        activeAccount === null ? 0 : readPredecessorMailboxCursor(activeAccount, keyHex),
      writeCursor: (next) => {
        if (activeAccount === null) throw new Error("predecessor mailbox cursor write without an account");
        writePredecessorMailboxCursor(activeAccount, keyHex, next);
      },
      fetch: async (after, waitSecs, signal) =>
        predecessorClient.pollMailbox(auth.identitySecrets(), after, waitSecs, signal, {
          welcomesOnly: true,
        }),
      // Handled for the account that is current when the batch arrives;
      // `dispatch` refuses once that session has ended.
      dispatch: (events) => dispatch(predecessorMailboxEvents(events), scope),
    });
    predecessorPollers.set(keyHex, made);
    return made;
  }

  /** Read every own predecessor mailbox whose window is open, once each. */
  async function pollPredecessorMailboxes(accountIdB64: string, signal: AbortSignal): Promise<void> {
    const identities = chats.ownPredecessorIdentities();
    if (identities.length === 0) return;
    const byHex = new Map<string, WasmIdentity>();
    for (const identity of identities) byHex.set(journalHex(identity.signaturePublicKey), identity);
    for (const keyHex of predecessorKeysStillRead(accountIdB64, [...byHex.keys()])) {
      const identity = byHex.get(keyHex);
      if (identity === undefined) continue;
      polledPredecessor = identity;
      try {
        await predecessorPollerFor(keyHex).poll(0, signal);
      } catch (err) {
        // Never costs the account mailbox, or the next predecessor, its poll.
        // eslint-disable-next-line no-console
        console.warn("predecessor mailbox poll failed", err);
      } finally {
        polledPredecessor = null;
      }
    }
  }

  // ADR-0060 section 6 — one inbound mailbox per person: every Welcome is
  // addressed to the ACCOUNT mailbox. ADR-0078 removed the device mailbox:
  // nothing claims or polls one (its old cursor key `lakko.contacts.device-
  // mailbox.v1` is only wiped).

  function isHidden(): boolean {
    return globalThis.document?.visibilityState === "hidden";
  }

  function start(accountIdB64: string): void {
    if (activeAccount === accountIdB64 && loopAbort !== null) return;
    stop();
    activeAccount = accountIdB64;
    const opened = createPendingInbox({
      accountIdB64,
      isAccepted: (keyB64) =>
        loadAccepted(accountIdB64).some((c) => c.signaturePubkeyB64 === keyB64),
      onChange: () => setPending(opened.pending()),
    });
    inbox = opened;
    scope = { accountIdB64, inbox: opened };
    setPending(opened.pending());
    // Requests kept from an earlier page load: back through the poller's own
    // verify-and-open path before they are listed.
    void opened
      .restore((rows) => client.decodePendingRequestRows(auth.identitySecrets(), rows))
      // ADR-0079: a link request kept undecided before the reload is
      // decided now (or once the synced state loads).
      .then(() => decideLinkRequests())
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: restoring pending requests failed", err);
      });
    // ... and the unshown ones that were waiting for a verdict (UX 16.6).
    void restoreWaitingKnownRequests(opened).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("contacts inbox: restoring waiting requests failed", err);
    });
    const controller = new AbortController();
    loopAbort = controller;
    void runLongPollLoop(controller.signal, {
      shouldContinue: () => true,
      isPaused: isHidden,
      waitUntilResumed: (signal) => resume.wait(signal),
      tick: async (signal) => {
        // Long-poll the account mailbox (ADR-0060: the one inbound mailbox).
        // The predecessor's mailbox first, without holding the read open
        // (ADR-0074 section 3.5); then the long poll of the account's.
        // Rule 4 of section 3.5a: read only, for 30 days from the adoption,
        // then left. Nothing is registered or written there.
        await pollPredecessorMailboxes(accountIdB64, signal);
        await poller.poll(LONG_POLL_WAIT_SECS, signal);
        // ADR-0079 (reworked 2026-10-17): a claim whose journal read did not
        // finish, or a won claim whose build failed, is taken up again after
        // every pass; a pass with nothing undecided costs nothing.
        void decideLinkRequests();
        return 0;
      },
      classifyFailure: (err) => (isDsNetworkFailure(err) ? "network" : "other"),
      sleep: sleepCancellable,
      now: () => Date.now(),
      onFailure: (err, backoffMs) => {
        // eslint-disable-next-line no-console
        console.warn(`mailbox poll failed; retrying in ${backoffMs} ms`, err);
      },
    });
  }

  function stop(): void {
    waitingKnownRequests.length = 0;
    if (loopAbort !== null) {
      loopAbort.abort();
      loopAbort = null;
    }
    if (inbox !== null) {
      inbox.dispose();
      inbox = null;
    }
    scope = null;
    if (activeAccount !== null) {
      activeAccount = null;
      setPending([]);
      setError(null);
      setJoinFailure(null);
      joinedWelcomeIds.clear();
    }
  }

  createEffect(() => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    const identity = chats.identity();
    const account = s.kind === "session" ? s.account.accountId : null;
    untrack(() => {
      if (account !== null && secrets !== null && identity !== null) {
        start(account);
      } else {
        stop();
      }
    });
  });

  const onVisibility = (): void => resume.wake();
  globalThis.document?.addEventListener("visibilitychange", onVisibility);
  onCleanup(() => {
    globalThis.document?.removeEventListener("visibilitychange", onVisibility);
    stop();
  });

  const api: MailboxApi = {
    pending,
    busy,
    error,
    acceptedVersion,
    joinFailure,
    dismissJoinFailure: () => setJoinFailure(null),
    async refresh() {
      setError(null);
      if (activeAccount === null) {
        setError(() => t("contacts.mailbox.sign_in"));
        return;
      }
      // The session the refresh was asked in. Its failure is that session's
      // to see: `stop()` cleared the error line when it ended.
      const asked = scope;
      setBusy(true);
      try {
        await poller.poll(0);
      } catch (err) {
        if (scope === asked) setError(formatMailboxError(err));
      } finally {
        setBusy(false);
      }
    },
    removePending(messageId) {
      if (inbox !== null) inbox.remove(messageId);
      else setPending(pending().filter((r) => r.messageId !== messageId));
    },
  };

  return <MailboxContext.Provider value={api}>{props.children}</MailboxContext.Provider>;
}

/// One plain sentence for the inbox pane. The transport's own message
/// (endpoint paths, pseudonyms, status codes) is not user copy; it goes to
/// the console instead.
function formatMailboxError(err: unknown): () => string {
  if (err instanceof ContactsError && err.variant.kind === "no-identity-secrets") {
    return () => t("contacts.mailbox.sign_in");
  }
  console.warn("contacts inbox: poll failed", err);
  return () => t("contacts.mailbox.poll_failed");
}

/** A dispatch found its session gone after an await. Holds the cursor. */
class StaleMailboxScope extends Error {
  override name = "StaleMailboxScope";
  constructor() {
    super("mailbox dispatch: the session it was polled for ended");
  }
}

export function useMailbox(): MailboxApi {
  const ctx = useContext(MailboxContext);
  if (ctx === undefined) {
    throw new Error("useMailbox() called outside <MailboxProvider>");
  }
  return ctx;
}

/**
 * The one place a Welcome this device could not open is reported. Mounted at
 * the top level beside the update banner, because the Welcome is received for
 * the whole session and the user is usually not on the Contacts route when it
 * arrives.
 */
export function MailboxJoinFailureBanner(): JSX.Element {
  const mailbox = useMailbox();
  return (
    <Show when={mailbox.joinFailure()}>
      {(message) => (
        <div
          role="alert"
          style={{
            position: "fixed",
            top: "1rem",
            left: "50%",
            transform: "translateX(-50%)",
            "max-width": "32rem",
            "z-index": 1000,
            background: "var(--surface-elevated)",
            color: "var(--surface-fg)",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            "box-shadow": "var(--shadow-1)",
            padding: "0.75rem 1rem",
            display: "flex",
            "align-items": "center",
            gap: "1rem",
          }}
        >
          <span style={{ flex: 1, "font-size": "0.9rem" }}>{message()}</span>
          <button type="button" onClick={() => mailbox.dismissJoinFailure()}>
            {t("common.dismiss")}
          </button>
        </div>
      )}
    </Show>
  );
}

/**
 * ADR-0060 — whether a Welcome refused by `joinFromWelcome` was simply not
 * sealed to any KeyPackage of this device (OpenMLS `NoMatchingKeyPackage`):
 * another device's Welcome, or a co-publisher's, read from the shared account
 * mailbox. Every other refusal is a real failed join.
 */
export function isWelcomeForAnotherLeaf(err: unknown): boolean {
  const text =
    err instanceof Error
      ? `${err.message} ${String((err as { cause?: unknown }).cause ?? "")}`
      : String(err);
  return /NoMatchingKeyPackage|no matching key ?package/i.test(text);
}
