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
 *     with identity secrets exists, parked only while the tab is hidden and a
 *     push wake reaches it (`chat/receive-activity.ts`), stopped on sign-out;
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
import { mayParkReceive } from "../chat/receive-activity";
import { createPushWakeReach } from "../push/wake-reach";
import {
  createContext,
  createEffect,
  createSignal,
  on,
  onCleanup,
  Show,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { useGroupNames } from "../chat/GroupNameContext";
import { groupIdHex } from "../devices/sync/state";
import { isDsNetworkFailure } from "../ds/client";
import { journalHex } from "../ds/access";
import { installAddressRecheck } from "../reach/mailbox-write";
import {
  duplicateOfHeld,
  groupRequestHandling,
  installGroupContactRequestSink,
  openArrivedGroupRequest,
} from "./group-contact-request";
import { reachSession, reachSessionSignal } from "../reach/reach-service";
import { DiscoveryClient } from "../discovery/client";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { adoptAnsweredOneToOne, applyContactAck } from "./contact-ack";
import { foundOneToOneWith } from "./found-one-to-one";
import { heldSince } from "./held-since";
import { waitingSentRequests } from "./contact-list-model";
import { forgetInviteLinkSecret } from "./invite-link-secrets";
import { listMintedLinks } from "./minted-links";
import {
  deleteRevokedLinkRow,
  forgetLinksRevokedElsewhere,
  owedRowDeletions,
  revokeMintedLink,
  type OwnLinkRevokeOutcome,
} from "./own-links";
import { loadSentRequests, recordSentRequest } from "./sent-requests";
import { sentIntroFor } from "./sent-request-intros";
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
  decodeContactRequestBody,
  SupersessionRefused,
  type IncomingContactAck,
  type IncomingContactRequest,
  type MailboxRow,
  type IncomingMailboxEvent,
  isAnswerUnreachable,
} from "./client";
import { useProfiles } from "../profile/ProfileContext";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import {
  accountKeysOf,
  contactByAnyKey,
  isRemovedContact,
  loadAccepted,
  loadKeyAliases,
  readPin,
} from "./local-store";
import { contactRowAction, isCurrentContactKey, type ContactRowAction } from "../reach/contact-row";
import {
  contactSignedBy,
  establishedContactOfAccount,
  knownContactRequestVerdict,
  requestPredatesContact,
  siblingMayAdmit,
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
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { conversationWithContact } from "./contact-conversation";
import { duplicateOneToOneFolds, type HeldConversationGroup } from "./duplicate-one-to-one";
import { landPendingPublisherWelcome } from "../channels/channel-publishers";
import { landPublisherDeviceWelcome } from "../channels/publisher-devices";
import { refusalReasonOf } from "../chat/membership-refusal";
import { buildLineage } from "../chat/lineage";
import { heldSends, persistHeldSends } from "../chat/held-send-queue";
import { dropCarriedCopies, moveCarriedCopies } from "./carried-copies";
import { DIRECT_CHAT_STAND_IN } from "../chat/group-title";
import {
  restoreRemovedContact,
  upsertAccepted as upsertAcceptedContact,
  type AcceptedContact as AcceptedContactRow,
} from "./local-store";
import { readIsActive } from "../chat/membership-ended";
import { runHeldWelcomePass, type HeldWelcomeOutcome } from "../chat/held-welcomes";
import { deliverReentryRequest } from "../chat/reentry-admit";
import { errorLogTag } from "../chat/log-tag";
import { reentryRecordFor } from "../chat/reentry-store";
import { noteReentryJoined } from "../chat/reentry-join";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import {
  handOverPendingContact,
  loadPendingContacts,
  moveHeldToConversation,
  pendingContactRefusedBy,
  pendingContacts,
  pendingQueueKey,
  removePendingContact,
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
} from "./link-claims";
import { loadWasm, wasmSync, type Wasm } from "../wasm";
import { blockVerdictForSender } from "../moderation/block-rules";
import { isSuspendedKeyB64, standingWithin, standingsWithin, suspendedAmong } from "../moderation/suspension-marks";
import { REQUEST_STANDING_BOUND_MS, dropsRequestFrom } from "../moderation/suspension-rules";

/** How often held Welcomes are tried again (chosen). */
const HELD_WELCOME_RETRY_MS = 30_000;

/** Copy for a Welcome this device could not open. */
export const JOIN_FAILURE_MESSAGE = (): string => t("contacts.mailbox.join_failed");

/** ADR-0099 section 6 - whether the shared block rule drops this request:
 *  its sender's account key is in the block register. */
function requestBlocked(request: IncomingContactRequest): boolean {
  return (
    blockVerdictForSender(wasmSyncOrNull(), "contact-request", base64Encode(request.fromPubkey)) === "drop"
  );
}

/** ADR-0099 step 4, UX 3 - whether a request is dropped unseen because the
 *  moderation log confirms its sender's account is suspended. Waits a few
 *  seconds at most; a timeout or an unchecked standing keeps the request. */
async function requestFromSuspended(request: IncomingContactRequest): Promise<boolean> {
  return dropsRequestFrom(await standingWithin(base64Encode(request.fromPubkey), REQUEST_STANDING_BOUND_MS));
}

/** The loaded wasm namespace, or `null` before it is. */
function wasmSyncOrNull(): Wasm | null {
  try {
    return wasmSync();
  } catch {
    return null;
  }
}

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
  const membershipEnd = useMembershipEnd();
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

  // `docs/ux/who-can-find-me.md` section 6.4 - a contact request sealed to
  // this account inside a group it shares with the sender. The chat receive
  // path hands every such envelope here; only this account's device opens
  // it, and it joins the request list as an inbox request would, with the
  // group it came through.
  installGroupContactRequestSink(async (arrived) => {
    const polled = scope;
    if (polled === null) return false;
    const own = chats.accountIdentity();
    if (own === null) return false;
    const wasm = await loadWasm();
    const x25519Secret = own.x25519SecretKey;
    const opened = openArrivedGroupRequest(arrived, {
      check: (plaintext, secret, sender) => wasm.checkGroupContactRequest(plaintext, secret, sender),
      x25519Secret,
      decode: (wire, rowId, createdAt, via) =>
        decodeContactRequestBody(wasm, rowId, wire, createdAt, x25519Secret, via),
      allowsGroupMembers: (groupId) => reachSession()?.allowsGroupMembers(groupId) ?? false,
      isContact: (keyB64) => loadAccepted(polled.accountIdB64).some((c) => c.signaturePubkeyB64 === keyB64),
      keyB64: base64Encode,
    });
    if (opened === null) return true;
    if (scope !== polled) return false;
    const { request, row } = opened;
    // ADR-0099 section 6 - a request through a group from a blocked account:
    // dropped unseen, and the row is handled.
    if (requestBlocked(request)) return true;
    // ADR-0099 step 4 - a request from an account the log confirms is
    // suspended: dropped unseen too.
    if (await requestFromSuspended(request)) return true;
    if (scope !== polled) return false;
    if (request.profile !== null) profiles.ingest(base64Encode(request.fromPubkey), request.profile);
    // A contact this device already talks to in a one-to-one: answered
    // silently, as from the inbox (ADR-0073 R.7 case 1). A contact with NO
    // one-to-one here (first met in a group) is the case this request exists
    // for: it is listed, and Accept starts the chat.
    const keyB64 = base64Encode(request.fromPubkey);
    const handling = groupRequestHandling({
      senderIsContact: loadAccepted(polled.accountIdB64).some((c) => c.signaturePubkeyB64 === keyB64),
      holdsOneToOne: holdsOneToOneWith(keyB64),
    });
    if (handling === "answer-silently" && (await handleKnownContactRequest(request, polled.accountIdB64, row))) {
      return true;
    }
    // The same request through the inbox, or one already listed from this
    // person: one request per person.
    if (duplicateOfHeld(request, polled.inbox.pending())) return true;
    if (scope !== polled) return false;
    // Written BEFORE this returns, and the journal poll moves its cursor only
    // after: a reload in between reads the row again (a failed write throws,
    // which reports it unhandled).
    polled.inbox.addFetched([{ request, row }]);
    return true;
  });
  onCleanup(() => installGroupContactRequestSink(null));

  // One person, one one-to-one row (`duplicate-one-to-one.ts`): a
  // one-to-one the other person founded continues an older one this account
  // founded with them, which their device never opened. Recorded in the
  // lineage, so the list shows one row. Re-run whenever the groups or the
  // lineage move (a reload merges rows already held); writing the same
  // register again changes nothing.
  createEffect(() => {
    if (!sync.ready()) return;
    const entries = sync.lineage();
    const groups = chats.groups();
    const identity = chats.identity();
    untrack(() => {
      const lineage = buildLineage(entries);
      const held: HeldConversationGroup[] = [];
      const hexes: string[] = [];
      for (const g of groups) {
        try {
          hexes.push(groupIdHex(g.group.groupId));
        } catch {
          continue;
        }
      }
      const account = sessionAccountOf(auth.state());
      const since = account === null ? new Map<string, number | undefined>() : heldSince(account, hexes, Date.now());
      for (const g of groups) {
        let groupHex: string;
        try {
          groupHex = groupIdHex(g.group.groupId);
        } catch {
          continue;
        }
        const heldSinceMs = since.get(groupHex);
        held.push({
          groupHex,
          conversationHex: lineage.conversationOf(groupHex),
          isOneToOne: conversationKindOf(g.group) === "chat",
          people: personKeysOf(g.group),
          founderKeyB64: founderKeyOf(g.group),
          ...(heldSinceMs === undefined ? {} : { heldSinceMs }),
        });
      }
      for (const fold of duplicateOneToOneFolds(held, ownPersonKeyB64(identity))) {
        void sync.setLineage(hexBytes(fold.successorHex), hexBytes(fold.rootHex)).catch(() => false);
      }
    });
  });

  /** Try one mailbox Welcome against this account's pending publisher
   *  requests (`channels/pending-publisher-requests.ts`), under the identity
   *  the requests are made with. */
  async function landPublisherWelcome(accountIdB64: string, welcome: Uint8Array): Promise<boolean> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return false;
    if (await landPendingPublisherWelcome(identity, accountIdB64, welcome)) return true;
    // ADR-0090 section 2.3: a sibling (or the owner) admitted this device to
    // a channel the account owns or co-publishes.
    return landPublisherDeviceWelcome(identity, welcome);
  }

  /** base64(group id) of the live one-to-one this device holds with the
   *  person, or `null`. A one-to-one whose membership ended here is not
   *  live. */
  function oneToOneGroupWith(keyB64: string): string | null {
    return (
      conversationWithContact({
        contact: { signaturePubkeyB64: keyB64 },
        conversations: chats.groups().filter((g) => readIsActive(g.group) !== false),
        ownKeyB64: ownPersonKeyB64(chats.identity()),
        peopleOf: (g) => personKeysOf(g.group),
        isOneToOne: (g) => conversationKindOf(g.group) === "chat",
      })?.groupIdB64 ?? null
    );
  }

  /** Whether this device holds a live one-to-one with the person. */
  function holdsOneToOneWith(keyB64: string): boolean {
    return oneToOneGroupWith(keyB64) !== null;
  }

  // ADR-0074 section 3.5a rule 5 - what was granted before a rotation of
  // this account's keys is spent. The two owed steps of the rotation record
  // (`devices/key-rotation.ts`: `links`, `requests`) run here, where the
  // directory client and the contacts client are.
  // The session token is read at request time: the ledger-row deletions this
  // client makes (`DELETE /discovery/invite/...`) sit behind discovery's
  // `require_session`, and a tokenless client made every one a 401.
  const spentDiscovery = new DiscoveryClient(undefined, {
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
  });
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
  // link deletes its ledger row itself, once, with the seed the synced
  // record carries (`InviteLinksPanel`). Re-run on every change of the
  // register: a deletion this device still owes (the ledger was unreachable,
  // or the app stopped in between) is tried again, and a link another device
  // revoked only loses this device's local record of it; the ledger is not
  // asked again.
  createEffect(() => {
    const s = auth.state();
    if (s.kind !== "session") return;
    const account = s.account.accountId;
    const revokedRecords = sync.linkRecords().filter((r) => r.revoked);
    if (revokedRecords.length === 0) return;
    void (async () => {
      const owed = owedRowDeletions(account);
      for (const r of revokedRecords) {
        if (!owed.has(r.linkIdHex)) continue;
        try {
          await deleteRevokedLinkRow(spentDiscovery, account, r);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("owed ledger row deletion failed", err instanceof Error ? err.name : "");
        }
      }
      await forgetLinksRevokedElsewhere(
        spentDiscovery,
        account,
        revokedRecords.filter((r) => !owedRowDeletions(account).has(r.linkIdHex)),
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
  /**
   * Base scenario 7 - a person this device asked answered silently (their
   * device still holds this account as a contact, and a one-to-one), while
   * this device holds no one-to-one with them: no Welcome will come, so one
   * is founded here, as Message on a contact card does, and they are a
   * contact with it. Their device joins it; the duplicate-one-to-one rule
   * makes it the head of the conversation they had. A failure keeps the
   * request under Sent and is logged.
   */
  async function foundForSilentAnswers(
    accountIdB64: string,
    acks: readonly IncomingContactAck[],
    current: () => boolean,
  ): Promise<void> {
    const identity = auth.identitySecrets();
    if (identity === null) return;
    for (const ack of acks) {
      if (!current()) return;
      const keyB64 = base64Encode(ack.accepterPubkey);
      if (oneToOneGroupWith(keyB64) !== null) continue;
      const accountIdHex = groupIdHex(ack.accepterAccountId);
      try {
        const founded = await foundOneToOneWith({
          client,
          identity,
          chats,
          profiles,
          contact: {
            signaturePubkeyB64: keyB64,
            displayName: "",
            acceptedAtMs: Date.now(),
            accountIdHex,
          },
          peerAccountId: ack.accepterAccountId,
          peerPubkey: ack.accepterPubkey,
        });
        adoptAnsweredOneToOne(accountIdB64, keyB64, accountIdHex, founded.conversation.groupIdB64);
        setAcceptedVersion(acceptedVersion() + 1);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: founding a chat after a silent answer failed", err instanceof Error ? err.name : "");
      }
    }
  }

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
    const accepted = loadAccepted(accountIdB64);
    const known =
      establishedContactOfAccount(accepted, groupIdHex(fromAccountId), {
        pinKeysB64: pinKeysOf(accountKeysOf(fromAccountIdB64, accountIdB64)),
        bindingKeyB64: readVerifiedBinding(fromAccountIdB64, accountIdB64)?.keyB64 ?? null,
      }) ?? contactSignedBy(accepted, base64Encode(request.fromPubkey));
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
      // Base scenario 7 - answered silently only while this device holds a
      // live one-to-one with them; else listed, and Accept starts the chat.
      holdsOneToOne:
        holdsOneToOneWith(base64Encode(request.fromPubkey)) ||
        conversationWithContact({
          contact: known,
          conversations: chats.groups().filter((g) => readIsActive(g.group) !== false),
          ownKeyB64: ownPersonKeyB64(chats.identity()),
          peopleOf: (g) => personKeysOf(g.group),
          isOneToOne: (g) => conversationKindOf(g.group) === "chat",
        }) !== undefined,
      // A request read again after a lost cursor is not a new one.
      predatesContact: requestPredatesContact(request.createdAt, known.acceptedAtMs),
      // A sibling device that holds the chat answers it; this one waits to
      // be admitted rather than offer an Accept that founds a second chat.
      siblingMayAdmit: siblingMayAdmit({
        otherDeviceCount: sync.otherDeviceCount?.() ?? null,
        requestCreatedAt: request.createdAt,
        nowMs: Date.now(),
      }),
    });
    if (verdict === "show") {
      inbox?.dropWaiting(request.messageId);
      return false;
    }
    let answered = true;
    if (verdict === "answer") {
      // The ack is the whole of the answer on this path: the reset the check
      // just accepted (or accepted earlier) is what moves the conversation.
      try {
        await client.sendContactAck(auth.identitySecrets(), request);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("silent ack to an existing contact failed", err instanceof Error ? err.name : "");
        // Nothing opens their mailbox: asking again changes nothing. Any
        // other failure keeps the request for the next pass.
        answered = isAnswerUnreachable(err);
      }
    }
    if (verdict === "wait" || !answered) {
      // Kept on disk first: the cursor is about to move past this row.
      const kept = inbox?.holdWaiting(row) ?? false;
      if (kept && !waitingKnownRequests.some((w) => w.request.messageId === request.messageId)) {
        waitingKnownRequests.push({ request, row });
      }
      return true;
    }
    if (verdict === "drop") {
      // eslint-disable-next-line no-console
      console.warn("contacts inbox: a request from a contact's account was dropped", check);
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
  // Base scenario 9 - a request waiting for a sibling to admit this device to
  // the chat is judged again when the conversations change (the admission
  // landed) and each minute (its wait ends), not only at the next mailbox row.
  createEffect(
    on(
      () => chats.groups().length,
      () => {
        if (waitingKnownRequests.length > 0) void retryWaitingKnownRequests();
      },
      { defer: true },
    ),
  );
  const waitingRetryTimer = setInterval(() => {
    if (waitingKnownRequests.length > 0) void retryWaitingKnownRequests();
  }, 60_000);
  onCleanup(() => clearInterval(waitingRetryTimer));

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
            { replyGrantB64: request.replyGrantB64, answersRequest: true },
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
            { replyGrantB64: request.replyGrantB64, answersRequest: true },
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
            // The Welcome landed. Nothing opening their mailbox ends it there;
            // any other failure is taken up at the next pass.
            if (!isAnswerUnreachable(err)) throw err;
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
      // The live one-to-one this device holds with a person, if any: a
      // requester who has one is put back on the contact list with it
      // rather than given a second conversation (amended 2026-10-24).
      const liveOneToOneWith = (keyB64: string): string | null => {
        for (const g of chats.groups()) {
          if (conversationKindOf(g.group) !== "chat") continue;
          if (!personKeysOf(g.group).includes(keyB64)) continue;
          if (readIsActive(g.group) !== true) continue;
          return g.groupIdB64;
        }
        return null;
      };
      const undecided = target.undecidedLinkRequests();
      // ADR-0099 step 4 - a held request is decided only after its sender's
      // standing is asked again (the account may have been suspended since
      // it arrived, or the check timed out then): every sender at once,
      // under one bound. Only a verified suspension drops one.
      const suspended = await suspendedAmong(
        undecided.flatMap((r) => (r.linkAnswer === null ? [] : [base64Encode(r.fromPubkey)])),
        REQUEST_STANDING_BOUND_MS,
      );
      for (const req of undecided) {
        if (!current()) return;
        const answer = req.linkAnswer;
        if (answer === null) {
          target.remove(req.messageId);
          continue;
        }
        const key = linkRequestKeyHex(answer.linkId, req.fromPubkey);
        const requesterKeyB64 = base64Encode(req.fromPubkey);
        if (suspended.has(requesterKeyB64)) {
          // Dropped unseen, as at arrival: no card, no one-to-one, no answer.
          claims.forgetOwn(key);
          target.remove(req.messageId);
          continue;
        }
        const liveGroupIdB64 = liveOneToOneWith(requesterKeyB64);
        // ADR-0079 amendment of 2026-10-19 - the moment the request was
        // written decides the link's expiry: the time the DS stamped on the
        // row, kept with the request across a restart.
        const verdict = sync.linkAnswerVerdictAt(
          answer.linkId,
          answer.linkSecret,
          req.fromPubkey,
          Date.parse(req.createdAt),
        );
        let step = linkRequestStep({
          own: claims.own(key),
          answeredElsewhere: answeredElsewhere(key),
          verdict,
          noteHeld: claims.note(key) !== null,
          liveOneToOne: liveGroupIdB64 !== null,
        });
        try {
          if (step === "wait") continue;
          if (step === "restore" && liveGroupIdB64 !== null) {
            // A person removed and invited again: back on the list with the
            // conversation they already have. Nothing is founded or sent; a
            // live link records its use, as the requester's consume did.
            restoreRemovedContact(
              {
                signatureKey: requesterKeyB64,
                accountIdHex: req.fromAccountId === null ? null : groupIdHex(req.fromAccountId),
                groupIdB64: liveGroupIdB64,
                nowMs: Date.now(),
              },
              accountIdB64,
            );
            if (verdict === "accept") await sync.recordLinkUse(answer.linkId, req.fromPubkey);
            if (!current()) return;
            setAcceptedVersion(acceptedVersion() + 1);
            claims.forgetOwn(key);
            target.remove(req.messageId);
            continue;
          }
          if (step === "drop") {
            claims.forgetOwn(key);
            target.remove(req.messageId);
            continue;
          }
          if (step === "refuse") {
            // The link does not count: no card. The requester is answered
            // with a refusal and the request goes; a failed write keeps it
            // for the next pass.
            await client.sendLinkRefusal(auth.identitySecrets(), req);
            if (!current()) return;
            claims.forgetOwn(key);
            target.remove(req.messageId);
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
          // State bundle v3: what was written to them on another device of
          // this account, shown here as copies, follows into the chat (still
          // display only; that device sends them).
          await moveCarriedCopies(accountIdB64, pendingQueueKey(contact), conversationHex);
          return true;
        },
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("pending contact hand-over failed", err instanceof Error ? err.name : "");
    }
  }

  /**
   * ADR-0091 section 5 - join `welcome` over the stale group `groupIdB64` when
   * this device recorded a re-entry request for it. Keeps the conversation
   * and its history (the registry entry is replaced in place, never wiped).
   * `null` when there is no record or the core refuses the Welcome (nothing
   * changed then; the Welcome stays held).
   */
  async function joinAsReentry(
    welcome: Uint8Array,
    groupIdB64: string,
    current: () => boolean,
  ): Promise<ActiveGroup | null> {
    const record = reentryRecordFor(groupIdB64);
    if (record === undefined) return null;
    const prior = chats.get(groupIdB64);
    let group;
    try {
      group = await client.joinReentry(
        auth.identitySecrets(),
        welcome,
        record.keyPackagesB64.map((b) => base64Decode(b)),
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`chat re-entry Welcome not joined: ${errorLogTag(err)}`);
      return null;
    }
    if (!current()) throw new StaleMailboxScope();
    const entry = chats.add({
      label: prior?.label ?? groupNames.nameOf(groupIdB64) ?? "",
      group,
      rejoined: true,
    });
    try {
      await noteReentryJoined({ groupIdB64, group, record });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`chat re-entry bookkeeping failed: ${err instanceof Error ? err.name : "error"}`);
    }
    return entry;
  }

  /** Open one Welcome and put the group in the registry. Throws only when the
   *  session it was opened for ended. */
  async function openWelcome(
    welcome: Uint8Array,
    messageId: number,
    current: () => boolean,
  ): Promise<HeldWelcomeOutcome<ActiveGroup>> {
    try {
      const entry = await membershipEnd.joinAlone(async () => {
        const group = await client.joinGroupFromWelcome(auth.identitySecrets(), welcome);
        // Signed out during the join: the chat list is not this account's any
        // more. Not added, not marked joined; the cursor is held (the catch
        // below is skipped by the rethrow).
        if (!current()) throw new StaleMailboxScope();
        const prior = chats.get(base64Encode(group.groupId));
        // ADR-0051 — a name this device already holds for the group (a second
        // Welcome) titles it at once; otherwise the stand-in the title resolver
        // reads as "unnamed". `rejoined`: a group replaced under the same id is
        // a new conversation to a view that has it open.
        const added = chats.add({
          label: groupNames.nameOf(base64Encode(group.groupId)) ?? `Joined group (Welcome ${messageId})`,
          group,
          rejoined: true,
        });
        await membershipEnd.startOver(prior);
        return added;
      });
      joinedWelcomeIds.add(messageId);
      return { kind: "joined", entry };
    } catch (err) {
      if (err instanceof StaleMailboxScope) throw err;
      const stillActive = membershipEnd.stillActiveGroupOf(err);
      if (stillActive !== null) {
        // ADR-0091 section 5 - a Welcome refused because this device still
        // holds its group as active is first tried as the answer to a
        // re-entry request this device recorded.
        const reentered = await joinAsReentry(welcome, stillActive, current);
        if (reentered !== null) {
          joinedWelcomeIds.add(messageId);
          return { kind: "joined", entry: reentered };
        }
        return { kind: "still-active", groupIdB64: stillActive };
      }
      // eslint-disable-next-line no-console
      console.warn("joinGroupFromWelcome failed", messageId, err);
      // ADR-0060 — every device of the account reads the one account mailbox,
      // so a Welcome here may add ANOTHER device of ours, or be a channel
      // co-publisher's (opened by that flow under the account identity).
      // Neither holds a KeyPackage of this device, and that is not a failed
      // join the user needs to hear about.
      // A Welcome the core refuses as not a conversation (C-86, typed
      // `noRoleTable`) is a channel PublisherGroup's or a DeviceGroup's, which
      // their own flows open; not a failed join either (walk of 2026-11-14:
      // a returning owner device showed the banner for its channel's Welcome).
      return isWelcomeForAnotherLeaf(err) || isWelcomeNotAConversation(err) ? { kind: "not-ours" } : { kind: "failed" };
    }
  }

  /** What follows joins, once they are in the registry. */
  async function settleJoins(accountIdB64: string, entries: readonly ActiveGroup[]): Promise<void> {
    // The joined group lives only in memory until a snapshot lands, and the
    // Welcome cannot be opened a second time in this page lifetime. A failed
    // persist throws, which HOLDS the mailbox cursor. After a reload the
    // durable snapshot still holds the unconsumed KeyPackage, so the
    // re-fetched Welcome is opened then; if that snapshot still holds the
    // group as active the core refuses it and it is held like any other
    // (`retryHeldWelcomes`).
    await chats.persistSnapshotDurable(accountIdB64);
    for (const entry of entries) {
      // ADR-0087 section 9 - an end still under way goes first, and a person
      // back in a group this account's devices were told it left says so.
      await membershipEnd.noteJoined(entry);
      // ADR-0048 §2 — publish this device's profile into every group it just
      // joined, once the join is durable. The members already there cannot
      // learn the newcomer's name any other way.
      profiles.groupJoined(entry.groupIdB64);
      // ADR-0079 section 5 - a one-to-one with a person this account added
      // through their link: what was written to them before their app
      // answered goes out now, in order, through the held-send queue.
      await handOverToConversation(accountIdB64, entry.groupIdB64);
    }
  }

  /** One pass over the held Welcomes at a time. */
  let heldWelcomePass: Promise<void> = Promise.resolve();

  /** A pass over the Welcomes this device holds (`chat/held-welcomes.ts`). */
  function retryHeldWelcomes(): Promise<void> {
    const run = async (): Promise<void> => {
      const polled = scope;
      if (polled === null) return;
      const current = (): boolean => scope === polled;
      const joinedEntries = await runHeldWelcomePass<ActiveGroup>({
        held: () => membershipEnd.heldWelcomes(),
        open: (held) => openWelcome(base64Decode(held.welcomeB64), held.messageId, current),
        readJournal: (groupIdB64) => membershipEnd.readJournal(groupIdB64),
        settle: () => membershipEnd.settle(),
        drop: (messageId) => membershipEnd.dropHeldWelcome(messageId),
        // ADR-0091 section 5 - never dropped while a re-entry record names
        // its group: it may be the answer, and the core decides that.
        keepForReentry: (groupIdB64) => reentryRecordFor(groupIdB64) !== undefined,
        noteFailed: () => setJoinFailure(JOIN_FAILURE_MESSAGE),
        current,
      });
      if (joinedEntries.length > 0 && current()) await settleJoins(polled.accountIdB64, joinedEntries);
    };
    const pass = heldWelcomePass.then(run, run).catch((err: unknown) => {
      if (err instanceof StaleMailboxScope) return;
      // eslint-disable-next-line no-console
      console.warn("held Welcomes not retried", err instanceof Error ? err.name : "");
    });
    heldWelcomePass = pass;
    return pass;
  }

  // An end finishing is what a held Welcome waits for, and a pass that could
  // not read the journal is tried again on a timer.
  createEffect(() => {
    membershipEnd.settledVersion();
    void untrack(() => retryHeldWelcomes());
  });
  const heldWelcomeTimer = setInterval(() => {
    if (membershipEnd.heldWelcomes().length > 0) void retryHeldWelcomes();
  }, HELD_WELCOME_RETRY_MS);
  onCleanup(() => clearInterval(heldWelcomeTimer));

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
    // ADR-0060 section 1 (amended 2026-11-06) - each row by the key that
    // admitted it: a retired `contact` key from a current contact is
    // processed and the contact handed the current grant; from anyone else
    // it is dropped, silently.
    const rowSession = reachSession();
    const { peekWelcomeFounder } = await loadWasm();
    const contactRows = loadAccepted();
    const contactAliases = loadKeyAliases();
    const isContact = (keyB64: string): boolean =>
      isCurrentContactKey(keyB64, (k) => contactByAnyKey(k, contactRows, contactAliases), (k) => isRemovedContact(k));
    const rowAction = (reachPk: Uint8Array | undefined, senders: () => readonly string[]): ContactRowAction =>
      rowSession === null
        ? "process"
        : contactRowAction({
            reachPk,
            verdict: (pk, c) => rowSession.contactRowVerdict(pk, c),
            senderIsContact: () => senders().some(isContact),
          });
    /** Hand a contact the current grant: republish the profile in the
     *  one-to-one with them, nothing shown. */
    const regrant = (senderB64: string): void => {
      const own = ownPersonKeyB64(chats.identity());
      const conv = chats
        .groups()
        .find(
          (g) =>
            conversationKindOf(g.group) === "chat" &&
            personKeysOf(g.group).some((k) => k !== own && (k === senderB64 || contactByAnyKey(k, contactRows, contactAliases)?.signaturePubkeyB64 === contactByAnyKey(senderB64, contactRows, contactAliases)?.signaturePubkeyB64)),
        );
      if (conv !== undefined) profiles.requested(conv.groupIdB64);
    };
    const dropped = (): void => {
      // eslint-disable-next-line no-console
      console.info("[reach] a mailbox row under a retired contact key was dropped");
    };
    const fresh: FetchedRequest[] = [];
    await retryWaitingKnownRequests();
    let welcomeAttempted = false;
    /** Base scenario 7 - acks answering a request of this device from a
     *  person it holds no one-to-one with; founded for after the batch. */
    const needsOneToOne: IncomingContactAck[] = [];
    /** A Welcome of this batch kept for later: a one-to-one it may carry is
     *  not founded a second time. */
    let welcomeKept = false;
    let joined = false;
    const joinedEntries: ActiveGroup[] = [];
    let failed = false;
    // ADR-0099 step 4 - the senders' standings for the whole batch, asked
    // at once under one bound (a slow moderation server holds the poll a few
    // seconds at most, not a few seconds per request).
    const standings = await standingsWithin(
      events.flatMap((ev) =>
        ev.kind === "contact-request" && !requestBlocked(ev.request) ? [base64Encode(ev.request.fromPubkey)] : [],
      ),
      REQUEST_STANDING_BOUND_MS,
    );
    try {
      for (const ev of events) {
        if (ev.kind === "contact-request") {
          const senderB64 = base64Encode(ev.request.fromPubkey);
          // ADR-0099 section 6 - a request from a blocked account is dropped
          // unseen, whichever way it came (the inbox, an invite link).
          if (requestBlocked(ev.request)) continue;
          // ADR-0099 step 4 - and one from an account the moderation log
          // confirms is suspended (verified only; never on a timeout).
          if (dropsRequestFrom(standings.get(senderB64) ?? "unknown")) continue;
          const action = rowAction(ev.reachPk, () => [senderB64]);
          if (action === "drop") {
            dropped();
            continue;
          }
          if (action === "regrant") regrant(senderB64);
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
          // Who-can-find-me 6.4 - the same request already listed through a
          // group it was also sent into.
          if (duplicateOfHeld(ev.request, [...target.pending(), ...fresh.map((f) => f.request)])) continue;
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
        } else if (ev.kind === "publisher-welcome") {
          // ADR-0087 section 15, ADR-0045 decision 2 - an owner's device
          // approved a publisher request of this account; it lands whenever
          // it arrives, after a restart too. Not a request's: nothing. A
          // landed join that cannot be made durable throws, which holds the
          // cursor.
          await landPublisherWelcome(accountIdB64, ev.welcomeBytes);
        } else if (ev.kind === "welcome") {
          if (joinedWelcomeIds.has(ev.welcome.messageId)) continue;
          // A pending publisher request's Welcome sent as a plain Welcome row
          // is the request's, never a chat to join.
          if (await landPublisherWelcome(accountIdB64, ev.welcome.welcomeBytes)) continue;
          welcomeAttempted = true;
          // An end under way goes first: the Welcome may replace its group.
          await membershipEnd.settle();
          // ADR-0060 section 1 (2026-11-06) - a Welcome admitted by a retired
          // contact key: its founder is read WITHOUT joining
          // (`peekWelcomeFounder`), and a Welcome from someone who is not a
          // contact is discarded unjoined. A peek that fails leaves no sender:
          // dropped too. From a contact, the join publishes the profile with
          // the current grant (`settleJoins`): the regrant.
          const welcomeAction = rowAction(ev.reachPk, () => {
            const identity = chats.identity();
            if (identity === null) return [];
            try {
              const founder = peekWelcomeFounder(ev.welcome.welcomeBytes, identity);
              try {
                return [base64Encode(founder.founderAccountKey)];
              } finally {
                founder.free();
              }
            } catch {
              return [];
            }
          });
          if (welcomeAction === "drop") {
            dropped();
            continue;
          }
          const opened = await openWelcome(ev.welcome.welcomeBytes, ev.welcome.messageId, current);
          if (opened.kind === "joined") {
            joinedEntries.push(opened.entry);
            joined = true;
          } else if (opened.kind === "failed") {
            failed = true;
          } else if (opened.kind === "still-active") {
            // ADR-0045 amendment (C-88) - this device still holds that group
            // as active, so the core refused the Welcome and changed nothing.
            // The cursor is about to move past the row, so it is kept first:
            // a failed write throws here, which holds the cursor. Its group's
            // journal is read after this batch.
            await membershipEnd.holdWelcome({
              messageId: ev.welcome.messageId,
              groupIdB64: opened.groupIdB64,
              welcomeB64: base64Encode(ev.welcome.welcomeBytes),
              heldAtMs: Date.now(),
            });
            welcomeKept = true;
          }
        } else if (ev.kind === "conversation-reentry") {
          // ADR-0091 section 3, route 2 - the other person of a one-to-one
          // asks to be brought back. Decoded here, judged by the re-admitter
          // (`chat/reentry-admit.ts`), which checks the conversation, the
          // epoch and the KeyPackage; anything that does not fit is dropped.
          const wasm = await loadWasm();
          let decoded;
          try {
            decoded = wasm.decodeConversationReentryEnvelope(ev.envelope);
          } catch {
            continue;
          }
          try {
            deliverReentryRequest({
              route: "mailbox",
              groupId: decoded.groupId,
              epochLeft: decoded.epochLeft,
              gapFromMs: decoded.gapFromMs,
              gapKey: decoded.gapKey,
              keyPackage: decoded.keyPackage,
            });
          } finally {
            decoded.free();
          }
        } else if (ev.kind === "link-refusal") {
          // ADR-0079 amendment of 2026-10-19 - the inviter's app refused the
          // request this account sent through their link (the link no longer
          // counted): the pending contact and the messages held for it go,
          // silently. Only a refusal signed by the key the link named, over
          // that link and this account, removes anything.
          const own = chats.accountIdentity();
          if (own === null) continue;
          const wasm = await loadWasm();
          const refused = pendingContactRefusedBy(pendingContacts(), ev.refusal, own.signaturePublicKey, (w, k, l, o) =>
            wasm.verifyLinkRefusal(w, k, l, o),
          );
          if (refused === null) continue;
          await removePendingContact(accountIdB64, refused.accountIdHex);
          await dropCarriedCopies(accountIdB64, refused.accountIdHex);
          void reachSession()?.closeRepliesTo(journalHex(base64Decode(refused.signaturePubkeyB64)));
        } else {
          // A retired contact key from someone who is not a contact: dropped.
          const ackSenderB64 = base64Encode(ev.ack.accepterPubkey);
          const ackAction = rowAction(ev.reachPk, () => [ackSenderB64]);
          if (ackAction === "drop") {
            dropped();
            continue;
          }
          if (ackAction === "regrant") regrant(ackSenderB64);
          // 1.9b1: contact-ack — the recipient of our ContactRequest pressed
          // Accept. A silent answer from a person this device removed and
          // asked again puts them back with the one-to-one both still hold
          // (base scenario 7).
          const ackOutcome = applyContactAck(ev.ack, accountIdB64, {
            current,
            fileFoundName: (key, found) => profiles.fileFoundName(key, found),
            liveOneToOneWith: (key) => (current() ? oneToOneGroupWith(key) : null),
          });
          // ... and when this device holds no one-to-one with them, one is
          // founded after this batch (their device still holds one, so no
          // Welcome comes).
          if (
            ackOutcome === "needs-one-to-one" &&
            !needsOneToOne.some((a) => base64Encode(a.accepterPubkey) === ackSenderB64)
          ) {
            needsOneToOne.push(ev.ack);
          }
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
        await settleJoins(accountIdB64, joinedEntries);
      }
      if (needsOneToOne.length > 0 && !welcomeKept) await foundForSilentAnswers(accountIdB64, needsOneToOne, current);
      if (fresh.some((f) => f.undecidedLink === true)) void decideLinkRequests();
      void retryHeldWelcomes();
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

  /** Whether a push wake can reach this tab at all (`push/wake-reach.ts`). */
  const pushWakeReach = createPushWakeReach();

  /**
   * `chat/receive-activity.ts`: the loop sleeps only where a push wake reaches
   * it. Parking on `hidden` alone meant a closed window of the desktop app, and
   * a tab with notifications refused, took no contact request and no Welcome at
   * all: nothing else could wake them. This provider sits above the call host,
   * so it cannot see a call; a hidden tab push reaches parks during one as it
   * did.
   */
  function mayPark(): boolean {
    return mayParkReceive({
      documentHidden: globalThis.document?.visibilityState === "hidden",
      callInProgress: false,
      pushWake: pushWakeReach.read(),
    });
  }

  function start(accountIdB64: string): void {
    if (activeAccount === accountIdB64 && loopAbort !== null) return;
    stop();
    activeAccount = accountIdB64;
    const opened = createPendingInbox({
      accountIdB64,
      isAccepted: (keyB64) =>
        loadAccepted(accountIdB64).some((c) => c.signaturePubkeyB64 === keyB64),
      // A request from a contact this device holds no one-to-one with is
      // listed whichever way it came (a group, or the inbox: base scenario
      // 7); Accept starts the chat.
      keepsFromContact: (request) => !holdsOneToOneWith(base64Encode(request.fromPubkey)),
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
      isPaused: mayPark,
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
    // ADR-0099 section 6 - a request from a blocked account is never listed,
    // also one that was held before the block was written.
    pending: () =>
      pending().filter((r) => !requestBlocked(r) && !isSuspendedKeyB64(base64Encode(r.fromPubkey))),
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

/**
 * Whether `joinFromWelcome` refused a Welcome because its group is not a
 * conversation (no role table, C-86): a channel PublisherGroup's or a
 * DeviceGroup's Welcome read from the shared account mailbox. The typed
 * refusal (`refusalReason === "noRoleTable"`, followed through wrappers) is
 * read first; the core's sentence only where no reason travelled.
 */
export function isWelcomeNotAConversation(err: unknown): boolean {
  const reason = refusalReasonOf(err);
  if (reason !== undefined) return reason === "noRoleTable";
  const text =
    err instanceof Error
      ? `${err.message} ${String((err as { cause?: unknown }).cause ?? "")}`
      : String(err);
  return /carries no role table, so it is not a conversation/i.test(text);
}

/** The founder of a conversation: the role table's owner (a one-to-one's
 *  never changes, ADR-0062), else the person at leaf 0; `null` when neither
 *  reads. */
function founderKeyOf(group: WasmGroupHandle): string | null {
  try {
    const owner = group.roleTableOwner();
    if (owner !== undefined && owner.length === 32) return base64Encode(owner);
  } catch {
    // fall through to the roster
  }
  try {
    const roster = group.members() as unknown as {
      leafIndex: number;
      signaturePublicKey: Uint8Array;
      accountKey?: Uint8Array;
    }[];
    const first = roster.find((m) => m.leafIndex === 0);
    if (first === undefined) return null;
    const key = first.accountKey !== undefined && first.accountKey.length === 32 ? first.accountKey : first.signaturePublicKey;
    return base64Encode(key);
  } catch {
    return null;
  }
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
