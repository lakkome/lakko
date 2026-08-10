// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042 orchestration — mint / receive / derive / subscribe / notify
 * the opaque per-group push routing id (`push_gid`), closing the
 * invariant-#5 push social-graph leak.
 *
 * ## The load-bearing rule: OWNER mints, everyone else RECEIVES
 *
 * `push_routing_secret` is 32 RANDOM bytes, so two members minting
 * independently would diverge and break routing (their `push_gid`s would
 * differ and the notify JOIN would never match). Therefore exactly ONE
 * source mints:
 *
 *   - Only the ADR-0022 group **OWNER** mints, once, and persists it
 *     ({@link ensureGroupPushRouting}). It then broadcasts the secret to
 *     the group over an E2EE MLS application message (the DS sees
 *     ciphertext — invariant #2).
 *   - Everyone else ONLY receives + persists the secret from that envelope
 *     ({@link receivePushRoutingSecretGated}) — they never mint.
 *
 * ## The gate is on BOTH sides
 *
 * `ensureGroupPushRouting` enforces "owner mints" on the SEND side, but that
 * alone is not a security boundary: nothing stops a non-owner from composing
 * and broadcasting an envelope by hand. So the RECEIVE side enforces it too —
 * {@link receivePushRoutingSecretGated} authorises a CHANGED secret against
 * the ADR-0022 role table before applying it. Without the receive gate a
 * single member could unilaterally rotate the group's routing id: every other
 * member would overwrite its secret and re-subscribe, and since per-group
 * unsubscribe is never called, each churn strands an orphan push-gw
 * subscription that keeps receiving wakes.
 *
 * ## Who may write what: owner > admin > member
 *
 * The accepted senders are the role table's **owner + admins**, ranked by
 * *what they may write* — deliberately WITHOUT persisting an authority level
 * alongside the secret (see {@link routingSecretVerdict}):
 *
 *   - **owner** — may always write, whatever is stored. The owner mints, so
 *     it is the only sender that may ROTATE an established secret. Because
 *     this branch ignores what is already held, the owner wins regardless of
 *     the order envelopes arrive in; no tie-break state is needed.
 *   - **admin** — may only FILL A HOLE (accepted when this device holds no
 *     secret at all), never overwrite one. This is what makes an
 *     admin-performed add work: ADR-0022 lets owner OR admin commit an
 *     add-member, and `redistributeSecretToGroup` fires after ANY add — so
 *     when a non-owner admin adds a member, that admin's redistribute is the
 *     new member's ONLY source of the secret (the owner broadcasts only at
 *     mint time). Gating on the owner alone silently denied those members
 *     push routing forever.
 *   - **plain member** — may never change the secret.
 *
 * A byte-identical re-broadcast is accepted from ANYONE (it changes nothing,
 * so it needs no authority), which keeps every relay of the redistribute
 * idempotent.
 *
 * ## Never subscribe under the leaky `group_id`
 *
 * The whole point of the ADR is that push-gw + APNs/FCM must not see the
 * DS journal key. So a member that does NOT yet hold the secret **defers**
 * its push subscription (transient no-push until the secret arrives) —
 * it never falls back to subscribing under `group_id`
 * ({@link subscribeGroupUnderPushGid} returns `"deferred"`). The publisher
 * always holds the secret (it is a member), so notify always has a
 * `push_gid`.
 *
 * ## Concurrency
 *
 * Provisioning can be triggered from two places in the same session — the
 * legacy-sweep on rehydrate and the chat view's onMount. A per-group
 * in-flight lock coalesces them so the random secret is minted exactly
 * once (a second concurrent mint would overwrite + double-broadcast a
 * DIFFERENT secret).
 */

import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { loadWasm } from "../wasm";
import { PushClient } from "./client";
import { getOrCreateDevicePushId } from "./device-id";
import {
  getGroupRoutingSecret,
  hasGroupRoutingSecret,
  putGroupRouting,
} from "./group-routing-store";

/** `push_routing_secret` length (bytes) — mirrors the Rust
 *  `PUSH_ROUTING_SECRET_LEN`. */
export const PUSH_ROUTING_SECRET_LEN = 32;

/** Total distribution-envelope length (bytes): the 29-byte domain
 *  separator + 1 version byte + the 32-byte secret. Used only as a cheap
 *  receive-side pre-filter before the authoritative wasm decode. Mirrors
 *  `lakko_contacts::push_routing` (29 + 1 + 32). */
