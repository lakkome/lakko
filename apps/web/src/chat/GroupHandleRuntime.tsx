// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 sections 3, 4 and 5 — the whole-session part of `@name`s, mounted
 * once beside the other session runtimes in `App.tsx`. Renders nothing.
 *
 * - **Owed writes** (`handle-owed.ts`): a change of a group's `@name` or of
 *   who can join that did not land is retried, one at a time, every
 *   {@link OWED_RETRY_MS} and when the browser comes back online, until it
 *   lands or is refused for good.
 * - **Lease renewal**: a group or channel this account owns is renewed when
 *   fewer than 90 days of its 180-day lease are left, at most 20 per session,
 *   with no action from the person.
 * - **Republish**: when the owner's knock key moves to a new version
 *   (an admin left), a public group's record is published again with the
 *   new key.
 *
 * Only the owner holds the handle key (it comes from the journal root), so
 * every step here is skipped for a group or channel this account does not
 * own.
 */
import { createEffect, onCleanup, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { channelIdFromHex } from "../channels/composer";
import { knockKeyFor } from "../contacts/client";
import { channelsActions, channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { useChats, type ActiveGroup } from "./ChatContext";
import { useGroupNames } from "./GroupNameContext";
import { ownsGroup } from "./group-owner";
import { reuploadedPhoto } from "./GroupInfoExtras";
import { installRotationHandleRekeys } from "../ds/rotation-takeover";
import type { StoredConversationHandle } from "./group-settings-envelope";
import {
  claimGroupHandle,
  rekeyGroupHandle,
  handleNeedsRenewal,
  HANDLE_RENEWALS_PER_SESSION,
  replaceGroupHandle,
  updateChannelHandle,
  updateGroupHandle,
  type PublicGroupFacts,
} from "./group-handle";
import {
  clearOwedRekey,
  dropOwedHandle,
  handleWriteFor,
  noteHandleWrite,
  openHandleOwed,
  owedHandleFor,
  owedHandles,
  owedOutcomeOf,
  recordOwedHandle,
  type OwedHandle,
  type OwedOutcome,
} from "./handle-owed";
import { personKeyOf, type RosterLeaf } from "./person-key";

/** How often owed writes are tried again and the leases looked at. */
export const OWED_RETRY_MS = 60_000;

/** The number of people in a group (its members' account keys). */
function peopleCount(group: ActiveGroup["group"]): number {
  try {
    const leaves = group.members() as unknown as RosterLeaf[];
    return new Set(leaves.map((m) => base64Encode(personKeyOf(m)))).size;
  } catch {
    return 1;
  }
}


export function GroupHandleRuntime(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const groupNames = useGroupNames();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  /** Whether this account owns the group (only the owner holds its key). */
  function owns(entry: ActiveGroup): boolean {
    return ownsGroup(entry.group, [
      chats.conversationAccountIdentity()?.signaturePublicKey,
      chats.accountIdentity()?.signaturePublicKey,
    ]);
  }

  /** Whether this account owns the channel (not a co-publisher). */
  function ownsChannel(idHex: string): boolean {
    return channelsState.keysByChannel().has(idHex) && channelsActions.coPublisherGroupIdOf(idHex) === undefined;
  }

  async function groupFacts(
    wasm: Awaited<ReturnType<typeof loadWasm>>,
    account: WasmIdentity,
    entry: ActiveGroup,
  ): Promise<PublicGroupFacts | { readonly missing: string }> {
    const name = (groupNames.nameOf(entry.groupIdB64) ?? "").trim();
    // A title built from member names is never published.
    if (name.length === 0) return { missing: "needs-name" };
    const knock = await knockKeyFor(wasm, account, entry.group, chats.conversationAccountIdentity());
    if (knock === null) return { missing: "no-knock-key" };
    return {
      name,
      memberCount: peopleCount(entry.group),
      knockPublicKey: knock.publicKey,
      knockVersion: knock.version,
      avatar: groupNames.photoOf(entry.groupIdB64)?.avatar ?? null,
    };
  }

  async function runGroup(owed: OwedHandle): Promise<OwedOutcome> {
    let o = owed;
    // A republish reads the name now: nothing to publish for a group that
    // went private or holds no name.
    if (o.reason === "republish") {
      const held = groupNames.handleOf(o.id);
      if (held === undefined || !held.isPublic) return "done";
      o = { ...o, handle: held.handle, isPublic: true };
    }
    const entry = chats.get(o.id);
    if (entry === undefined) return { dropped: "no-group" };
    if (!owns(entry)) return { dropped: "not-owner" };
    const account = chats.accountIdentity();
    if (account === null) return "retry";
    const wasm = await loadWasm();
    // ADR-0087 sections 3 and 5: after a rotation the name moves to the new
    // root's handle key first; what else is owed then runs under it.
    if (owed.rekeyFrom !== undefined) {
      const held = groupNames.handleOf(o.id);
      if (held === undefined) {
        await clearOwedRekey("group", o.id);
        if (o.reason === "rekey") return "done";
      } else {
        const old = ownIdentityByKey(owed.rekeyFrom);
        if (old === null) return { dropped: "no-old-key" };
        let next: StoredConversationHandle;
        try {
          next = await rekeyGroupHandle({
            wasm,
            oldIdentity: old,
            accountIdentity: account,
            groupId: entry.group.groupId,
            handle: held.handle,
            isPublic: held.isPublic,
          });
        } catch (err) {
          // Refused for good: the name may already be under the new key (an
          // answer lost on the way back); an update under the new key tells.
          if (owedOutcomeOf(err) === "retry") throw err;
          next = await updateGroupHandle({
            wasm,
            accountIdentity: account,
            groupId: entry.group.groupId,
            handle: held.handle,
            isPublic: held.isPublic,
            facts: held.isPublic ? await factsOrThrow(wasm, account, entry) : null,
          });
        }
        await groupNames.setHandle(entry, next);
        await clearOwedRekey("group", o.id);
        if (o.reason === "rekey") return "done";
      }
    }
    let facts: PublicGroupFacts | null = null;
    if (o.isPublic) {
      const f = await groupFacts(wasm, account, entry);
      if ("missing" in f) return f.missing === "needs-name" ? { dropped: "needs-name" } : "retry";
      facts = f;
    }
    const before = groupNames.handleOf(o.id);
    const common = { wasm, accountIdentity: account, groupId: entry.group.groupId, isPublic: o.isPublic, facts };
    const next =
      o.op === "claim"
        ? await claimGroupHandle({ ...common, handle: o.handle })
        : o.op === "replace" && o.oldHandle !== null
          ? await replaceGroupHandle({ ...common, oldHandle: o.oldHandle, newHandle: o.handle })
          : await updateGroupHandle({ ...common, handle: o.handle });
    await groupNames.setHandle(entry, next);
    await noteHandleWrite({
      kind: "group",
      id: o.id,
      handle: next.handle,
      isPublic: next.isPublic,
      lastWriteMs: Date.now(),
      leaseUntilMs: 0,
    });
    // Section 18.3 — going private sends the photo again under a fresh key.
    if (before?.isPublic === true && !o.isPublic) {
      try {
        const fresh = await reuploadedPhoto(groupNames.photoOf(o.id)?.avatar ?? null);
        if (fresh !== null) await groupNames.setPhoto(entry, fresh);
      } catch {
        /* the old photo stays until the next change */
      }
    }
    return "done";
  }

  /** The own account identity (current or a kept predecessor) whose
   *  signature key is `keyB64`. */
  function ownIdentityByKey(keyB64: string): WasmIdentity | null {
    const all = [chats.accountIdentity(), ...chats.ownPredecessorIdentities()];
    return all.find((i) => i !== null && base64Encode(i.signaturePublicKey) === keyB64) ?? null;
  }

  async function factsOrThrow(
    wasm: Awaited<ReturnType<typeof loadWasm>>,
    account: WasmIdentity,
    entry: ActiveGroup,
  ): Promise<PublicGroupFacts> {
    const f = await groupFacts(wasm, account, entry);
    if ("missing" in f) throw new Error(f.missing);
    return f;
  }

  // ADR-0087 sections 3 and 5 — the rotation step: one owed re-key per name
  // an owned group holds, recorded durably; the loop above runs them.
  onCleanup(
    installRotationHandleRekeys(async (oldAccountKeyB64) => {
      const acct = accountId();
      if (acct === null) throw new Error("no session");
      await openHandleOwed(acct);
      let recorded = 0;
      for (const entry of chats.groups()) {
        const held = groupNames.handleOf(entry.groupIdB64);
        if (held === undefined) continue;
        if (
          !ownsGroup(entry.group, [
            chats.conversationAccountIdentity()?.signaturePublicKey,
            chats.accountIdentity()?.signaturePublicKey,
            base64Decode(oldAccountKeyB64),
          ])
        ) {
          continue;
        }
        await recordOwedHandle({
          kind: "group",
          id: entry.groupIdB64,
          op: "update",
          handle: held.handle,
          oldHandle: null,
          isPublic: held.isPublic,
          sinceMs: Date.now(),
          reason: "rekey",
          rekeyFrom: oldAccountKeyB64,
        });
        recorded += 1;
      }
      void tick(acct);
      return recorded;
    }),
  );

  async function runChannel(owed: OwedHandle): Promise<OwedOutcome> {
    let o = owed;
    if (!ownsChannel(o.id)) return { dropped: "not-owner" };
    const owner = chats.conversationAccountIdentity();
    if (owner === null) return "retry";
    const channel = channelsState.channels().find((c) => c.id === o.id);
    if (channel === undefined) return { dropped: "no-channel" };
    // A republish (a name or description change) reads the name now: a
    // channel that holds none has nothing to publish.
    if (o.reason === "republish" && o.handle.length === 0) {
      const held = (channel.localHandle || handleWriteFor("channel", o.id)?.handle || "").replace(/^@/, "");
      if (held.length === 0) return "done";
      o = { ...o, handle: held };
    }
    const meta = channelsState.metaByChannel().get(o.id);
    const wasm = await loadWasm();
    const written = await updateChannelHandle({
      wasm,
      ownerIdentity: owner,
      channelId: channelIdFromHex(o.id),
      handle: o.handle,
      facts: {
        name: (meta?.name ?? channel.localName).trim(),
        description: (meta?.description ?? channel.localDescription).trim(),
      },
    });
    await noteHandleWrite({
      kind: "channel",
      id: o.id,
      handle: written.handle,
      isPublic: true,
      lastWriteMs: Date.now(),
      leaseUntilMs: written.leaseUntilMs,
    });
    return "done";
  }

  async function runOne(o: OwedHandle): Promise<void> {
    let outcome: OwedOutcome;
    try {
      outcome = o.kind === "group" ? await runGroup(o) : await runChannel(o);
    } catch (err) {
      outcome = owedOutcomeOf(err);
    }
    // A newer change recorded while this one ran is kept.
    const now = owedHandleFor(o.kind, o.id);
    const same =
      now !== undefined && now.op === o.op && now.handle === o.handle && now.isPublic === o.isPublic;
    if (outcome === "done") {
      if (same) await dropOwedHandle(o.kind, o.id);
      return;
    }
    if (outcome === "retry") return;
    // eslint-disable-next-line no-console
    console.warn("[group-handle] an owed @name write was refused for good", outcome.dropped);
    if (same) await dropOwedHandle(o.kind, o.id, o.reason === "change" ? outcome.dropped : undefined);
  }

  let renewed = 0;

  /** Record a renewal for every held name of this account close to lapsing. */
  async function scanLeases(): Promise<void> {
    const now = Date.now();
    for (const entry of chats.groups()) {
      if (renewed >= HANDLE_RENEWALS_PER_SESSION) return;
      const held = groupNames.handleOf(entry.groupIdB64);
      if (held === undefined || owedHandleFor("group", entry.groupIdB64) !== undefined) continue;
      if (!owns(entry)) continue;
      const write = handleWriteFor("group", entry.groupIdB64);
      // The envelope's stamp is the last write by any device of the owner.
      const last = Math.max(Number(held.updatedAtMs), write?.lastWriteMs ?? 0);
      if (!handleNeedsRenewal(last, write?.leaseUntilMs ?? 0, now)) continue;
      renewed += 1;
      await recordOwedHandle({
        kind: "group",
        id: entry.groupIdB64,
        op: "update",
        handle: held.handle,
        oldHandle: null,
        isPublic: held.isPublic,
        sinceMs: now,
        reason: "renew",
      });
    }
    for (const channel of channelsState.channels()) {
      if (renewed >= HANDLE_RENEWALS_PER_SESSION) return;
      const handle = channel.localHandle;
      if (handle === undefined || handle.length === 0 || !ownsChannel(channel.id)) continue;
      if (owedHandleFor("channel", channel.id) !== undefined) continue;
      const write = handleWriteFor("channel", channel.id);
      // Claimed at creation when this device never wrote it since.
      const last = write?.lastWriteMs ?? channel.addedAtMs;
      if (!handleNeedsRenewal(last, write?.leaseUntilMs ?? 0, now)) continue;
      renewed += 1;
      await recordOwedHandle({
        kind: "channel",
        id: channel.id,
        op: "update",
        handle,
        oldHandle: null,
        isPublic: true,
        sinceMs: now,
        reason: "renew",
      });
    }
  }

  let running = false;
  let again = false;
  async function tick(account: string): Promise<void> {
    if (running) {
      again = true;
      return;
    }
    running = true;
    try {
      do {
        again = false;
        await openHandleOwed(account);
        if (accountId() !== account) return;
        await scanLeases();
        for (const o of [...owedHandles()]) {
          if (accountId() !== account) return;
          await runOne(o);
        }
      } while (again && accountId() === account);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[group-handle] owed writes not run", err instanceof Error ? err.name : "");
    } finally {
      running = false;
    }
  }

  createEffect(() => {
    const account = accountId();
    if (account === null) return;
    renewed = 0;
    const first = setTimeout(() => void tick(account), 5_000);
    const timer = setInterval(() => void tick(account), OWED_RETRY_MS);
    const online = (): void => void tick(account);
    window.addEventListener("online", online);
    onCleanup(() => {
      clearTimeout(first);
      clearInterval(timer);
      window.removeEventListener("online", online);
    });
  });

  return null;
}
