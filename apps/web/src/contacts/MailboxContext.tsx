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
import {
  loadAccepted,
  loadCursor,
  saveCursor,
  upsertAccepted,
  type AcceptedContact,
} from "./local-store";
import { createMailboxPoller, type MailboxPoller } from "./mailbox-poller";

/** Copy for a Welcome this device could not open. */
export const JOIN_FAILURE_MESSAGE =
  "You were added to a group, but the invitation could not be opened on " +
  "this device. Ask the person who added you to add you again.";

export interface MailboxApi {
  /** Contact requests awaiting Accept / Decline, oldest first. In memory
   *  only — the intro plaintext never goes to localStorage. */
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
  /** Drop a request from the pending list (after Accept or Decline). */
  removePending(messageId: number): void;
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

  const [pending, setPending] = createSignal<readonly IncomingContactRequest[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [acceptedVersion, setAcceptedVersion] = createSignal(0);
  const [joinFailure, setJoinFailure] = createSignal<string | null>(null);

  // The single-account mailbox cursor (`lakko.contacts.v2`), mirrored in
  // memory so the poller's cursor re-read is synchronous.
  let cursor = loadCursor();
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
    const seen = new Set(pending().map((r) => r.messageId));
    const fresh: IncomingContactRequest[] = [];
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
          if (!seen.has(ev.request.messageId)) {
            seen.add(ev.request.messageId);
            fresh.push(ev.request);
          }
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
              loadAccepted().find(
                (c) => c.signaturePubkeyB64 === base64Encode(ev.ack.accepterPubkey),
              )?.displayName ?? "",
            acceptedAtMs: Date.now(),
            // ADR-0021 — the peer's account_id (hex) so the SFU group-call
            // roster can resolve to this contact's name.
            accountIdHex: groupIdHex(ev.ack.accepterAccountId),
          };
          upsertAccepted(accepter);
          setAcceptedVersion(acceptedVersion() + 1);
        }
      }
      if (fresh.length > 0) setPending([...pending(), ...fresh]);
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
    readCursor: () => cursor,
    writeCursor: (next) => {
      cursor = next;
      saveCursor(next);
    },
    fetch: async (after, waitSecs, signal) =>
      client.pollMailbox(auth.identitySecrets(), after, waitSecs, signal),
    dispatch,
  });

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
    cursor = loadCursor();
    const controller = new AbortController();
    loopAbort = controller;
    void runLongPollLoop(controller.signal, {
      shouldContinue: () => true,
      isPaused: isHidden,
      waitUntilResumed: (signal) => resume.wait(signal),
      tick: async (signal) => {
        // Long-poll the account mailbox (ADR-0060: the one inbound mailbox).
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
        setError("please sign in again to check for new requests");
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
      setPending(pending().filter((r) => r.messageId !== messageId));
    },
  };

  return <MailboxContext.Provider value={api}>{props.children}</MailboxContext.Provider>;
}

/// One plain sentence for the inbox pane. The transport's own message
/// (endpoint paths, pseudonyms, status codes) is not user copy; it goes to
/// the console instead.
function formatMailboxError(err: unknown): string {
  if (err instanceof ContactsError && err.variant.kind === "no-identity-secrets") {
    return "please sign in again to check for new requests";
  }
  console.warn("contacts inbox: poll failed", err);
  return "Couldn't check for new contact requests. Trying again.";
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
            Dismiss
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