export const PUSH_ROUTING_ENVELOPE_LEN =
  29 + 1 + PUSH_ROUTING_SECRET_LEN;

/** First byte of the envelope domain separator (`'l'` of
 *  `"lakko/push-routing-secret\0"`). The cheap pre-filter; the wasm
 *  `decodePushRoutingSecret` is the authoritative fail-closed check. */
const PUSH_ROUTING_ENVELOPE_FIRST_BYTE = 0x6c;

/** Mint a fresh `push_routing_secret` (32 CSPRNG bytes). OWNER ONLY —
 *  everyone else receives it over the distribution envelope. */
export function mintPushRoutingSecret(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(PUSH_ROUTING_SECRET_LEN));
}

/** Derive the stable opaque `push_gid` (32 bytes) from a secret via the
 *  audited wasm keyed-BLAKE3 derivation. Deterministic: every member that
 *  holds the same secret derives the same `push_gid`. */
export async function derivePushGid(secret: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.derivePushGid(secret);
}

/** Compose the E2EE distribution-envelope bytes for a secret (wasm). */
export async function composeSecretEnvelope(
  secret: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composePushRoutingSecret(secret);
}

/**
 * Attempt to decode an MLS application-message plaintext as a
 * push-routing-secret distribution envelope. Returns the 32-byte secret,
 * or `null` when the bytes are not this envelope (fail-closed — the
 * authoritative check is the wasm codec's domain + version + exact-length
 * gate). A cheap length/first-byte pre-filter avoids a wasm call on every
 * chat message.
 */
export async function tryDecodeSecretEnvelope(
  bytes: Uint8Array,
): Promise<Uint8Array | null> {
  if (
    bytes.length !== PUSH_ROUTING_ENVELOPE_LEN ||
    bytes[0] !== PUSH_ROUTING_ENVELOPE_FIRST_BYTE
  ) {
    return null;
  }
  const wasm = await loadWasm();
  try {
    return wasm.decodePushRoutingSecret(bytes);
  } catch {
    // Not a valid push-routing envelope (domain/version/length mismatch) —
    // fall through so the caller runs the normal chat parse.
    return null;
  }
}

/** Lowercase-hex a byte buffer (the push-gw pseudonym wire form). */
function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/**
 * Persist a group's secret (device-key-encrypted) AND its
 * `push_gid → (account, group_id)` reverse-index entry. Idempotent.
 * Returns the derived `push_gid` bytes, or `null` when there is no device
 * key (half-reset — the caller bails out of subscribing).
 */
export async function persistGroupRoutingSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
): Promise<Uint8Array | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  const pushGid = await derivePushGid(secret);
  await putGroupRouting(
    { accountId, groupIdB64, secret, pushGidHex: toHex(pushGid) },
    deviceKey,
  );
  return pushGid;
}

/**
 * Look up the `push_gid` bytes for a group, or `null` when this device
 * does not (yet) hold the secret. Used by the notify path (publisher) and
 * the subscribe path.
 */
export async function pushGidForGroup(
  accountId: string,
  groupIdB64: string,
): Promise<Uint8Array | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  const secret = await getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
  if (secret === null) return null;
  return derivePushGid(secret);
}

/**
 * Subscribe this device to push wakes for a group UNDER ITS `push_gid`.
 * A member that does not hold the secret **defers** (returns `"deferred"`)
 * — it never subscribes under the leaky `group_id`. Returns `"subscribed"`
 * once the wake registration lands.
 */
export async function subscribeGroupUnderPushGid(
  accountId: string,
  groupIdB64: string,
): Promise<"subscribed" | "deferred"> {
  const pushGid = await pushGidForGroup(accountId, groupIdB64);
  if (pushGid === null) return "deferred";
  const devicePseudonym = getOrCreateDevicePushId();
  await new PushClient().subscribeGroup(devicePseudonym, pushGid);
  return "subscribed";
}

/** Fire-and-forget wrapper for the subscribe — failures are logged and
 *  swallowed (push is a UX nicety; foreground polling still works). */
export function subscribeGroupUnderPushGidBestEffort(
  accountId: string,
  groupIdB64: string,
): void {
  void subscribeGroupUnderPushGid(accountId, groupIdB64).catch((err) => {
    // eslint-disable-next-line no-console
    console.warn("push_gid subscribe failed", err);
  });
}

