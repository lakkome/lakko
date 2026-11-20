// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 — the session-wide runtime of the per-person account status.
 *
 * Owns, for the signed-in account:
 *
 *   * binding the store (`person-status.ts`) to the account, encrypted under
 *     the device key (`account-status-idb.ts`), so it is wiped with it;
 *   * the receive sink of the account-deleted statement: verified against
 *     the conversation's group id and the sender's account key resolved at
 *     receipt (C-68), recorded under every key this device knows for that
 *     person, and, where this device is the group's owner or an admin, the
 *     removal of that person's leaves (section 5), recorded so it is sent
 *     once per (person, group) and retried while owed;
 *   * the observer of every identity answer (`discovery/identity-answers.ts`):
 *     `404`, the held key, a verifying conversation link, another key;
 *   * the on-screen check: a person a view shows is asked about only when
 *     `directoryCheckDue` says so, one request in flight per person.
 *
 * Renders nothing.
 */

import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { installAccountDeletedSink, acceptAccountDeleted } from "../chat/account-deleted-envelope";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { conversationKindOf } from "../chat/conversation-kind";
import { readIsActive } from "../chat/membership-ended";
import { markOwnPublished } from "../chat/own-published";
import type { ProposalRecordView } from "../chat/pending-requests";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { ownPersonKeyB64, personKeyOf, type RosterLeaf } from "../chat/person-key";
import { ContactsClient } from "../contacts/client";
import { accountIdB64OfHex, accountIdB64OfKey, loadAccepted, readPin } from "../contacts/local-store";
import { useMailbox } from "../contacts/MailboxContext";
import { dropHeldForDeletedPending, pendingContacts } from "../contacts/pending-contacts";
import { DiscoveryClient } from "../discovery/client";
import { installIdentityAnswerObserver } from "../discovery/identity-answers";
import type { IdentityFetchResponse } from "../discovery/wire";
import { useProfiles } from "../profile/ProfileContext";
import { loadWasm } from "../wasm";
import { classifyIdentityAnswer } from "./account-status";
import { IdbPersonStatusBackend } from "./account-status-idb";
import { deletedRemovalPlan } from "./deleted-removal";
import {
  bindPersonStatus,
  directoryCheckDueFor,
  installPersonChecker,
  isDeletedAccount,
  personStatusEntries,
  personStatusOf,
  recordDeletionStatement,
  recordDirectoryAnswer,
  recordDirectoryAttempt,
  recordRemoval,
  usePersonStatusBackend,
} from "./person-status";

/** How often owed removals are tried again. Chosen, not measured. */
const OWED_RETRY_MS = 10 * 60 * 1000;

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function bytesOfHex(hex: string): Uint8Array | null {
  if (!/^[0-9a-f]{32}$/.test(hex)) return null;
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i++) out[i] = Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

