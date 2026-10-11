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
import { reachSession } from "../reach/reach-service";
import { DiscoveryClient } from "../discovery/client";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import { waitingSentRequests } from "./contact-list-model";
import { forgetInviteLinkSecret } from "./invite-link-secrets";
import { forgetMintedLink, mintedLinkFor } from "./minted-links";
import { loadSentRequests, recordSentRequest } from "./sent-requests";
import {
  installSpentRunners,
  linkRevokeOutcome,
  requestsToResend,
  revocationSeedOrder,
  spentFailureOutcome,
  type RevokeAttempt,
  type SpentOutcome,
} from "./rotation-spent";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import {
  createWakeLatch,
  LONG_POLL_WAIT_SECS,
  runLongPollLoop,
  sleepCancellable,
} from "../ds/long-poll-loop";
import { base64Encode } from "../auth/account-id";
import {
  ContactsClient,
  ContactsError,
  type IncomingContactRequest,
  type IncomingMailboxEvent,
} from "./client";
import { useProfiles } from "../profile/ProfileContext";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { loadAccepted, upsertAccepted, type AcceptedContact } from "./local-store";
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

/** The own predecessor keys whose mailbox is still read (ADR-0066 section
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
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    // ADR-0050: requests, acks and the account mailbox are the person's;
    // groups and the device mailbox are this device's.
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  // ADR-0066 section 3.5a rule 6 - every write to a person's mailbox that is
  // refused re-checks that person's address once, through this client's
  // directory fetch and fail-closed gate.
  installAddressRecheck((accountKey) => client.recheckAddress(accountKey));
  onCleanup(() => installAddressRecheck(null));

  // ADR-0066 section 3.5a rule 5 - what was granted before a rotation of
  // this account's keys is spent. The two owed steps of the rotation record
  // (`devices/key-rotation.ts`: `links`, `requests`) run here, where the
  // directory client and the contacts client are.
  const spentDiscovery = new DiscoveryClient();
  installSpentRunners({
    links: async () => {
      const secrets = auth.identitySecrets();
      const current = chats.accountIdentity();
      const predecessors = chats.ownPredecessorIdentities();
      if (secrets === null || current === null || predecessors.length === 0) return null;
      const accountIdB64 = base64Encode(secrets.accountId);
      // The owner list is answered to the key the directory publishes NOW.
      const currentSeed = current.ed25519Seed;
      let listed;
      try {
        listed = await spentDiscovery.listInvitesByOwner(secrets.accountId, currentSeed);
      } catch (err) {
        return [spentFailureOutcome(err)];
      } finally {
        currentSeed.fill(0);
      }
      if (listed.kind !== "list") return [];
      // A link is signed by the account key of the moment it was minted: a
      // predecessor's. The current seed is never tried, so a link minted
      // after the rotation is not this step's to revoke.
      const seeds = revocationSeedOrder(predecessors.map((p) => p.ed25519Seed));
      const outcomes: SpentOutcome[] = [];
      try {
        for (const invite of listed.invites) {
          const blobId = base64UrlDecodeNoPad(invite.blobIdSlug);
          const attempts: RevokeAttempt[] = [];
          for (const seed of seeds) {
            try {
              const result = await spentDiscovery.revokeInvite(blobId, seed);
              attempts.push(result.kind === "revoked" ? "revoked" : "not-owner");
            } catch (err) {
              attempts.push(spentFailureOutcome(err) === "unreachable" ? "unreachable" : "not-owner");
            }
            const last = attempts[attempts.length - 1];
            if (last === "revoked" || last === "unreachable") break;
          }
          const outcome = linkRevokeOutcome(attempts);
          outcomes.push(outcome);
          if (outcome !== "done") continue;
          // Out of every local list of own links: the link's reach key, the
          // kept secret and the kept link id.
          const local = await mintedLinkFor(accountIdB64, "contact", invite.blobIdSlug);
          if (local !== null) await reachSession()?.revokeLink(local.linkId).catch(() => undefined);
          await forgetMintedLink(accountIdB64, "contact", invite.blobIdSlug);
          forgetInviteLinkSecret(base64UrlEncodeNoPad(secrets.accountId), invite.blobIdSlug);
        }
      } finally {
        for (const seed of seeds) seed.fill(0);
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

  const [pending, setPending] = createSignal<readonly IncomingContactRequest[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();
  const [acceptedVersion, setAcceptedVersion] = createSignal(0);
  const [joinFailure, setJoinFailure] = createLiveText();

  // The signed-in account's mailbox cursor + pending requests, persisted
  // together (`inbox-store.ts`). `null` while signed out.
  let inbox: PendingInbox | null = null;
  // Welcome rows already joined in this page lifetime. A batch whose cursor
  // write was HELD (the post-join persist failed) is fetched again; without
  // this, the second pass would try to open a Welcome whose KeyPackage the
  // first pass consumed, and report a failure for a group that did join.
  const joinedWelcomeIds = new Set<number>();

  let activeAccount: string | null = null;
  let loopAbort: AbortController | null = null;
  const resume = createWakeLatch();

  async function dispatch(events: readonly IncomingMailboxEvent[]): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") throw new Error("mailbox dispatch without a session");
    const accountIdB64 = s.account.accountId;
    const target = inbox;
    if (target === null) throw new Error("mailbox dispatch without an inbox");
    const fresh: FetchedRequest[] = [];
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
          // ADR-0066 section 3.5a rule 5 - one request per person: a request
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
            // ADR-0021 — the peer's account_id (hex) so the SFU group-call
            // roster can resolve to this contact's name.
            accountIdHex: groupIdHex(ev.ack.accepterAccountId),
          };
          upsertAccepted(accepter, accountIdB64);
          setAcceptedVersion(acceptedVersion() + 1);
        }
      }
      // Persisted with the cursor it was fetched under; a failed write throws
      // here, which holds the cursor so the rows are fetched again.
      if (fresh.length > 0) target.addFetched(fresh);
      if (joined) {
        // The joined group lives only in memory until a snapshot lands, and
        // the Welcome cannot be opened a second time in this page lifetime.
        // A failed persist throws, which HOLDS the mailbox cursor: after a
        // reload the durable snapshot still holds the unconsumed KeyPackage,
        // so the re-fetched Welcome can be opened then.
        await chats.persistSnapshotDurable(accountIdB64);
        // ADR-0048 §2 — publish this device's profile into every group it just
        // joined, once the join is durable. The members already there cannot
        // learn the newcomer's name any other way.
        for (const groupIdB64 of joinedGroupIds) profiles.groupJoined(groupIdB64);
      }
    } finally {
      if (failed) setJoinFailure(JOIN_FAILURE_MESSAGE);
      if (welcomeAttempted) {
        void auth.republishKeyPackage().catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("KeyPackage republish after Welcome failed", err);
        });
      }
    }
  }

  const poller: MailboxPoller = createMailboxPoller<IncomingMailboxEvent>({
    readCursor: () => inbox?.cursor() ?? 0,
    writeCursor: (next) => {
      // One write for the cursor and the pending rows; a failure throws and
      // leaves the in-memory cursor where it was.
      if (inbox === null) throw new Error("mailbox cursor write without an inbox");
      inbox.advanceCursor(next);
    },
    fetch: async (after, waitSecs, signal) =>
      client.pollMailbox(auth.identitySecrets(), after, waitSecs, signal),
    dispatch,
  });

  // ADR-0066 section 3.5 (Stage A) — after a proven key rotation the
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
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
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
      dispatch: (events) => dispatch(predecessorMailboxEvents(events)),
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
  // addressed to the ACCOUNT mailbox (the device mailbox is claimed and
  // closed, and no longer polled; its cursor key `lakko.contacts.device-
  // mailbox.v1` is left unread).

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
    setPending(opened.pending());
    // Requests kept from an earlier page load: back through the poller's own
    // verify-and-open path before they are listed.
    void opened
      .restore((rows) => client.decodePendingRequestRows(auth.identitySecrets(), rows))
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("contacts inbox: restoring pending requests failed", err);
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
        // (ADR-0066 section 3.5); then the long poll of the account's.
        // Rule 4 of section 3.5a: read only, for 30 days from the adoption,
        // then left. Nothing is registered or written there.
        await pollPredecessorMailboxes(accountIdB64, signal);
        await poller.poll(LONG_POLL_WAIT_SECS, signal);
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
    if (loopAbort !== null) {
      loopAbort.abort();
      loopAbort = null;
    }
    if (inbox !== null) {
      inbox.dispose();
      inbox = null;
    }
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
      setBusy(true);
      try {
        await poller.poll(0);
      } catch (err) {
        setError(formatMailboxError(err));
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