/**
 * RECEIVE side, UNCHECKED — persist the secret (idempotent) + subscribe
 * under the derived `push_gid`. Best-effort; a persist that yields no
 * `push_gid` (no device key) skips the subscribe.
 *
 * This applies whatever it is handed. Production receive paths MUST go
 * through {@link receivePushRoutingSecretGated}, which authorises the
 * envelope against the ADR-0022 role table (owner + admins) first; this
 * remains exported only as that wrapper's apply step.
 */
export async function receivePushRoutingSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
): Promise<void> {
  const pushGid = await persistGroupRoutingSecret(
    accountId,
    groupIdB64,
    secret,
  );
  if (pushGid === null) return;
  subscribeGroupUnderPushGidBestEffort(accountId, groupIdB64);
}

/** Byte-wise equality. Not constant-time on purpose: both operands are
 *  values the sender already knows (it just transmitted one of them), so
 *  there is no secret to leak through timing here. */
function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/**
 * Outcome of the receive-side role gate ({@link receivePushRoutingSecretGated}).
 * The four `accept-*` verdicts persist + re-subscribe; the three `reject-*`
 * verdicts drop the envelope entirely (no persist, no subscribe).
 */
export type RoutingSecretVerdict =
  /** Byte-identical to the secret already held — idempotent no-op re-persist.
   *  Keeps the post-add-member `redistribute` working when relayed or
   *  re-observed, whoever forwarded it; needs no authority. */
  | "accept-identical"
  /** A CHANGED secret authored by the ADR-0022 role-table OWNER. The owner
   *  may always write, whatever is stored — this is the rotation authority. */
  | "accept-owner"
  /** A first-ever secret authored by an ADR-0022 ADMIN while this device
   *  holds NONE. An admin may seed a hole (so an admin-performed add-member
   *  actually delivers routing to the new member) but never overwrite. */
  | "accept-admin-seed"
  /** A CHANGED secret in a group with NO role table (legacy / open group).
   *  There is no owner or admin set to check against, so this keeps the
   *  pre-gate behaviour rather than silently breaking those groups. */
  | "accept-no-owner"
  /** An ADMIN tried to overwrite a secret this device already holds — or one
   *  it cannot prove it lacks (unreadable store). Only the owner rotates. */
  | "reject-admin-overwrite"
  /** A CHANGED secret from a member that is neither owner nor admin. */
  | "reject-not-authorized"
  /** A CHANGED secret whose authorship could not be established (the
   *  member lookup / role-table read threw, or the frame carried no sender
   *  leaf) — dropped, fail-closed. */
  | "reject-unverifiable";

/** True when the verdict lets the envelope through. */
export function verdictAccepts(v: RoutingSecretVerdict): boolean {
  return v.startsWith("accept");
}

/** True when `key` is byte-equal to one of `set`. */
function containsKey(set: readonly Uint8Array[], key: Uint8Array): boolean {
  return set.some((k) => bytesEqual(k, key));
}

/**
 * Pure decision core of the receive-side role gate — no IO, so the whole
 * accept/reject matrix is directly testable.
 *
 * The rank is **owner > admin > member**, expressed purely as *who may
 * overwrite what*, so NO authority level is persisted alongside the secret:
 *
 * | stored              | sender          | verdict                  |
 * |---------------------|-----------------|--------------------------|
 * | identical           | anyone          | `accept-identical`       |
 * | any (or none)       | owner           | `accept-owner`           |
 * | none (provably)     | admin           | `accept-admin-seed`      |
 * | present / unprovable| admin           | `reject-admin-overwrite` |
 * | changed             | plain member    | `reject-not-authorized`  |
 * | changed, no roles   | anyone          | `accept-no-owner`        |
 * | changed, unreadable | anyone          | `reject-unverifiable`    |
 *
 * Because the owner branch is decided BEFORE anything is read about what is
 * stored, an owner rotation lands whatever an admin seeded and in whatever
 * order the two envelopes arrive — that is the whole of "owner takes
 * priority", with no tie-break state.
 */
