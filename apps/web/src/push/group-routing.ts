// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042 orchestration — mint / receive / derive / subscribe / notify
 * the opaque per-group push routing id (`push_gid`), closing the
 * invariant-#5 push social-graph leak — plus the ADR-0043 rotation of that
 * id when a member is REMOVED (see the rotation section at the bottom of
 * this file for why removal is the only event that rotates).
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
 *   - **admin** — may also write whatever is stored (ADR-0043, ratified
 *     option (b)). Two distinct things depend on it: an admin FILLING A HOLE
 *     (`accept-admin-seed`) is what makes an admin-performed add work —
 *     ADR-0022 lets owner OR admin commit an add-member, and
 *     `redistributeSecretToGroup` fires after ANY add, so that admin's
 *     redistribute is the new member's ONLY source of the secret; an admin
 *     ROTATING an established secret (`accept-admin-rotate`) is what makes
 *     ADR-0043 rotation-on-removal reachable at all, since ADR-0022 lets an
 *     admin commit the Remove. Refusing the overwrite left an admin able to
 *     evict a member but unable to retire that member's routing id.
 *   - **plain member** — may never change the secret.
 *
 * The cost of the admin tier is recorded honestly in ADR-0043 § fork (b):
 * it widens G-36 residual 3 from "a malicious admin can seed a secret to a
 * member who holds none" to "a malicious admin can churn the group's routing
 * id at will". The capability gain is small (an admin can already evict
 * members outright) and the churn is a wake outage, never a downgrade —
 * a member without the current secret still DEFERS, it never falls back to
 * the leaky `group_id`.
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
 * ## The broadcast must be durable, not just attempted
 *
 * The owner persists the minted secret BEFORE broadcasting (two divergent
 * random secrets would be worse than two identical envelopes). That makes a
 * failed broadcast dangerous on its own: a guard shaped "do I hold a
 * secret?" short-circuits forever after, the envelope is never re-sent, and
 * every other member defers permanently — the whole group silently loses
 * push. So the store records whether the envelope was CONFIRMED on the wire
 * (`distributed`), and {@link ensureGroupPushRouting} re-broadcasts while it
 * is false. The envelope is recomputable from the stored secret, so this
 * needs one durable boolean, not an outbox of wire bytes (contrast the
 * ADR-0040 commit outbox, whose commit bytes are unrecoverable once the
 * in-memory MLS state moves on). Re-sending is safe because the value is
 * byte-identical: every member reads it as `accept-identical`.
 *
 * The same flag has to cover a failed REDISTRIBUTE, not only a failed mint
 * broadcast. It is per-`(account, group)`, not per-member, so once the mint
 * lands the row reads `distributed` — and a later
 * {@link redistributeSecretToGroup} that fails after an add-member would then
 * never be retried, leaving the NEW member (whose only source of the secret
 * is that redistribute) deferring push forever: the same permanent-no-push
 * bug, scoped to one member. So a failed send flips the row back to pending
 * and the re-broadcast branch below repairs it.
 *
 * ## MINT is owner-only; RE-BROADCAST needs no authority at all
 *
 * The retry has to be reachable from whichever device's send failed, and
 * ADR-0022 lets an ADMIN commit an add-member — so an admin's redistribute
 * can be the new member's only source of the secret. Gating the retry on
 * ownership would leave exactly that case unrepaired: the admin's row goes
 * pending, the admin never re-sends, the owner's row still reads
 * `distributed` so the owner never learns, and the newly-added member never
 * gets push.
 *
 * So the two branches of {@link ensureGroupPushRouting} carry DIFFERENT
 * authority requirements:
 *
 *   - **mint** (state `absent`) stays OWNER-ONLY. The secret is 32 random
 *     bytes; two independent mints diverge the group's `push_gid` and break
 *     the notify JOIN. That is the whole reason the owner-mints rule exists.
 *   - **re-broadcast** (state `pending`, i.e. this device already HOLDS a
 *     secret) is open to ANY holder — owner, admin, or plain member. It
 *     re-sends the exact stored bytes, which {@link routingSecretVerdict}
 *     resolves as `accept-identical` for ANY sender, before authority is even
 *     consulted. No new secret is ever generated on that path, so it cannot
 *     introduce divergence, and it grants nobody a new capability: a member
 *     could already replay a byte-identical envelope and have it accepted.
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
  deleteRoute,
  getGroupRoutingSecret,
  getGroupRoutingState,
  markGroupRoutingDistributed,
  markGroupRoutingPending,
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
 *
 * @param distributed whether the in-group distribution envelope for this
 *                    secret is already on the wire. `false` on the owner's
 *                    mint (the broadcast follows); `true` on a receive.
 */
