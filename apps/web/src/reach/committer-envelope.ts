// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - the committer-delegation envelope of an Add this device
 * proposed: posting it, and posting a fresh one while the Add waits.
 *
 * Under ADR-0045 whoever sweeps the Add may hold no key that reaches the added
 * person, so the proposer hands every committer candidate a single-use
 * delegation. A delegation lives 23 hours and a proposal has no deadline, so an
 * Add that waits longer loses its way to the person unless the proposer sends
 * again. When to send is the core's rule (`committerEnvelopeDue`), never decided
 * here: this module reads what was last sent from the add's own-request record
 * (`chat/membership-retry.ts`), the rule says due or not, and a due envelope is
 * built exactly as the first one was.
 *
 * Only the device that proposed holds an own-request record, so only that
 * device re-sends. The check runs at the tail of a poll, in the page; the
 * service worker never sends.
 */
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import { base64Decode } from "../auth/account-id";
import {
  loadOwnRequests,
  ownAddIsPending,
  retryScopeKey,
  type CommitterEnvelopeSend,
  type IntentSnapshot,
} from "../chat/membership-retry";
import { personKeysHex, resolveAccountKey } from "../contacts/address-resolver";
import { journalHex } from "../ds/access";
import { loadWasm, type Wasm } from "../wasm";
import { DELEGATION_TTL_MS, committerCandidates } from "./delegation-rules";
import { openCommitterEnvelope } from "./found-grant";
import { personKeysOf, verifiedTargetOfGrant } from "./mailbox-write";
import { reachSession } from "./reach-service";
import { bestGrantForPerson } from "./received-grants";

/** What a send came to. */
export type EnvelopeSendResult =
  /** Published and noted. */
  | "sent"
  /** Nothing to build one from: no grant held for the person and no open
   *  envelope recorded, a grant whose binding does not verify, no candidate. */
  | "nothing-to-send"
  /** Built, but the publish failed. The next poll tail tries again. */
  | "failed";

/** The group as the envelope reads it. */
type CommitterGroup = Pick<
  WasmGroup,
  "groupId" | "roleTableOwner" | "roleTableAdmins" | "members" | "committerRank"
>;