export function routingSecretVerdict(args: {
  /** `group.roleTableOwner()` — `undefined` = legacy / open group. */
  ownerKey: Uint8Array | undefined;
  /** `group.roleTableAdmins()` sliced into 32-byte keys. Only consulted when
   *  `ownerKey` is defined (an open group has no admin set either). */
  adminKeys: readonly Uint8Array[];
  /** The sender leaf's signature public key, `undefined` when it could
   *  not be resolved from the member snapshot. */
  senderKey: Uint8Array | undefined;
  /** The secret this device already holds, `null` when it holds none. */
  storedSecret: Uint8Array | null;
  /** Set when the local secret could NOT be read (store threw, or there is
   *  no device key). `storedSecret === null` then means "unknown", not
   *  "provably absent" — so the admin hole-fill branch must refuse. */
  storedUnreadable: boolean;
  /** The secret carried by the incoming envelope. */
  incomingSecret: Uint8Array;
  /** Set when reading the role table / member snapshot threw — authorship
   *  cannot be established, so a CHANGED secret must be refused. */
  verifyFailed: boolean;
}): RoutingSecretVerdict {
  // Idempotent case first — a re-observed identical secret changes nothing,
  // so it needs no authority at all.
  if (
    args.storedSecret !== null &&
    bytesEqual(args.storedSecret, args.incomingSecret)
  ) {
    return "accept-identical";
  }
  // From here the envelope would CHANGE this device's routing id.
  if (args.verifyFailed) return "reject-unverifiable";
  // Legacy / open group: no role table, hence no owner or admin set to
  // authorise against. Keep today's behaviour rather than breaking those
  // groups.
  if (args.ownerKey === undefined) return "accept-no-owner";
  if (args.senderKey === undefined) return "reject-unverifiable";
  // Owner outranks everything: may rotate whatever is stored, in any order.
  if (bytesEqual(args.senderKey, args.ownerKey)) return "accept-owner";
  // Admin may SEED a hole but never overwrite. "Hole" must be provable —
  // an unreadable store is treated as possibly-occupied (fail-closed).
  if (containsKey(args.adminKeys, args.senderKey)) {
    return args.storedSecret === null && !args.storedUnreadable
      ? "accept-admin-seed"
      : "reject-admin-overwrite";
  }
  return "reject-not-authorized";
}

/**
 * RECEIVE side, GATED — the production entry point for an incoming
 * push-routing distribution envelope.
 *
 * The role gate previously existed only on the SEND side
 * ({@link ensureGroupPushRouting} checks `ownRole === Owner`), so any single
 * non-owner member could broadcast a fresh secret and unilaterally rotate
 * the whole group's routing id: every other member would overwrite their
 * secret and re-subscribe, and because per-group unsubscribe is never
 * called, each churn stranded an orphan push-gw subscription that keeps
 * receiving wakes.
 *
 * The authorised senders are the ADR-0022 owner AND admins, ranked by what
 * they may write — see {@link routingSecretVerdict} for the full matrix and
 * for why admins must be included (an admin-performed add-member is the new
 * member's only source of the secret).
 *
 * Never throws — the poll loop calls this per message.
 *
 * @returns the verdict, so the caller can log / assert on it.
 */
export async function receivePushRoutingSecretGated(
  accountId: string,
  group: WasmGroup,
  groupIdB64: string,
  senderLeaf: number | undefined,
  secret: Uint8Array,
): Promise<RoutingSecretVerdict> {
  let storedSecret: Uint8Array | null = null;
  let storedUnreadable = false;
  try {
    const deviceKey = await getDeviceKey(accountId);
    if (deviceKey === null) {
      // Half-reset: the store cannot be read at all, so "this device holds
      // none" is unprovable. (A persist would no-op for the same reason.)
      storedUnreadable = true;
    } else {
      storedSecret = await getGroupRoutingSecret(
        accountId,
        groupIdB64,
        deviceKey,
      );
    }
  } catch {
    // Unreadable local secret — cannot prove "identical" NOR "absent"; the
    // role check below still decides, fail-closed on the admin-seed branch.
    storedSecret = null;
    storedUnreadable = true;
  }

  let ownerKey: Uint8Array | undefined;
  let adminKeys: Uint8Array[] = [];
  let senderKey: Uint8Array | undefined;
  let verifyFailed = false;
  try {
    ownerKey = group.roleTableOwner() ?? undefined;
    // Flat concatenation of 32-byte admin signature keys (empty for an open
    // group), sliced the same way `contacts/client.ts` does.
    const flatAdmins = group.roleTableAdmins();
    for (let i = 0; i + 32 <= flatAdmins.length; i += 32) {
      adminKeys.push(flatAdmins.slice(i, i + 32));
    }
    if (senderLeaf !== undefined) {
      const members = group.members() as Array<{
        leafIndex: number;
        signaturePublicKey: Uint8Array;
      }>;
      senderKey = members.find((m) => m.leafIndex === senderLeaf)
        ?.signaturePublicKey;
    }
  } catch {
    // A torn group / unreadable role table — authorship is unverifiable.
    verifyFailed = true;
    adminKeys = [];
  }

  const verdict = routingSecretVerdict({
    ownerKey,
    adminKeys,
    senderKey,
    storedSecret,
    storedUnreadable,
    incomingSecret: secret,
    verifyFailed,
  });

  if (!verdictAccepts(verdict)) {
    // eslint-disable-next-line no-console
    console.warn(
      "push-routing secret REFUSED (ADR-0042 role gate)",
      verdict,
      "senderLeaf",
      senderLeaf,
    );
    return verdict;
  }

  try {
    await receivePushRoutingSecret(accountId, groupIdB64, secret);
  } catch (err) {
    // Best-effort, exactly as before the gate — never throw out of the
    // poll loop.
    // eslint-disable-next-line no-console
    console.warn("push-routing secret receive failed", err);
  }
  return verdict;
}