export function PersonStatusRuntime(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const mailbox = useMailbox();

  usePersonStatusBackend(new IdbPersonStatusBackend(getDeviceKey));

  const accountId = (): string | null => sessionAccountOf(auth.state());

  createEffect(() => {
    const account = accountId();
    untrack(() => {
      void bindPersonStatus(account).then(() => {
        if (account !== null) void retryOwedRemovals();
      });
    });
  });

  const contacts = new ContactsClient(new DiscoveryClient(), undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: durableSnapshotHook(chats, accountId),
    onPersistWithCommit: async (entry) => {
      const s = auth.state();
      if (s.kind !== "session") throw new Error("no active session - commit aborted (ADR-0040)");
      await chats.persistSnapshotWithCommit(s.account.accountId, entry);
    },
  });

  /** The account id (hex) this device holds for a person key: a trusted
   *  local record first, else the person's profile claim, else the record. */
  function accountIdHexOfKey(keyB64: string): string | null {
    const trusted = accountIdB64OfKey(keyB64);
    if (trusted !== null) {
      try {
        return hexOf(base64Decode(trusted));
      } catch {
        // fall through
      }
    }
    const claimed = profiles.accountIdHexOf(keyB64);
    if (claimed !== null) return claimed;
    const pending = pendingContacts().find((p) => p.signaturePubkeyB64 === keyB64);
    if (pending !== undefined) return pending.accountIdHex;
    return personStatusOf(keyB64)?.accountIdHex ?? null;
  }

  /** Every key this device holds for the person with `accountIdHex`. */
  function heldKeysOf(accountIdHex: string): string[] {
    const keys = new Set<string>();
    const accountIdB64 = accountIdB64OfHex(accountIdHex);
    if (accountIdB64 !== null) {
      const pin = readPin(accountIdB64);
      if (pin !== undefined) {
        keys.add(pin.pinnedSignaturePubkeyB64);
        for (const k of pin.provenPredecessorsB64 ?? []) keys.add(k);
      }
    }
    for (const c of loadAccepted()) {
      if (c.accountIdHex === accountIdHex) keys.add(c.signaturePubkeyB64);
    }
    for (const p of pendingContacts()) {
      if (p.accountIdHex === accountIdHex) keys.add(p.signaturePubkeyB64);
    }
    for (const b of profiles.accountIdBindings()) {
      if (b.accountIdHex === accountIdHex) keys.add(b.personKeyB64);
    }
    for (const [k, rec] of personStatusEntries()) {
      if (rec.accountIdHex === accountIdHex) keys.add(k);
    }
    return [...keys];
  }

  // ---- the identity-answer observer ----------------------------------------
  installIdentityAnswerObserver((accountIdBytes: Uint8Array, bundle: IdentityFetchResponse | null) => {
    if (accountId() === null || accountIdBytes.length !== 16) return;
    const idHex = hexOf(accountIdBytes);
    const keys = heldKeysOf(idHex);
    if (keys.length === 0) return;
    const own = ownPersonKeyB64(chats.identity());
    void loadWasm().then((wasm) => {
      const now = Date.now();
      for (const key of keys) {
        if (key === own) continue;
        const answer = classifyIdentityAnswer(
          key,
          bundle === null
            ? null
            : { signaturePubkeyB64: bundle.signature_pubkey, conversationLinkB64: bundle.conversation_link },
          (link, current) => wasm.verifyConversationLink(link, current),
          base64Decode,
          base64Encode,
        );
        void recordDirectoryAnswer(key, answer, now, idHex);
      }
    });
  });

  // ---- the on-screen check -------------------------------------------------
  const inFlight = new Set<string>();
  installPersonChecker((keyB64) => {
    if (accountId() === null || inFlight.has(keyB64)) return;
    if (keyB64 === ownPersonKeyB64(chats.identity())) return;
    if (!directoryCheckDueFor(keyB64)) return;
    const idHex = accountIdHexOfKey(keyB64);
    const idBytes = idHex === null ? null : bytesOfHex(idHex);
    if (idBytes === null) return;
    inFlight.add(keyB64);
    void (async () => {
      try {
        await recordDirectoryAttempt(keyB64);
        await new DiscoveryClient().identityFetch(idBytes);
      } catch (err) {
        // A network error or another status is not an answer: nothing recorded.
        // eslint-disable-next-line no-console
        console.warn("[person-status] directory check failed", err instanceof Error ? err.name : "unknown");
      } finally {
        inFlight.delete(keyB64);
      }
    })();
  });

  // ---- the statement sink ---------------------------------------------------
  installAccountDeletedSink(async (arrived) => {
    if (accountId() === null) return;
    const wasm = await loadWasm();
    if (!acceptAccountDeleted(wasm, arrived.plaintext, arrived.groupId, arrived.senderAccountKey)) return;
    const senderKeyB64 = base64Encode(arrived.senderAccountKey as Uint8Array);
    if (senderKeyB64 === ownPersonKeyB64(chats.identity())) return;
    const idHex = accountIdHexOfKey(senderKeyB64);
    const keys = new Set<string>([senderKeyB64]);
    if (idHex !== null) for (const k of heldKeysOf(idHex)) keys.add(k);
    for (const k of keys) await recordDeletionStatement(k, idHex ?? undefined);
    const entry = chats.get(arrived.groupIdB64);
    if (entry !== undefined && conversationKindOf(entry.group) === "group") {
      await recordRemoval(senderKeyB64, arrived.groupIdB64, "owed");
      await tryRemoval(entry, senderKeyB64);
    }
  });

  // ---- section 5: the removal ----------------------------------------------
  function ownRoleOf(entry: ActiveGroup): "owner" | "admin" | "member" | null {
    const own = ownPersonKeyB64(chats.identity());
    if (own === null) return null;
    try {
      const ownerRaw = entry.group.roleTableOwner() as Uint8Array | undefined;
      if (ownerRaw !== undefined && base64Encode(ownerRaw) === own) return "owner";
      const flat = entry.group.roleTableAdmins() as Uint8Array;
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        if (base64Encode(flat.slice(i, i + 32)) === own) return "admin";
      }
      return "member";
    } catch {
      return null;
    }
  }

  const removing = new Set<string>();
  async function tryRemoval(entry: ActiveGroup, deletedKeyB64: string): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null) return;
    if (readIsActive(entry.group) === false) return;
    const tag = `${entry.groupIdB64}:${deletedKeyB64}`;
    if (removing.has(tag)) return;
    let roster: { leafIndex: number; personKeyB64: string }[];
    let pendingRemoveLeaves: number[];
    let ownerKeyB64: string | undefined;
    try {
      roster = (entry.group.members() as unknown as RosterLeaf[]).map((m) => ({
        leafIndex: m.leafIndex,
        personKeyB64: base64Encode(personKeyOf(m)),
      }));
      pendingRemoveLeaves = (entry.group.pendingProposals() as ProposalRecordView[])
        .filter((r) => r.kind === "remove" && r.removeTargetLeaf !== undefined)
        .map((r) => r.removeTargetLeaf as number);
      const ownerRaw = entry.group.roleTableOwner() as Uint8Array | undefined;
      ownerKeyB64 = ownerRaw === undefined ? undefined : base64Encode(ownerRaw);
    } catch {
      return;
    }
    const plan = deletedRemovalPlan({
      kind: conversationKindOf(entry.group),
      ownRole: ownRoleOf(entry),
      ownerKeyB64,
      deletedKeyB64,
      roster,
      pendingRemoveLeaves,
      alreadyProposed: personStatusOf(deletedKeyB64)?.removals?.[entry.groupIdB64] === "proposed",
    });
    if (plan.kind === "gone") {
      await recordRemoval(deletedKeyB64, entry.groupIdB64, "proposed");
      return;
    }
    if (plan.kind !== "propose") return;
    removing.add(tag);
    try {
      const outcome = await contacts.removeMemberFromGroup(secrets, entry.group, plan.leafIndex);
      markOwnPublished(
        entry.groupIdB64,
        outcome.kind === "proposed" ? outcome.proposalMessageId : outcome.commitMessageId,
      );
      await recordRemoval(deletedKeyB64, entry.groupIdB64, "proposed");
    } catch (err) {
      // Owed: tried again later.
      // eslint-disable-next-line no-console
      console.warn("[person-status] removal not proposed", err instanceof Error ? err.name : "unknown");
    } finally {
      removing.delete(tag);
    }
  }

  async function retryOwedRemovals(): Promise<void> {
    for (const [key, rec] of personStatusEntries()) {
      if (!rec.statementVerified) continue;
      for (const [groupIdB64, state] of Object.entries(rec.removals ?? {})) {
        if (state !== "owed") continue;
        const entry = chats.get(groupIdB64);
        if (entry !== undefined) await tryRemoval(entry, key);
      }
    }
  }

  // ADR-0098 / UX section 3: a pending contact (ADR-0079) whose account was
  // deleted keeps its record, read as "Deleted account"; the messages held
  // for it are discarded, never sent.
  const heldDropped = new Set<string>();
  createEffect(() => {
    const account = accountId();
    if (account === null) return;
    for (const p of pendingContacts()) {
      if (heldDropped.has(p.accountIdHex) || !isDeletedAccount(p.signaturePubkeyB64)) continue;
      heldDropped.add(p.accountIdHex);
      void untrack(() => dropHeldForDeletedPending(account, p.accountIdHex));
    }
  });

  // An unanswered contact request from a deleted account goes, silently.
  createEffect(() => {
    if (accountId() === null) return;
    for (const req of mailbox.pending()) {
      if (isDeletedAccount(base64Encode(req.fromPubkey))) {
        untrack(() => mailbox.removePending(req.messageId));
      }
    }
  });

  // Groups load after the store: try again when the list changes, and on a
  // slow timer for a proposal the network refused.
  createEffect(() => {
    chats.groups();
    untrack(() => {
      if (accountId() !== null) void retryOwedRemovals();
    });
  });
  const timer = setInterval(() => {
    if (accountId() !== null) void retryOwedRemovals();
  }, OWED_RETRY_MS);

  onCleanup(() => {
    clearInterval(timer);
    installAccountDeletedSink(null);
    installIdentityAnswerObserver(null);
    installPersonChecker(null);
  });

  return null;
}
