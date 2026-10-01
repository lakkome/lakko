// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.5a point 4 — the glue between a recorded own request
 * (`chat/membership-retry.ts`) and the propose functions of
 * `contacts/client.ts`.
 *
 * Two hosts drive a re-request: the chat view for the open chat, and the
 * background receive loop for every other one. Both read the group the same
 * way ({@link readIntentSnapshot}) and re-issue the same way
 * ({@link reissueOwnRequest}), so the two cannot drift.
 *
 * A re-request is **re-derived from the intent against the current epoch**,
 * never the old bytes re-sent: an add fetches the person's KeyPackages again,
 * a device admission fetches that device's current KeyPackage, a removal
 * resolves the subject's CURRENT leaf from its key (OpenMLS reuses a vacated
 * leaf), a role change hands the delta to `setMemberAdmin` /
 * `transferGroupOwnership`, which recompute the full table from the table as
 * it is now. Every call goes through the same public method the user path
 * uses, so the per-group MLS lock, the ADR-0040 outbox and the ADR-0057
 * registration steps are the user path's own, and the method records the
 * request again at the epoch it was re-proposed at.
 */

import { base64Decode, base64Encode } from "../auth/account-id";
import type { IdentitySecrets } from "../auth/AuthContext";
import type { ContactsClient, MembershipOutcome } from "../contacts/client";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import type { IntentSnapshot, OwnRequest, RosterMemberView } from "./membership-retry";
import { proposalKey, type ProposalRecordView } from "./pending-requests";

/** One roster leaf as the core reports it. */
interface RosterLeaf {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
  readonly accountKey?: Uint8Array;
}

/** The roster with each leaf's account key, or `null` on a torn read. */
export function readRosterViews(group: WasmGroup): RosterMemberView[] | null {
  try {
    return (group.members() as unknown as RosterLeaf[]).map((m) => ({
      leafIndex: m.leafIndex,
      signatureKeyB64: base64Encode(m.signaturePublicKey),
      ...(m.accountKey !== undefined && m.accountKey.length === 32
        ? { accountKeyB64: base64Encode(m.accountKey) }
        : {}),
    }));
  } catch {
    return null;
  }
}

/**
 * The group as the superseded verdict reads it, in ONE pass: the merge that
 * empties the pending set is the merge that moves the roster and the epoch, so
 * reading them at different moments would compare two different groups.
 *
 * `membershipEnded` is the caller's, because the chat view and the background
 * loop each already hold the authoritative read of it.
 */
export function readIntentSnapshot(
  group: WasmGroup,
  membershipEnded: boolean,
): IntentSnapshot {
  const roster = readRosterViews(group);
  let pendingKeys: string[] = [];
  let pendingCount = 0;
  try {
    const records = group.pendingProposals() as unknown as ProposalRecordView[];
    pendingCount = records.length;
    pendingKeys = records.filter((r) => r.stored).map(proposalKey);
  } catch {
    /* an unreadable pending set reads as empty */
  }
  let roleTable: IntentSnapshot["roleTable"] = null;
  try {
    const owner = group.roleTableOwner();
    if (owner !== undefined) {
      const flat = group.roleTableAdmins();
      const admins: string[] = [];
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        admins.push(base64Encode(flat.slice(i, i + 32)));
      }
      roleTable = { ownerKeyB64: base64Encode(owner), adminKeysB64: admins };
    }
  } catch {
    roleTable = null;
  }
  let retainedJoinKeys: string[] | null;
  try {
    const flat = group.pendingExternalJoins();
    retainedJoinKeys = [];
    for (let i = 0; i + 32 <= flat.length; i += 32) {
      retainedJoinKeys.push(base64Encode(flat.slice(i, i + 32)));
    }
  } catch {
    retainedJoinKeys = null;
  }
  let epoch: bigint;
  try {
    epoch = group.epoch;
  } catch {
    // An unreadable epoch can never look like a new commit.
    epoch = 0n;
  }
  return {
    roster,
    pendingKeys,
    pendingCount,
    roleTable,
    membershipEnded,
    retainedJoinKeys,
    epoch,
  };
}