/** Per-group in-flight provisioning promise, so a concurrent legacy-sweep
 *  + onMount for the same group mint the random secret exactly once. */
const provisioning = new Map<string, Promise<void>>();

/**
 * Ensure a group's push routing is provisioned for this device.
 *
 *   - If the local client is the OWNER (ADR-0022) and no secret is held
 *     yet: mint + persist + broadcast the distribution envelope (via
 *     `broadcast`, which posts an E2EE MLS application message to the
 *     group journal).
 *   - Then subscribe under `push_gid` if the secret is held (owner just
 *     minted it; a member may already hold one from a prior receive),
 *     else defer.
 *
 * This single funnel serves BOTH the create path (owner opens the freshly
 * founded group) AND the legacy-sweep (owner opens / rehydrates an
 * un-provisioned group). Non-owners never mint here — they only subscribe
 * if they already hold a secret; otherwise they wait for the receive path.
 *
 * @param broadcast posts the distribution-envelope bytes to the group
 *                  journal as an E2EE MLS application message (persist-
 *                  before-publish). Only invoked on the owner-mint branch.
 */
export function ensureGroupPushRouting(
  accountId: string,
  group: WasmGroup,
  identity: WasmIdentity,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<void> {
  const groupIdB64 = base64Encode(group.groupId);
  const lockKey = `${accountId} ${groupIdB64}`;
  const inflight = provisioning.get(lockKey);
  if (inflight !== undefined) return inflight;

  const run = (async (): Promise<void> => {
    // ADR-0022 role table: `Owner` (0). `undefined` = open / legacy group
    // with no role table — nobody mints (avoids divergence), matching the
    // ADR-0042 open-question decision to keep un-owned legacy groups on
    // deferred push until a natural re-creation.
    const wasm = await loadWasm();
    let isOwner = false;
    try {
      isOwner = group.ownRole(identity) === wasm.Role.Owner;
    } catch {
      isOwner = false;
    }

    if (isOwner && !(await hasGroupRoutingSecret(accountId, groupIdB64))) {
      const secret = mintPushRoutingSecret();
      const pushGid = await persistGroupRoutingSecret(
        accountId,
        groupIdB64,
        secret,
      );
      if (pushGid !== null) {
        // Distribute to founding members over the E2EE journal. Best-effort
        // beyond the persist — a broadcast failure retries on the next
        // launch / add-member; the local secret + reverse index are already
        // durable.
        try {
          await broadcast(await composeSecretEnvelope(secret));
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("push-routing secret broadcast failed", err);
        }
      }
    }

    // Subscribe (or defer) under push_gid. Owner just minted → subscribes;
    // a member with no secret → deferred (never under group_id).
    subscribeGroupUnderPushGidBestEffort(accountId, groupIdB64);
  })().finally(() => {
    provisioning.delete(lockKey);
  });

  provisioning.set(lockKey, run);
  return run;
}

/**
 * Re-broadcast the CURRENT secret to a group (used right after an
 * add-member commit so the new member learns it on their next poll).
 * No-op when this device does not hold the secret. Idempotent for existing
 * members (a re-received secret is the same value → an idempotent
 * re-persist). Best-effort.
 *
 * ADR-0022 lets owner OR admin commit the add, so this fires from an admin
 * too; the receive gate accepts it because the new member holds no secret
 * (`accept-admin-seed`) while every existing member sees the byte-identical
 * value (`accept-identical`).
 */
export async function redistributeSecretToGroup(
  accountId: string,
  group: WasmGroup,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<void> {
  const groupIdB64 = base64Encode(group.groupId);
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return;
  const secret = await getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
  if (secret === null) return;
  await broadcast(await composeSecretEnvelope(secret));
}