interface RosterLeaf {
  readonly signaturePublicKey: Uint8Array;
  readonly accountKey?: Uint8Array;
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function concatHexKeys(keysHex: readonly string[]): Uint8Array {
  const out = new Uint8Array(keysHex.length * 32);
  keysHex.forEach((hex, i) => out.set(hexBytes(hex), i * 32));
  return out;
}

/**
 * The committer candidates of `group` now, as the role table names them (the
 * owner, then the admins by rank), in hex, capped at the core's maximum.
 */
export async function committerCandidateHexes(group: CommitterGroup): Promise<string[]> {
  const wasm = await loadWasm();
  let ownerHex: string | null = null;
  const adminHexes: string[] = [];
  const owner = group.roleTableOwner();
  if (owner !== undefined && owner !== null) ownerHex = journalHex(owner);
  const flat = group.roleTableAdmins();
  for (let i = 0; i + 32 <= flat.length; i += 32) adminHexes.push(journalHex(flat.slice(i, i + 32)));
  return committerCandidates(
    ownerHex,
    adminHexes,
    // The role table names ACCOUNT keys; `committerRank` ranks a LEAF key
    // (ADR-0050: an account key holds no leaf on a bound device), so the
    // account is ranked through any of its leaves, all of which hold the
    // account's rank (section 2.5a item 6).
    (hex) => {
      const leaf = (group.members() as unknown as RosterLeaf[]).find(
        (m) => journalHex(m.accountKey ?? m.signaturePublicKey) === hex,
      );
      return leaf === undefined ? undefined : group.committerRank(leaf.signaturePublicKey);
    },
    wasm.maxCommitterDelegations(),
  );
}

/** The core's rule over a stored send. */
export function committerEnvelopeDueNow(
  wasm: Pick<Wasm, "committerEnvelopeDue">,
  last: CommitterEnvelopeSend | undefined,
  candidatesHex: readonly string[],
  nowMs: number,
): boolean {
  return wasm.committerEnvelopeDue(
    last === undefined ? undefined : BigInt(last.sentAtMs),
    last?.signed ?? false,
    concatHexKeys(last?.coveredHex ?? []),
    concatHexKeys(candidatesHex),
    BigInt(nowMs),
  );
}

/** The envelope signed with a held grant, one delegation per candidate, or
 *  `null` (no candidate, or the grant's binding does not verify under the
 *  added account key). */
async function signedEnvelope(
  candidatesHex: readonly string[],
  addedAccountKey: Uint8Array,
  grantWireB64: string,
): Promise<Uint8Array | null> {
  if (candidatesHex.length === 0) return null;
  const wasm = await loadWasm();
  const grant = wasm.decodeReachGrant(base64Decode(grantWireB64));
  // ADR-0078 - the added person's mailbox is the one the grant's binding
  // names, verified against the account key the Add adds (and its proven
  // chain); the envelope carries the binding so the committer can verify
  // it too. A binding that does not verify posts nothing.
  const target = await verifiedTargetOfGrant(grant, personKeysOf(addedAccountKey));
  const binding = grant.binding;
  const seed = grant.seed;
  grant.free();
  if (target === null) {
    seed.fill(0);
    // eslint-disable-next-line no-console
    console.warn("ADR-0078: committer delegations not posted; the kept grant's mailbox binding did not verify");
    return null;
  }
  const mailbox = target.address;
  const expires = BigInt(Date.now() + DELEGATION_TTL_MS);
  const blobs: Uint8Array[] = [];
  try {
    for (const hex of candidatesHex) {
      blobs.push(
        wasm.signDelegationBlob(
          seed,
          mailbox,
          // The committer as the role table names it, resolved: a committer
          // whose account rotated signs with its current key (it also tries
          // the key named here unresolved, for a proposer that has not
          // observed the rotation).
          resolveAccountKey(hexBytes(hex)),
          expires,
          crypto.getRandomValues(new Uint8Array(16)),
        ),
      );
    }
  } finally {
    seed.fill(0);
  }
  const env = new wasm.CommitterDelegationEnvelope(addedAccountKey, binding, blobs);
  try {
    return wasm.composeCommitterDelegationEnvelope(env);
  } finally {
    env.free();
  }
}

/**
 * Post the envelope for an Add this device proposed: one delegation per
 * committer candidate (the owner, then the admins by rank, at most
 * `maxCommitterDelegations()`), each signed with the grant this account keeps
 * for the added person (a grant a search found them through is kept before the
 * add, `reach/found-grant.ts`) and naming the candidate's account key.
 *
 * With no grant held, a person found in **Everyone** gets the envelope with NO
 * delegation and their mailbox binding (`openBindingB64`, from their public
 * record, verified under the added account key): whichever candidate commits
 * writes the Welcome unsigned to that mailbox. This device records the same
 * envelope for itself (a device never receives its own group messages), so its
 * own sweep can deliver; a send with no binding in hand re-sends the recorded
 * one. With neither, nothing is posted.
 *
 * `onSent` runs only after the publish succeeded. Never throws.
 */
export async function sendCommitterEnvelope(args: {
  readonly group: CommitterGroup;
  readonly addedAccountKey: Uint8Array;
  readonly openBindingB64: string | null;
  readonly broadcast: (bytes: Uint8Array) => Promise<void>;
  readonly onSent: (send: CommitterEnvelopeSend) => void;
}): Promise<EnvelopeSendResult> {
  const { group, addedAccountKey, openBindingB64 } = args;
  const session = reachSession();
  if (session === null) return "nothing-to-send";
  let bytes: Uint8Array | null;
  let signed: boolean;
  let coveredHex: string[];
  try {
    // ADR-0074 section 3.5a rules 1 and 2 - the grant held for the PERSON
    // (under any key they have had), for their CURRENT mailbox.
    const held = bestGrantForPerson(
      session.receivedGrants(),
      personKeysHex(journalHex(addedAccountKey)),
      { scopeHex: journalHex(group.groupId) },
    );
    coveredHex = await committerCandidateHexes(group);
    signed = held !== null;
    bytes =
      held === null
        ? await openCommitterEnvelope(journalHex(group.groupId), addedAccountKey, openBindingB64)
        : await signedEnvelope(coveredHex, addedAccountKey, held.wireB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ContactsClient: committer delegations not built", err instanceof Error ? err.message : "");
    return "nothing-to-send";
  }
  if (bytes === null) return "nothing-to-send";
  try {
    await args.broadcast(bytes);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ContactsClient: committer delegations not posted", err instanceof Error ? err.message : "");
    return "failed";
  }
  args.onSent({ sentAtMs: Date.now(), signed, coveredHex });
  return "sent";
}

/** What the re-send pass needs from its host. */
export interface EnvelopeResendPorts {
  readonly accountIdB64: string;
  readonly groupIdB64: string;
  /** The group now, in one pass. */
  snapshot(): IntentSnapshot;
  /** The committer candidates now ({@link committerCandidateHexes}). */
  candidatesHex(): Promise<readonly string[]>;
  /** The core's rule ({@link committerEnvelopeDueNow}). */
  due(
    last: CommitterEnvelopeSend | undefined,
    candidatesHex: readonly string[],
    nowMs: number,
  ): Promise<boolean>;
  /** Build and post a fresh envelope for the person `subjectKeyB64` names. */
  send(subjectKeyB64: string): Promise<EnvelopeSendResult>;
  /** The received-grants value now, or `null` with no reach session. It is
   *  immutable, so the same object means no grant arrived since. */
  grantsToken(): object | null;
  nowMs(): number;
}

/** What one pass did, for logs and tests. */
export interface EnvelopeResendSummary {
  readonly sent: number;
  readonly failed: number;
  readonly nothingToSend: number;
}

/** One pass at a time per `(account, group)` in this realm: a send is noted
 *  only after its publish, so a second pass meanwhile would still read it due. */
const resendInFlight = new Set<string>();

/**
 * The sends that came to nothing, per received-grants value. The rule keeps
 * saying due for such an Add, and asking again on every poll is work that
 * cannot succeed until a grant arrives (a new value) or the candidates change
 * (part of the key).
 */
const nothingToSendUnder = new WeakMap<object, Set<string>>();

/**
 * Re-send the envelope of every Add this device proposed that is still waiting
 * and is due. Costs one store read when there is no such Add. A send noted by
 * `ports.send` is what stops the next pass; the Add landing or being dropped
 * forgets its record, and with it the send.
 */
export async function runCommitterEnvelopeResends(
  ports: EnvelopeResendPorts,
): Promise<EnvelopeResendSummary> {
  const summary = { sent: 0, failed: 0, nothingToSend: 0 };
  const scope = retryScopeKey(ports.accountIdB64, ports.groupIdB64);
  if (resendInFlight.has(scope)) return summary;
  const adds = [...loadOwnRequests(ports.accountIdB64, ports.groupIdB64).values()].filter(
    (r) => r.request.kind === "add",
  );
  if (adds.length === 0) return summary;
  resendInFlight.add(scope);
  try {
    const snapshot = ports.snapshot();
    const waiting = adds.filter((r) => ownAddIsPending(r, snapshot));
    if (waiting.length === 0) return summary;
    const token = ports.grantsToken();
    if (token === null) return summary;
    const candidatesHex = await ports.candidatesHex();
    for (const record of waiting) {
      const subjectKeyB64 = record.request.subjectKeyB64;
      if (!(await ports.due(record.envelope, candidatesHex, ports.nowMs()))) continue;
      const memoKey = `${scope}|${subjectKeyB64}|${candidatesHex.join(",")}`;
      if (nothingToSendUnder.get(token)?.has(memoKey) === true) continue;
      const result = await ports.send(subjectKeyB64);
      if (result === "sent") {
        summary.sent += 1;
      } else if (result === "failed") {
        summary.failed += 1;
      } else {
        summary.nothingToSend += 1;
        const memo = nothingToSendUnder.get(token) ?? new Set<string>();
        memo.add(memoKey);
        nothingToSendUnder.set(token, memo);
      }
    }
  } finally {
    resendInFlight.delete(scope);
  }
  return summary;
}
