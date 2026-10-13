// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 "Phase 2 revised", R.6 — the session-wide SUCCESSOR-CLAIM
 * runtime.
 *
 * Owns, for the signed-in account:
 *
 *   * the store of claim states, keyed on the base64 id of the OLD group
 *     (`group-closed-store.ts`, encrypted in IndexedDB by
 *     `group-closed-idb.ts`);
 *   * the RECEIVE rule (`successor-claim.ts` `claimVerdict`) run under the
 *     per-`(account, group)` serialising lock (C-62 sub-invariant 5), from
 *     both the foreground poll and the background receive: the first
 *     eligible claim in the journal's order wins, for good;
 *   * the re-judge of the claims kept while this device had no verdict on
 *     the reset;
 *   * the SEND side of a claim: record that this device is claiming, then
 *     publish into the OLD group and record the DS message id.
 *
 * Nothing here is rendered and no view reads the store (R.0): the lineage
 * (`lineage.ts`) is what turns an accepted claim into "the same
 * conversation", and that is not a surface either.
 */

import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { loadSupersededKeys } from "../contacts/local-store";
import { withRoutingRowLock } from "../push/group-routing";
import { loadWasm } from "../wasm";
import { useChats, type ActiveGroup } from "./ChatContext";
import { ChatClient, type IncomingChatEvent } from "./client";
import { conversationKindOf } from "./conversation-kind";
import { IdbGroupClosedBackend } from "./group-closed-idb";
import {
  GroupClosedStore,
  NO_CLAIMS,
  withAcceptedClaim,
  withPendingClaim,
  type PreparedSuccessor,
  type StoredGroupClosed,
} from "./group-closed-store";
import { readIsActive } from "./membership-ended";
import { ownPersonKeyB64, personKeysOf } from "./person-key";
import {
  claimVerdict,
  firstEligibleClaim,
  unrepairableBy,
  type ClaimVerdict,
  type SuccessorClaim,
} from "./successor-claim";

/** The receive-loop event this runtime consumes. */
export type GroupClosedEvent = Extract<IncomingChatEvent, { kind: "group-closed" }>;

/** The role table and roster facts a verdict reads, off the live group. */
interface ClaimFacts {
  readonly kind: ReturnType<typeof conversationKindOf>;
  readonly rosterPersonKeysB64: string[];
  readonly ownerKeyB64: string | undefined;
  readonly adminKeysB64: string[];
  readonly ownPersonKeyB64: string | null;
}

export interface GroupClosedApi {
  /** Bumped on any change. */
  readonly version: Accessor<number>;
  /** The claim state of an old group. Reactive. */
  stateOf(groupIdB64: string): StoredGroupClosed | undefined;
  /** base64 of the successor the accepted claim names, or `null`. Reactive. */
  successorOf(groupIdB64: string): string | null;
  /** Run an incoming claim through the receive rule and, on accept, record
   *  the conversation's successor. Never throws; `null` when there was
   *  nothing to decide. */
  receive(entry: ActiveGroup, event: GroupClosedEvent): Promise<ClaimVerdict | null>;
  /** Judge again the claims kept while this device had no verdict on the
   *  reset; returns the accepted claim, if any. Never throws. */
  rejudge(entry: ActiveGroup): Promise<SuccessorClaim | null>;
  /** R.6 step 2 — post this device's claim naming `successorGroupId` into
   *  the OLD group. The own-claim record is written BEFORE the publish, so
   *  a restart cannot found a second successor. Throws the send failure. */
  postClaim(entry: ActiveGroup, successorGroupId: Uint8Array): Promise<{ messageId: number }>;
  /** R.6 step 3 decided: `winner` is the conversation's successor (this
   *  device's own claim when it won); the own-claim record is cleared. */
  settle(entry: ActiveGroup, winner: SuccessorClaim): Promise<void>;
  /** The publish of an own claim failed before any row was stored: forget
   *  the own-claim record so the next trigger may try again. */
  abandonOwn(groupIdB64: string): Promise<void>;
  /** Record (or, with `null`, forget) the successor this device prepared
   *  for `groupIdB64` (R.6 step 1). */
  setPrepared(groupIdB64: string, prepared: PreparedSuccessor | null): Promise<void>;
  /** Forget a group's claim state (`Delete this chat`). */
  forget(groupIdB64: string): Promise<void>;
  /** Called with the old group whenever a claim was kept for a later
   *  verdict, so the runtime that can fetch identities runs the check the
   *  claim triggers. One handler; the last registration wins. */
  onPendingClaim(handler: ((entry: ActiveGroup) => void) | null): void;
  /** Called with the old group whenever a claim was ACCEPTED, so the
   *  lineage records the successor. One handler. */
  onAcceptedClaim(handler: ((entry: ActiveGroup, claim: SuccessorClaim) => void) | null): void;
}