/** What the re-issue needs from its host. */
export interface ReissueHost {
  readonly contacts: Pick<
    ContactsClient,
    | "addMemberToGroup"
    | "removeMemberFromGroup"
    | "approveExternalJoin"
    | "setMemberAdmin"
    | "transferGroupOwnership"
    | "readmitDeviceByKey"
    | "retireDeviceFromGroup"
  >;
  readonly identity: IdentitySecrets;
  readonly group: WasmGroup;
  /** Echo suppression: our own published row must not come back as a peer's. */
  markOwnPublished(messageId: number): void;
  /** The chat view's quiet-window observation of an own request; optional
   *  because the background loop renders nothing. */
  noteOwnProposal?(observationKey: string, transferToB64?: string): void;
}

function publishedId(out: MembershipOutcome): number {
  return out.kind === "committed" ? out.commitMessageId : out.proposalMessageId;
}

/**
 * Re-issue one recorded own request through the user path.
 *
 * Resolves without doing anything when the request names a subject whose leaf
 * cannot be found right now (a torn read between the verdict and this call):
 * any fallback leaf names somebody, and for a removal that somebody is a real
 * person. The record stays, stamped at the current epoch, and the next merged
 * commit re-evaluates it.
 */
export async function reissueOwnRequest(
  host: ReissueHost,
  request: OwnRequest,
): Promise<void> {
  const { contacts, identity, group } = host;
  switch (request.kind) {
    case "add": {
      const out = await contacts.addMemberToGroup(
        identity,
        group,
        base64Decode(request.subjectAccountIdB64),
        base64Decode(request.subjectKeyB64),
      );
      host.markOwnPublished(publishedId(out));
      if (out.kind === "proposed") host.noteOwnProposal?.(`add:${request.subjectKeyB64}`);
      return;
    }
    case "remove":
    case "leave": {
      const roster = readRosterViews(group) ?? [];
      const leaf = roster.find(
        (m) =>
          m.signatureKeyB64 === request.subjectKeyB64 ||
          m.accountKeyB64 === request.subjectKeyB64,
      )?.leafIndex;
      if (leaf === undefined) return;
      // `removeMemberFromGroup` removes the PERSON — every leaf of the account
      // the named leaf belongs to — so one leaf is enough to name them.
      const out = await contacts.removeMemberFromGroup(identity, group, leaf);
      host.markOwnPublished(publishedId(out));
      if (out.kind === "proposed") host.noteOwnProposal?.(`remove:${leaf}`);
      return;
    }
    case "approve-join": {
      const out = await contacts.approveExternalJoin(
        identity,
        group,
        base64Decode(request.subjectKeyB64),
      );
      host.markOwnPublished(out.proposalMessageId);
      host.noteOwnProposal?.(`add:${request.subjectKeyB64}`);
      return;
    }
    case "set-admin": {
      const out = await contacts.setMemberAdmin(
        identity,
        group,
        base64Decode(request.subjectKeyB64),
        request.makeAdmin,
      );
      host.markOwnPublished(publishedId(out));
      if (out.kind === "proposed") host.noteOwnProposal?.("role-change");
      return;
    }
    case "transfer-ownership": {
      const out = await contacts.transferGroupOwnership(
        identity,
        group,
        base64Decode(request.subjectKeyB64),
        request.outgoing,
      );
      host.markOwnPublished(publishedId(out));
      if (out.kind === "proposed") {
        host.noteOwnProposal?.("role-change", request.subjectKeyB64);
      }
      return;
    }
    case "admit-device": {
      const out = await contacts.readmitDeviceByKey(
        identity,
        group,
        base64Decode(request.subjectAccountIdB64),
        base64Decode(request.subjectKeyB64),
      );
      if (out === null) return;
      host.markOwnPublished(publishedId(out));
      if (out.kind === "proposed") host.noteOwnProposal?.(`add:${request.subjectKeyB64}`);
      return;
    }
    case "retire-device": {
      const out = await contacts.retireDeviceFromGroup(
        identity,
        group,
        base64Decode(request.subjectKeyB64),
      );
      if (out === null) return;
      host.markOwnPublished(publishedId(out));
      return;
    }
  }
}