export async function persistGroupRoutingSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
  distributed: boolean,
): Promise<Uint8Array | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  const pushGid = await derivePushGid(secret);
  await putGroupRouting(
    { accountId, groupIdB64, secret, pushGidHex: toHex(pushGid), distributed },
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
 * ADR-0043 — retire a SUPERSEDED `push_gid`: drop this device's push-gw
 * subscription under it, then delete its reverse-index row.
 *
 * Called on BOTH sides of a rotation with the OLD secret — by the rotator
 * after it re-arms under the new id, and by every remaining member when the
 * gated receive path applies a CHANGED secret. Both must, and for the same
 * reason: `POST /push/group/unsubscribe` deletes exactly the
 * `(device, group)` row named in the request, and a client knows only its OWN
 * device pseudonym, so nobody can tear down anyone else's row. If only the
 * rotator unsubscribed, every OTHER member would leave a live row under the
 * old id — one dead subscription per member per rotation, which is the
 * "versioned cluster archive" ADR-0043 § Context calls strictly worse than
 * not rotating at all.
 *
 * The **subscription** teardown is what carries the privacy value; the route
 * row is local bookkeeping (a reverse-map miss already degrades to the
 * generic "New message" banner under the ADR-0041 fail-closed posture), so it
 * is deleted immediately — the simpler invariant — rather than retained for
 * an overlap window.
 *
 * Best-effort and never throws: the caller has ALREADY switched to the new
 * id, so a failure here must not unwind the rotation. The cost of a failed
 * unsubscribe is a stale row that nothing notifies again.
 */
export async function retireOldPushGid(oldSecret: Uint8Array): Promise<void> {
  let oldGid: Uint8Array;
  let oldGidHex: string;
  try {
    oldGid = await derivePushGid(oldSecret);
    oldGidHex = toHex(oldGid);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push-routing old push_gid derivation failed", err);
    return;
  }
  try {
    await new PushClient().unsubscribeGroup(getOrCreateDevicePushId(), oldGid);
  } catch (err) {
    // The new id is already armed; a failed unsubscribe leaves a row that
    // nothing notifies again, not a broken group.
    // eslint-disable-next-line no-console
    console.warn("push_gid unsubscribe failed", err);
  }
  try {
    await deleteRoute(oldGidHex);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push-routing stale route delete failed", err);
  }
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
 *
 * Persisted as `distributed: true`: a secret that ARRIVED over the journal
 * is by definition already on the wire, so a receiver has nothing to confirm.
 * Storing it pending would make every receiver re-broadcast a value the whole
 * group already has, on the very next provisioning pass.
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
    true,
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
 * The five `accept-*` verdicts persist + re-subscribe; the two `reject-*`
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
   *  PROVABLY holds none — the admin-seed path that makes an
   *  admin-performed add-member deliver routing to the new member. */
  | "accept-admin-seed"
  /** A CHANGED secret authored by an ADR-0022 ADMIN over one this device
   *  already holds (or cannot prove it lacks). ADR-0043 ratified option (b):
   *  an admin may rotate, because ADR-0022 lets an admin commit the Remove
   *  that makes a rotation necessary — refusing here left an admin able to
   *  evict a member but unable to retire that member's routing id. Kept as a
   *  DISTINCT verdict from `accept-admin-seed` so a rotation is legible in
   *  the log; both accept. */
  | "accept-admin-rotate"
  /** A CHANGED secret in a group with NO role table (legacy / open group).
   *  There is no owner or admin set to check against, so this keeps the
   *  pre-gate behaviour rather than silently breaking those groups. */
  | "accept-no-owner"
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
 * The rank is **owner ≥ admin > member**, expressed purely as *who may
 * overwrite what*, so NO authority level is persisted alongside the secret:
 *
 * | stored              | sender          | verdict                  |
 * |---------------------|-----------------|--------------------------|
 * | identical           | anyone          | `accept-identical`       |
 * | any (or none)       | owner           | `accept-owner`           |
 * | none (provably)     | admin           | `accept-admin-seed`      |
 * | present / unprovable| admin           | `accept-admin-rotate`    |
 * | changed             | plain member    | `reject-not-authorized`  |
 * | changed, no roles   | anyone          | `accept-no-owner`        |
 * | changed, unreadable | anyone          | `reject-unverifiable`    |
 *
 * Because the owner branch is decided BEFORE anything is read about what is
 * stored, an owner rotation lands whatever an admin seeded and in whatever
 * order the two envelopes arrive — that is the whole of "owner takes
 * priority", with no tie-break state.
 *
 * **ADR-0043 (ratified option (b)) widened the admin row from a refusal to an
 * acceptance.** The `storedSecret` / `storedUnreadable` pair no longer gates
 * an admin's *authority* — it only picks WHICH admin verdict is reported
 * (`seed` when the hole is provable, `rotate` otherwise), which keeps the
 * distinction legible in the log and keeps the two cases separately testable.
 * Owner is still evaluated first, so the ordering property above is
 * unchanged.
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
   *  "provably absent" — so an admin write is reported as a ROTATE rather
   *  than a seed. It no longer changes the accept/reject outcome (ADR-0043
   *  option (b) lets an admin overwrite either way); it keeps the verdict
   *  honest about what was actually observed. */
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
  // Admin may SEED a hole AND rotate an established secret (ADR-0043
  // option (b)). The two are reported separately — a provable hole is a
  // seed, anything else (held, or unreadable hence possibly held) is a
  // rotate — but both accept.
  if (containsKey(args.adminKeys, args.senderKey)) {
    return args.storedSecret === null && !args.storedUnreadable
      ? "accept-admin-seed"
      : "accept-admin-rotate";
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
 * member's only source of the secret; an admin-performed REMOVE is what makes
 * an ADR-0043 rotation necessary).
 *
 * ADR-0043 — when an accepted secret CHANGES the one this device held, the
 * old `push_gid` is retired here too ({@link retireOldPushGid}): every member
 * tears down its OWN push-gw row, because no client can delete anyone else's.
 * This is the receiving half of the rotation; the rotator runs the same
 * teardown in {@link rotatePushRoutingSecret}.
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
    // ADR-0043 — the applied secret SUPERSEDED one this device held, so the
    // id it was subscribed under is now dead. Retire it: this device is the
    // only party that can drop its own push-gw row. Read from the snapshot
    // taken BEFORE the persist above — the `secrets` row is overwritten in
    // place, so after the write the old bytes are unrecoverable.
    if (storedSecret !== null && !bytesEqual(storedSecret, secret)) {
      await retireOldPushGid(storedSecret);
    }
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
 * Send the distribution envelope for a secret and, ONLY once the broadcast
 * resolves, record it as distributed. Never throws — a failure leaves the
 * flag `false`, which is precisely what makes the next provisioning pass
 * retry it.
 *
 * The envelope is recomposed from the secret on every attempt, so nothing
 * about the wire bytes needs to be spooled between attempts.
 */
async function broadcastAndMarkDistributed(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<void> {
  try {
    await broadcast(await composeSecretEnvelope(secret));
    await markGroupRoutingDistributed(accountId, groupIdB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push-routing secret broadcast failed", err);
  }
}

/**
 * Ensure a group's push routing is provisioned for this device.
 *
 *   - **Mint (OWNER ONLY).** If the local client is the ADR-0022 OWNER and
 *     no secret is held yet: mint + persist + broadcast the distribution
 *     envelope (via `broadcast`, which posts an E2EE MLS application message
 *     to the group journal) + record the broadcast as confirmed. Owner-only
 *     because the secret is 32 RANDOM bytes — a second minter would diverge
 *     the group's `push_gid`. A non-owner holding no secret never mints; it
 *     waits for the receive path.
 *   - **Re-broadcast (ANY HOLDER).** If this device holds a secret whose
 *     envelope was never confirmed on the wire: RE-BROADCAST it (recomposed
 *     from the stored secret) and record the confirmation. Without this
 *     branch a single failed send is permanent — the secret is durable, so
 *     the mint branch never fires again, and every other member defers
 *     forever, i.e. the group silently loses push.
 *   - Then subscribe under `push_gid` if the secret is held (owner just
 *     minted it; a member may already hold one from a prior receive),
 *     else defer.
 *
 * The re-broadcast deliberately carries NO role check, and that is not a
 * relaxation of the ADR-0042 gate: it sends the byte-identical secret this
 * device already holds, which {@link routingSecretVerdict} resolves as
 * `accept-identical` for ANY sender BEFORE any authority is consulted. It
 * generates no new secret, so it cannot introduce divergence, and it confers
 * no new capability — a member could already replay an identical envelope and
 * have it accepted. Restricting it to the owner instead LOSES the
 * admin-performed add (ADR-0022 lets an admin commit an add-member, so that
 * admin's failed redistribute is the new member's only lost source of the
 * secret, and only that admin's own row records the failure).
 *
 * This single funnel serves BOTH the create path (owner opens the freshly
 * founded group) AND the legacy-sweep (any member opens / rehydrates a group
 * whose last send is unconfirmed).
 *
 * @param broadcast posts the distribution-envelope bytes to the group
 *                  journal as an E2EE MLS application message (persist-
 *                  before-publish). Invoked on the owner's mint branch and on
 *                  ANY holder's re-broadcast branch.
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

    const state = await getGroupRoutingState(accountId, groupIdB64);

    if (state === "absent") {
      // MINT — OWNER ONLY. The secret is 32 random bytes, so a second minter
      // would diverge the group's `push_gid` and break the notify JOIN. This
      // is the branch the owner-only rule actually protects.
      if (isOwner) {
        const secret = mintPushRoutingSecret();
        // Persist BEFORE broadcasting — minting twice (two divergent random
        // secrets) is worse than broadcasting twice (byte-identical, hence
        // idempotent). The `distributed: false` flag is what makes the
        // ordering recoverable: a broadcast failure below is retried by the
        // `pending` branch on the NEXT provisioning pass (chat-open or the
        // launch legacy-sweep), so the group is never stranded.
        const pushGid = await persistGroupRoutingSecret(
          accountId,
          groupIdB64,
          secret,
          false,
        );
        if (pushGid !== null) {
          // Distribute to founding members over the E2EE journal.
          await broadcastAndMarkDistributed(
            accountId,
            groupIdB64,
            secret,
            broadcast,
          );
        }
      }
    } else if (state === "pending") {
      // RE-BROADCAST — ANY HOLDER, no role check. A previous send from THIS
      // device (the owner's mint broadcast, or an admin's / any member's
      // redistribute) never landed. The envelope is recomputable from the
      // stored secret, so re-send it — no outbox of wire bytes is needed,
      // only this flag.
      //
      // No authority is required because nothing changes: the bytes are
      // byte-identical to what is already held, which `routingSecretVerdict`
      // resolves as `accept-identical` for ANY sender BEFORE it consults the
      // role table. Gating this on ownership would strand the admin-add case
      // — ADR-0022 lets an admin commit the add, so that admin's redistribute
      // is the new member's only source of the secret; if only the owner
      // could retry, the admin's pending flag would be inert and the owner's
      // row (still `distributed`) would never signal a retry.
      let secret: Uint8Array | null = null;
      try {
        const deviceKey = await getDeviceKey(accountId);
        secret =
          deviceKey === null
            ? null
            : await getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
      } catch (err) {
        // Half-reset or an undecryptable row — nothing to re-broadcast.
        // eslint-disable-next-line no-console
        console.warn(
          "push-routing secret unreadable, re-broadcast skipped",
          err,
        );
      }
      if (secret !== null) {
        await broadcastAndMarkDistributed(
          accountId,
          groupIdB64,
          secret,
          broadcast,
        );
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
 *
 * A successful send is the same confirmation the owner's own broadcast
 * gives, so it clears a pending `distributed` flag too — an add-member that
 * happens to follow a failed mint-broadcast repairs it here rather than
 * waiting for the next provisioning pass.
 *
 * A FAILED send does the converse: it marks the row **pending** again. The
 * `distributed` flag is per-`(account, group)`, not per-member, so without
 * this a group whose mint broadcast already landed looks settled forever —
 * no pass re-sends, and the newly-added member, whose ONLY source of the
 * secret is this redistribute, defers push permanently. Marking pending
 * re-arms {@link ensureGroupPushRouting}'s existing re-broadcast branch, so
 * the next launch / chat-open retries until it lands. The retry is
 * byte-identical, so every existing member reads it as `accept-identical`
 * (idempotent) while the new member finally gets it; the cost is at most one
 * extra control message per failed redistribute.
 *
 * The retry lands on THIS device, whatever its role: the re-broadcast branch
 * in {@link ensureGroupPushRouting} is open to any secret HOLDER, because
 * re-sending a byte-identical secret needs no authority. That is what makes
 * the mark meaningful for an admin (ADR-0022 lets admins commit an add too) —
 * the admin's own row is the only place the failure is recorded, since the
 * owner's row still reads `distributed` and the owner has no way to learn
 * that someone else's redistribute failed.
 *
 * Rejects with the broadcast's error after marking, so the caller still logs
 * the failure exactly as before.
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
  try {
    await broadcast(await composeSecretEnvelope(secret));
  } catch (err) {
    try {
      await markGroupRoutingPending(accountId, groupIdB64);
    } catch (markErr) {
      // Best-effort: a store failure here costs the retry, not correctness.
      // eslint-disable-next-line no-console
      console.warn("push-routing pending mark failed", markErr);
    }
    throw err;
  }
  await markGroupRoutingDistributed(accountId, groupIdB64);
}

// ---------------------------------------------------------------------------
// ADR-0043 — rotate the routing secret when a member is REMOVED
// ---------------------------------------------------------------------------
//
// ## What rotation buys, stated precisely
//
// A removed member keeps the group's `push_routing_secret` on disk, so it can
// still derive the current `push_gid` — and since nothing tears its
// subscription down, push-gw keeps waking that device for a group it was
// evicted from. The leak is not storage hygiene: an ex-member learns "this
// group was active at time T" on every send, indefinitely.
//
// It cannot be closed by unsubscribing, because `POST /push/group/unsubscribe`
// deletes exactly the `(device, group)` row named in the request and a client
// knows only its OWN device pseudonym — no remaining member can reach the
// ex-member's row, and the ex-member's client has no "you were removed"
// handling to do it itself. Re-keying is the only mechanism the GROUP has:
// after a rotation nobody ever notifies the old id again, so that row goes
// **inert without the ex-member's cooperation**.
//
// What it does NOT buy: unlinkability of the CLUSTER. The provider sees
// `{A,B,C,D}` under the old id and `{A,B,C}` under the new one and re-links by
// token-set intersection — the set difference even names the evicted device.
// That is a deliberate, recorded trade (ADR-0043 § crux), not an oversight:
// closing it needs per-device-blinded routing ids (G-36 residual 1).
//
// ## Rotate on REMOVAL only, never on add
//
// An added member is *supposed* to route — `redistributeSecretToGroup` already
// hands it the current secret. Rotating on add would also turn that
// redistribute into a CHANGED secret against every existing member's stored
// one on every add, for zero privacy gain.

/** The local device's ADR-0022 role in the group, in the shape the chat view
 *  already computes it. `null` = an open / legacy group with no role table. */
export type PushRotationRole = "owner" | "admin" | "member" | null;

/**
 * Read this device's ADR-0022 role off the LIVE group, at the moment of the
 * call.
 *
 * The rotation call sites MUST use this rather than a cached / signal-held
 * role. On web the chat view keeps `ownRole` in a signal refreshed by an async
 * effect, so it lags the group by up to one poll — and a poll batch carrying a
 * demotion AND a removal would then have a just-demoted admin rotate: every
 * peer refuses the envelope on the receive-side role gate
 * (`reject-not-authorized`) and the rotator strands itself on a secret nobody
 * else holds, i.e. the group's wakes stop. iOS and Android read the role
 * synchronously off the group at rotation time; this is the web equivalent.
 *
 * Never throws — a torn group reads as `null`, which does not rotate
 * (fail-closed).
 */
export async function readPushRotationRole(
  group: WasmGroup,
  identity: WasmIdentity,
): Promise<PushRotationRole> {
  try {
    const wasm = await loadWasm();
    const r = group.ownRole(identity);
    if (r === wasm.Role.Owner) return "owner";
    if (r === wasm.Role.Admin) return "admin";
    if (r === wasm.Role.Member) return "member";
    return null;
  } catch {
    return null;
  }
}

/** Verdict of the pure {@link membershipRotationDecision} core. */
export type PushRotationDecision =
  /** Membership SHRANK and this device may rotate. */
  | "rotate"
  /** Membership grew or is unchanged — an add / role change / ordinary
   *  commit. The routing id stays stable, which is the whole point of
   *  rotating only at removals. */
  | "skip-no-shrink"
  /** No usable before/after snapshot (first observation of the group, or a
   *  `members()` read that threw). Never rotate on an unknown delta — a
   *  missing snapshot would otherwise read as "everyone departed". */
  | "skip-unknown-membership"
  /** A shrink, but this device is a plain member (or the group has no role
   *  table). Rotating is owner-or-admin only. */
  | "skip-not-authorized";

/** Outcome of the rotation orchestration — the decision values plus what the
 *  IO half resolved to. */
export type PushRotationOutcome =
  | Exclude<PushRotationDecision, "rotate">
  /** Minted, persisted, broadcast, re-subscribed, old id retired. */
  | "rotated"
  /** This device holds no secret, so there is nothing to ROTATE. Minting is
   *  owner-only and lives in {@link ensureGroupPushRouting}; rotation
   *  deliberately never mints a first secret, so an admin can never become a
   *  second minter and diverge the group's `push_gid`. */
  | "skip-no-secret"
  /** Half-reset (`forgetDevice` in flight) — the store cannot be read or
   *  written, so nothing is touched. */
  | "skip-no-device-key";

/**
 * Stable per-member identities for the membership diff: the lowercase-hex
 * Ed25519 signature public keys from `group.members()`.
 *
 * Keys, NOT leaf indices: OpenMLS reuses a vacated leaf for the next Add, so
 * a remove-then-add pair can leave the index set identical while the
 * membership genuinely changed. The signature key is stable for a member's
 * lifetime in the group.
 *
 * Returns `null` when the snapshot cannot be read (a torn group), which the
 * decision core treats as "unknown", never as "everybody left".
 */
export function memberSignatureKeys(group: WasmGroup): string[] | null {
  try {
    const members = group.members() as Array<{
      leafIndex: number;
      signaturePublicKey: Uint8Array;
    }>;
    return members.map((m) => toHex(m.signaturePublicKey));
  } catch {
    return null;
  }
}

/**
 * Pure decision core — did the membership SHRINK, and may this device rotate?
 * No IO, so the whole matrix is directly testable (and portable verbatim to
 * iOS / Android, like the ADR-0037 `restoreVerdict` gate).
 *
 * "Shrank" = at least one member present in the previous snapshot is absent
 * from the current one. Deliberately not a count comparison: a poll batch can
 * carry a Remove AND an Add, which leaves the count equal while a member was
 * evicted — and that member must still lose its routing id.
 *
 * The authority check is owner-OR-admin (ADR-0043 ratified option (b)),
 * matching who ADR-0022 lets commit the Remove in the first place. A `null`
 * role (no role table) does NOT rotate: such a group has no owner, so nobody
 * ever minted a secret for it and there is nothing to rotate — and letting
 * every member of an un-owned group rotate would have them each mint divergent
 * secrets.
 */
export function membershipRotationDecision(args: {
  /** Member keys observed BEFORE the commits landed, or `null` if unknown. */
  previousMemberKeys: readonly string[] | null;
  /** Member keys observed AFTER, or `null` if unknown. */
  currentMemberKeys: readonly string[] | null;
  /** This device's ADR-0022 role. */
  ownRole: PushRotationRole;
}): PushRotationDecision {
  const { previousMemberKeys: before, currentMemberKeys: after } = args;
  if (before === null || after === null) return "skip-unknown-membership";
  // A first observation has no baseline to diff against.
  if (before.length === 0) return "skip-unknown-membership";
  const present = new Set(after);
  const departed = before.some((k) => !present.has(k));
  if (!departed) return "skip-no-shrink";
  if (args.ownRole !== "owner" && args.ownRole !== "admin") {
    return "skip-not-authorized";
  }
  return "rotate";
}

/**
 * Per-group rotation queue — a SERIALIZING lock, not a coalescing one.
 *
 * Two triggers can fire for one group (this device's own `onRemoveMember` and
 * a concurrent poll-loop diff). Interleaving them is one damaging case: both
 * would read the SAME old secret, both mint, the second persist wins — leaving
 * the first new `push_gid` subscribed and indexed but never notified again,
 * i.e. exactly the orphan this ADR exists to prevent.
 *
 * COALESCING (returning the in-flight promise to the second caller) rules that
 * out but voids the ADR's core property whenever the two triggers observe
 * DIFFERENT removals. An admin removes X (commit → epoch N+1); rotation R1
 * seals its envelope at N+1 and then spends three network round-trips
 * (broadcast, subscribe, unsubscribe); during that window the user removes Y.
 * Folding Y's trigger into R1 means NO rotation is ever sealed after Y left —
 * and Y, still a member at N+1, opens R1's envelope, learns the current
 * secret, and keeps its subscription live. Nothing re-rotates afterwards: the
 * removing device's own poll baseline is already post-removal, so no shrink is
 * ever seen. The "every removal already has one correctly-sequenced rotation
 * from the device that made it" argument does NOT hold, because the direct
 * path takes this same lock.
 *
 * So the second caller WAITS and then runs a full second rotation, reading the
 * first rotation's output as its own old secret. Every intermediate `push_gid`
 * is retired in turn and both callers observe a real rotation. The guarantee is
 * NOT that no envelope is ever sealed pre-eviction — R1's envelope *is*
 * pre-eviction with respect to Y, who is removed while R1 is in flight — but
 * that every eviction is followed by SOME seal at a strictly later epoch: Y's
 * own trigger queues behind R1 and seals R2 at an epoch Y is no longer in.
 * (Matches the per-`(account, group)` `Mutex` the Android client uses.)
 */
const rotating = new Map<string, Promise<PushRotationOutcome>>();

/**
 * ADR-0043 — re-mint the group's `push_routing_secret`, redistribute it, and
 * retire the superseded `push_gid`.
 *
 * **The caller MUST have already published the Remove commit.** The envelope
 * is an MLS application message: sealed at epoch N (pre-removal) the evicted
 * member is still a member of that epoch and can OPEN it, learning the new
 * secret and defeating the whole change. Sealed at N+1 it is unreadable to the
 * removed leaf — RFC 9420 forward secrecy is exactly the property doing the
 * work here. This is a hard sequencing requirement, not an optimisation.
 *
 * The steps, in the one order that works:
 *
 *   1. read the OLD secret and 2. derive the OLD `push_gid` — **before** the
 *      store write, because the `secrets` row is keyed `(account, group)` and
 *      is clobbered in place. Persist first and the id being retired can no
 *      longer be named: an undeletable orphan that keeps receiving wakes,
 *      i.e. the exact defect being fixed;
 *   3. mint the NEW secret and 4. persist it `distributed: false`;
 *   5. broadcast, marking distributed only once the send lands. That flag is
 *      the whole durability mechanism, reused verbatim from the mint path: a
 *      failed rotation broadcast leaves the row PENDING, and
 *      {@link ensureGroupPushRouting}'s re-broadcast branch — open to ANY
 *      holder — re-sends the stored (i.e. the new) secret on the next
 *      chat-open / launch. Without it, a failed rotation broadcast strands the
 *      whole group: the rotator alone on the new id, everyone else on the old
 *      one, nobody notifying either;
 *   6. subscribe under the NEW id, then 7./8. retire the OLD one
 *      ({@link retireOldPushGid}) — in that order, so the device is never
 *      unsubscribed from both at once.
 *
 * Rotation NEVER mints a first secret: with no secret held this is a no-op
 * (`skip-no-secret`). Minting stays owner-only in {@link ensureGroupPushRouting},
 * so widening rotation to admins does not create a second minter.
 *
 * Never throws — push is best-effort and a rotation failure must not unwind
 * the membership change that triggered it.
 */
export function rotatePushRoutingSecret(
  accountId: string,
  group: WasmGroup,
  ownRole: PushRotationRole,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<PushRotationOutcome> {
  if (ownRole !== "owner" && ownRole !== "admin") {
    return Promise.resolve("skip-not-authorized");
  }
  const groupIdB64 = base64Encode(group.groupId);
  const lockKey = `${accountId} ${groupIdB64}`;
  const previous = rotating.get(lockKey);

  const run = (async (): Promise<PushRotationOutcome> => {
    // Queue behind any in-flight rotation for this group, so the read of the
    // OLD secret below observes the predecessor's OUTPUT rather than racing
    // it. A predecessor failure is its own business and must not cancel this
    // rotation — the removal that triggered it still needs a re-key.
    if (previous !== undefined) {
      try {
        await previous;
      } catch {
        /* handled by whoever awaited the predecessor */
      }
    }

    const deviceKey = await getDeviceKey(accountId);
    if (deviceKey === null) return "skip-no-device-key";

    // (1)+(2) — the OLD secret, read BEFORE anything overwrites it. The gid is
    // derived from these bytes at teardown time, in `retireOldPushGid`.
    let oldSecret: Uint8Array | null = null;
    try {
      oldSecret = await getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
    } catch (err) {
      // An unreadable row cannot be rotated: the old id could never be named,
      // so a rotation would strand it subscribed forever.
      // eslint-disable-next-line no-console
      console.warn("push-routing secret unreadable, rotation skipped", err);
      return "skip-no-secret";
    }
    if (oldSecret === null) return "skip-no-secret";

    // (3)+(4) — mint + persist PENDING. Persist before broadcasting for the
    // same reason the mint path does: two divergent random secrets are worse
    // than two identical envelopes.
    const next = mintPushRoutingSecret();
    const pushGid = await persistGroupRoutingSecret(
      accountId,
      groupIdB64,
      next,
      false,
    );
    if (pushGid === null) return "skip-no-device-key";

    // (5) — redistribute at the POST-removal epoch. Failure leaves the row
    // pending for the existing retry branch; it must NOT abort the local
    // switch, which is already durable.
    await broadcastAndMarkDistributed(accountId, groupIdB64, next, broadcast);

    // (6) — arm the NEW id first, so a failure between the two never leaves
    // this device subscribed to nothing.
    try {
      await subscribeGroupUnderPushGid(accountId, groupIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("push_gid subscribe failed", err);
    }

    // (7)+(8) — retire the OLD id: unsubscribe this device's row, drop the
    // stale reverse-index entry.
    await retireOldPushGid(oldSecret);
    return "rotated";
  })();

  rotating.set(lockKey, run);
  // Release only if nothing queued behind this run in the meantime — a later
  // rotation has already replaced the entry with ITS promise, and deleting
  // that would let a third trigger race the second instead of queueing.
  // `catch` before `finally` so a rejected run never surfaces as an unhandled
  // rejection through this bookkeeping chain (the caller still sees `run`).
  void run
    .catch(() => undefined)
    .finally(() => {
      if (rotating.get(lockKey) === run) rotating.delete(lockKey);
    });
  return run;
}

/**
 * ADR-0043 — the WIRE-OBSERVED trigger: rotate when this device notices that
 * the membership shrank, i.e. when SOMEONE ELSE committed the Remove.
 *
 * A Commit surfaces from the poll loop with no membership delta attached
 * (surfacing removed leaves from `lakko-mls` would be exact, but it is an
 * invariant-#3 change to the MLS state machine and needs its own ADR), so the
 * removal is INFERRED from a `members()` snapshot taken either side of the
 * poll batch. The snapshot is already read per poll, so the diff is free.
 *
 * The device that performed the removal itself does NOT come through here: it
 * applied its own commit locally before the poll, so the next poll's baseline
 * is already post-removal and shows no shrink. It rotates directly instead,
 * via {@link rotatePushRoutingSecret}, right after its Remove commit is
 * published.
 *
 * Never throws.
 */
export async function rotatePushRoutingSecretIfMembershipShrank(args: {
  accountId: string;
  group: WasmGroup;
  ownRole: PushRotationRole;
  /** `memberSignatureKeys(group)` from BEFORE the poll applied commits. */
  previousMemberKeys: readonly string[] | null;
  /** `memberSignatureKeys(group)` from AFTER. */
  currentMemberKeys: readonly string[] | null;
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>;
}): Promise<PushRotationOutcome> {
  const decision = membershipRotationDecision({
    previousMemberKeys: args.previousMemberKeys,
    currentMemberKeys: args.currentMemberKeys,
    ownRole: args.ownRole,
  });
  if (decision !== "rotate") return decision;
  return rotatePushRoutingSecret(
    args.accountId,
    args.group,
    args.ownRole,
    args.broadcast,
  );
}