const GroupClosedContext = createContext<GroupClosedApi>();

function factsOf(entry: ActiveGroup, own: string | null): ClaimFacts | null {
  try {
    const ownerRaw = entry.group.roleTableOwner() as Uint8Array | undefined;
    const flat = entry.group.roleTableAdmins() as Uint8Array;
    const adminKeysB64: string[] = [];
    for (let i = 0; i + 32 <= flat.length; i += 32) adminKeysB64.push(base64Encode(flat.slice(i, i + 32)));
    return {
      kind: conversationKindOf(entry.group),
      rosterPersonKeysB64: personKeysOf(entry.group),
      ownerKeyB64: ownerRaw === undefined ? undefined : base64Encode(ownerRaw),
      adminKeysB64,
      ownPersonKeyB64: own,
    };
  } catch {
    return null;
  }
}

export function GroupClosedProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();

  const [version, setVersion] = createSignal(0);
  const bump = (): void => {
    setVersion((v) => v + 1);
  };

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const store = new GroupClosedStore(new IdbGroupClosedBackend(getDeviceKey), (err) => {
    // eslint-disable-next-line no-console
    console.warn("[successor-claim] store persist failed", err);
  });
  const unsubscribeStore = store.subscribe(bump);
  let opened: Promise<boolean> = Promise.resolve(true);
  let pendingHandler: ((entry: ActiveGroup) => void) | null = null;
  let acceptedHandler: ((entry: ActiveGroup, claim: SuccessorClaim) => void) | null = null;

  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const account = accountId();
      if (account === null) return;
      await chats.persistSnapshot(account);
    },
  });

  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      if (account === null) {
        store.close();
        opened = Promise.resolve(true);
        return;
      }
      opened = store.open(account);
    });
  });
  onCleanup(() => {
    unsubscribeStore();
  });

  /** The verdict inputs that depend on this device's accepted resets. */
  function judgeInputs(entry: ActiveGroup): {
    facts: ClaimFacts;
    superseded: Map<string, string>;
    unrepairableKeyB64: string | null;
  } | null {
    const facts = factsOf(entry, ownPersonKeyB64(chats.identity()));
    if (facts === null) return null;
    const superseded = loadSupersededKeys();
    return {
      facts,
      superseded,
      unrepairableKeyB64: unrepairableBy({
        kind: facts.kind,
        rosterPersonKeysB64: facts.rosterPersonKeysB64,
        ownPersonKeyB64: facts.ownPersonKeyB64,
        ownerKeyB64: facts.ownerKeyB64,
        supersededKeys: superseded,
      }),
    };
  }

  function notifyAccepted(entry: ActiveGroup, claim: SuccessorClaim): void {
    try {
      acceptedHandler?.(entry, claim);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[successor-claim] accepted-claim handler failed", err);
    }
  }

  const api: GroupClosedApi = {
    version,
    stateOf(groupIdB64) {
      version();
      return store.get(groupIdB64);
    },
    successorOf(groupIdB64) {
      version();
      return store.get(groupIdB64)?.accepted?.successorGroupIdB64 ?? null;
    },
    onPendingClaim(handler) {
      pendingHandler = handler;
    },
    onAcceptedClaim(handler) {
      acceptedHandler = handler;
    },
    async receive(entry, event) {
      const account = accountId();
      if (account === null) return null;
      const incoming = event.groupClosed;
      // A malformed envelope, one naming no successor, and one whose sender
      // did not resolve are not claims.
      if (incoming === undefined || incoming.successorGroupIdB64 === null) return null;
      if (event.senderKey === undefined) return null;
      const successorGroupIdB64 = incoming.successorGroupIdB64;
      const senderKeyB64 = event.senderKey;
      const groupIdB64 = entry.groupIdB64;
      try {
        return await withRoutingRowLock(account, groupIdB64, async () => {
          const readable = await opened;
          // An unreadable store cannot say which claim was first: decide
          // nothing rather than accept a later one.
          if (!readable) return null;
          const inputs = judgeInputs(entry);
          if (inputs === null) return null;
          const state = store.get(groupIdB64) ?? NO_CLAIMS;
          const claim: SuccessorClaim = {
            messageId: event.messageId,
            successorGroupIdB64,
            senderKeyB64,
            senderWasAdmin: inputs.facts.adminKeysB64.includes(senderKeyB64),
            adminKeysAtReceiptB64: inputs.facts.adminKeysB64,
          };
          const verdict = claimVerdict({
            kind: inputs.facts.kind,
            groupIdB64,
            claim,
            held: state.accepted,
            unrepairableKeyB64: inputs.unrepairableKeyB64,
            supersededKeys: inputs.superseded,
            survivingKeyB64: inputs.facts.ownPersonKeyB64,
          });
          if (verdict === "accept") {
            await store.set(account, groupIdB64, withAcceptedClaim(state, claim));
            notifyAccepted(entry, claim);
          } else if (verdict === "keep-pending") {
            await store.set(account, groupIdB64, withPendingClaim(state, claim));
            try {
              pendingHandler?.(entry);
            } catch (err) {
              // eslint-disable-next-line no-console
              console.warn("[successor-claim] pending-claim handler failed", err);
            }
          }
          return verdict;
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[successor-claim] receive failed", err);
        return null;
      }
    },
    async rejudge(entry) {
      const account = accountId();
      if (account === null) return null;
      const groupIdB64 = entry.groupIdB64;
      try {
        return await withRoutingRowLock(account, groupIdB64, async () => {
          const readable = await opened;
          if (!readable) return null;
          const state = store.get(groupIdB64);
          if (state === undefined || state.pending.length === 0) return state?.accepted ?? null;
          const inputs = judgeInputs(entry);
          if (inputs === null || inputs.unrepairableKeyB64 === null) return state.accepted;
          const first = firstEligibleClaim({
            kind: inputs.facts.kind,
            groupIdB64,
            claims: state.pending,
            unrepairableKeyB64: inputs.unrepairableKeyB64,
            supersededKeys: inputs.superseded,
            survivingKeyB64: inputs.facts.ownPersonKeyB64,
          });
          const held = state.accepted;
          const winner =
            first !== null && (held === null || first.messageId < held.messageId) ? first : held;
          // The pending list is spent either way: every kept claim was judged.
          await store.set(account, groupIdB64, { ...state, accepted: winner, pending: [] });
          if (winner !== null && winner !== held) notifyAccepted(entry, winner);
          return winner;
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[successor-claim] re-judge failed", err);
        return null;
      }
    },
    async postClaim(entry, successorGroupId) {
      const account = accountId();
      const secrets = auth.identitySecrets();
      if (account === null || secrets === null || chats.identity() === null) {
        throw new Error("Sign in again.");
      }
      if (readIsActive(entry.group) === false) throw new Error("You are no longer in this group.");
      const successorGroupIdB64 = base64Encode(successorGroupId);
      await opened;
      const state = store.get(entry.groupIdB64) ?? NO_CLAIMS;
      await store.set(account, entry.groupIdB64, {
        ...state,
        own: { successorGroupIdB64, messageId: null },
      });
      const wasm = await loadWasm();
      const envelope = wasm.composeGroupClosedEnvelope(BigInt(Date.now()), successorGroupId);
      // The claim is the one frame posted into the superseded session.
      const published = await publisher.broadcastControl(secrets, entry.group, envelope, {
        intoSupersededSession: true,
      });
      const after = store.get(entry.groupIdB64) ?? NO_CLAIMS;
      await store.set(account, entry.groupIdB64, {
        ...after,
        own: { successorGroupIdB64, messageId: published.messageId },
      });
      return published;
    },
    async settle(entry, winner) {
      const account = accountId();
      if (account === null) return;
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await opened;
        const state = store.get(entry.groupIdB64) ?? NO_CLAIMS;
        const held = state.accepted;
        const next = held !== null && held.messageId <= winner.messageId ? held : winner;
        await store.set(account, entry.groupIdB64, { ...state, accepted: next, pending: [], own: null });
        if (next !== held) notifyAccepted(entry, next);
      });
    },
    async abandonOwn(groupIdB64) {
      const account = accountId();
      if (account === null) return;
      const state = store.get(groupIdB64);
      if (state === undefined || state.own === null) return;
      await store.set(account, groupIdB64, { ...state, own: null });
    },
    async setPrepared(groupIdB64, prepared) {
      const account = accountId();
      if (account === null) return;
      await opened;
      const state = store.get(groupIdB64) ?? NO_CLAIMS;
      await store.set(account, groupIdB64, { ...state, prepared });
    },
    async forget(groupIdB64) {
      const account = accountId();
      if (account === null) return;
      await store.remove(account, groupIdB64);
    },
  };

  return <GroupClosedContext.Provider value={api}>{props.children}</GroupClosedContext.Provider>;
}

export function useGroupClosed(): GroupClosedApi {
  const ctx = useContext(GroupClosedContext);
  if (ctx === undefined) throw new Error("useGroupClosed() called outside <GroupClosedProvider>");
  return ctx;
}
