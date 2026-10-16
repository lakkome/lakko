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
 * ## Concurrency — ONE serializing lock per routing row
 *
 * Every orchestration entry point here is a read-modify-write of the SAME
 * per-`(account, group)` store row, with `await` suspension points between
 * the read and the write. Run two of them concurrently and both read the
 * pre-write value, so the survivor is decided by microtask scheduling rather
 * than by any rule this file states. {@link withRoutingRowLock} is the single
 * mechanism that stops that: it **serializes** the whole read-decide-write of
 * each of the four entry points ({@link receivePushRoutingSecretGated},
 * {@link ensureGroupPushRouting}, {@link rotatePushRoutingSecret},
 * {@link redistributeSecretToGroup}) against every other one for that row.
 *
 * Serializing, never coalescing — see {@link withRoutingRowLock} for why the
 * distinction is the security property and not an implementation detail.
 *
 * The lock has TWO layers, because IndexedDB is shared across every tab on
 * the origin while a module-level `Map` is not: an in-process queue that
 * orders callers within this JS realm, and a **Web Lock** (`navigator.locks`)
 * of the same name that excludes OTHER tabs. See {@link withRoutingRowLock}
 * for why both are kept and what happens where `navigator.locks` is absent.
 *
 * ## Bounded hold time — the lock must never wedge (availability)
 *
 * Three of the four entry points perform network IO **while holding the
 * lock**, and that is deliberate (ADR-0043's serialization argument is
 * explicitly about the window of "three network round-trips" a rotation
 * spends broadcasting, subscribing and unsubscribing). But an unbounded await
 * there means a single hung POST wedges that `(account, group)` row
 * **forever**: every later envelope queues behind it, so the device never
 * applies a peer's rotation and keeps notifying an id an evicted member still
 * derives — the F-A consequence, reached through availability rather than
 * through the gate.
 *
 * So every network call made under the lock is bounded by
 * {@link ROUTING_NETWORK_TIMEOUT_MS} ({@link withNetworkTimeout}).
 *
 * **The trade, stated rather than left implicit.** ADR-0033 is the precedent
 * for failing OPEN on a bounded resource, on the reasoning that failing closed
 * there is self-inflicted denial of service; this lands on the same side, but
 * for a stronger reason than availability. Holding the lock forever is not
 * merely less available — it is **less safe**, because the wedged row is
 * exactly the row that stops accepting rotations. Refusing to proceed is not
 * an option that preserves anything here.
 *
 * The timeout is nevertheless **fail-closed for correctness**: it aborts only
 * the *wait*, never a decision. Each bounded call already sits in a failure
 * path that leaves the row repairable rather than advanced —
 *
 *   - a timed-out broadcast leaves `distributed` **false**, which is precisely
 *     the flag {@link ensureGroupPushRouting}'s re-broadcast branch retries;
 *   - a timed-out subscribe leaves this device unsubscribed under the CURRENT
 *     id, re-armed by the next provisioning pass;
 *   - a timed-out unsubscribe leaves a stale push-gw row that nothing ever
 *     notifies again — the residual ADR-0043 already records.
 *
 * No store write is ever made on the strength of a call that timed out, and
 * the underlying request is merely abandoned (not cancelled): should it land
 * late it is byte-identical and idempotent, an `accept-identical` for every
 * receiver.
 *
 * The two fixes compose, and the composition is load-bearing: because every
 * hold is bounded, waiting UNBOUNDEDLY for the cross-tab Web Lock is safe —
 * no tab can hold it indefinitely. Bounding the *wait* instead would mean
 * proceeding unserialized, which is the defect itself.
 *
 * The lock is deliberately taken ONLY at those four entry points; every
 * helper they call (`subscribeGroupUnderPushGid`, `pushGidForGroup`,
 * `retireOldPushGid`, `persistGroupRoutingSecret`, …) is lock-FREE. That is a
 * requirement, not an accident: `rotatePushRoutingSecret` awaits
 * `subscribeGroupUnderPushGid` **while holding the lock**, so a lock inside
 * that helper would deadlock a non-reentrant lock instantly. None of the four
 * entry points calls another, so no reentrancy is possible today — and the
 * `broadcast` callback, the one caller-supplied function invoked under the
 * lock, must not re-enter them either (both production callbacks route to
 * `ChatClient.broadcastControl`, which publishes to the DS and returns).
 */

import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { loadWasm } from "../wasm";
import { PushClient } from "./client";
import { getDevicePushToken } from "./device-token";
import {
  deleteRoute,
  ensureRouteSubId,
  getGroupRoutingRecord,
  getGroupRoutingSecret,
  getGroupRoutingState,
  getRouteSubId,
  listRoutesForAccount,
  markGroupRoutingDistributed,
  markGroupRoutingPending,
  putGroupRouting,
} from "./group-routing-store";
import { pushWakeKey } from "./wake-key";

/** `push_routing_secret` length (bytes) — mirrors the Rust
 *  `PUSH_ROUTING_SECRET_LEN`. */
export const PUSH_ROUTING_SECRET_LEN = 32;

/** Byte length of the big-endian `mint_epoch` field (ADR-0044) — mirrors
 *  the Rust `PUSH_ROUTING_SECRET_MINT_EPOCH_LEN`. */
export const PUSH_ROUTING_MINT_EPOCH_LEN = 8;

/** Total distribution-envelope length (bytes): the 29-byte domain
 *  separator + 1 version byte + the 8-byte big-endian `mint_epoch` + the
 *  32-byte secret. Used only as a cheap receive-side pre-filter before the
 *  authoritative wasm decode. Mirrors `lakko_contacts::push_routing`
 *  (29 + 1 + 8 + 32 = 70). The v1 envelope was 62 bytes and is refused
 *  outright — ADR-0044 is a hard flag day, because a v1 envelope carries no
 *  `mint_epoch` and so has no position in the total order. */
export const PUSH_ROUTING_ENVELOPE_LEN =
  29 + 1 + PUSH_ROUTING_MINT_EPOCH_LEN + PUSH_ROUTING_SECRET_LEN;

/** First byte of the envelope domain separator (`'l'` of
 *  `"lakko/push-routing-secret\0"`). The cheap pre-filter; the wasm
 *  `decodePushRoutingSecret` is the authoritative fail-closed check. */
const PUSH_ROUTING_ENVELOPE_FIRST_BYTE = 0x6c;

/** Mint a fresh `push_routing_secret` (32 CSPRNG bytes). OWNER ONLY —
 *  everyone else receives it over the distribution envelope. */
export function mintPushRoutingSecret(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(PUSH_ROUTING_SECRET_LEN));
}

/**
 * ADR-0044 — read the group's CURRENT MLS epoch, the stamp source for a
 * mint and the ceiling for the receive-side clamp.
 *
 * `undefined` when the read throws (a torn group / poisoned lock), which
 * every caller treats as fail-closed: a mint that cannot be stamped is not
 * performed, and an incoming envelope that cannot be clamped is refused.
 */
export function readGroupEpoch(group: WasmGroup): bigint | undefined {
  try {
    return group.epoch;
  } catch {
    return undefined;
  }
}

/** Derive the stable opaque `push_gid` (32 bytes) from a secret via the
 *  audited wasm keyed-BLAKE3 derivation. Deterministic: every member that
 *  holds the same secret derives the same `push_gid`. */
export async function derivePushGid(secret: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.derivePushGid(secret);
}

/**
 * Compose the E2EE distribution-envelope bytes for a secret (wasm), stamped
 * with the epoch that secret was MINTED under.
 *
 * **`mintEpoch` is NOT "now".** It is stamped once, at mint time, and then
 * travels with the secret forever: every re-broadcast recomposes the envelope
 * from the STORED secret and must replay the STORED epoch. Re-stamping the
 * current epoch on a re-broadcast would hand a stale secret a newer position
 * in the order than the live rotation it was superseded by — reproducing the
 * F-A replay (G-36 residual 9) with the ordering gate fully in place. See
 * ADR-0044 § "The mint epoch must be PERSISTED and re-emitted verbatim".
 */
export async function composeSecretEnvelope(
  mintEpoch: bigint,
  secret: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.composePushRoutingSecret(mintEpoch, secret);
}

/**
 * Attempt to decode an MLS application-message plaintext as a
 * push-routing-secret distribution envelope. Returns the 32-byte secret plus
 * its `mintEpoch`, or `null` when the bytes are not this envelope
 * (fail-closed — the authoritative check is the wasm codec's domain +
 * version + exact-length gate, which also refuses every retired v1 envelope
 * with `UnknownVersion`). A cheap length/first-byte pre-filter avoids a wasm
 * call on every chat message.
 */
export async function tryDecodeSecretEnvelope(
  bytes: Uint8Array,
): Promise<{ secret: Uint8Array; mintEpoch: bigint } | null> {
  if (
    bytes.length !== PUSH_ROUTING_ENVELOPE_LEN ||
    bytes[0] !== PUSH_ROUTING_ENVELOPE_FIRST_BYTE
  ) {
    return null;
  }
  const wasm = await loadWasm();
  try {
    const decoded = wasm.decodePushRoutingSecret(bytes);
    return { secret: decoded.secret, mintEpoch: decoded.mintEpoch };
  } catch {
    // Not a valid push-routing envelope (domain/version/length mismatch) —
    // fall through so the caller runs the normal chat parse.
    return null;
  }
}

/**
 * The canonical recency order, as the wasm core defines it once:
 * `(mintEpoch, secretBytes)`, epoch first, secret bytes lexicographically on
 * an exact tie, **strictly greater wins**.
 *
 * Exactly the shape of the wasm `pushRoutingSecretIsNewer` export, so that
 * function assigns to this type structurally. It is threaded into
 * {@link routingSecretVerdict} as a parameter rather than re-derived in
 * TypeScript **deliberately**: three clients each hand-rolling this
 * comparison is precisely the drift class that produced the F-A replay, so
 * the order lives in the audited core and every platform routes through it.
 */
export type PushRoutingSecretIsNewer = (
  incomingEpoch: bigint,
  incomingSecret: Uint8Array,
  storedEpoch: bigint,
  storedSecret: Uint8Array,
) => boolean;

/** The wasm-backed {@link PushRoutingSecretIsNewer}. The ONLY value any
 *  production call site may pass to {@link routingSecretVerdict}. */
export async function loadIsNewer(): Promise<PushRoutingSecretIsNewer> {
  const wasm = await loadWasm();
  return wasm.pushRoutingSecretIsNewer;
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
 * @param mintEpoch   ADR-0044 — the epoch this secret was MINTED under, not
 *                    the current one. On a receive it is the epoch the
 *                    accepted envelope carried, so a re-broadcast from this
 *                    device replays the minter's stamp rather than inventing
 *                    a fresher one.
 * @param distributed whether the in-group distribution envelope for this
 *                    secret is already on the wire. `false` on the owner's
 *                    mint (the broadcast follows); `true` on a receive.
 */
export async function persistGroupRoutingSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
  mintEpoch: bigint,
  distributed: boolean,
): Promise<Uint8Array | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  const pushGid = await derivePushGid(secret);
  await putGroupRouting(
    {
      accountId,
      groupIdB64,
      secret,
      pushGidHex: toHex(pushGid),
      distributed,
      mintEpoch,
    },
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
 * ADR-0078 section 3 — the `x-lakko-wake` header value for a publish to a
 * group: a wake sealed to push-gw's key, signed by the group's wake key and
 * carrying its unlock key, all derived from the group's routing secret inside
 * the wasm core. `null` when this device holds no secret for the group (the
 * ADR-0042 defer: no header, never the `group_id`) or push-gw's key cannot be
 * had right now.
 */
export async function sealedWakeForGroup(
  accountId: string,
  groupIdB64: string,
  kind: "message" | "call",
): Promise<string | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  const secret = await getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
  if (secret === null) return null;
  const gwKey = await pushWakeKey();
  if (gwKey === null) return null;
  const wasm = await loadWasm();
  return wasm.pushWakeHeaderValue(
    gwKey,
    secret,
    kind === "call" ? wasm.PushWakeKind.Call : wasm.PushWakeKind.Message,
    Date.now(),
  );
}

/**
 * Subscribe this device to push wakes for a group UNDER ITS `push_gid`
 * (ADR-0078 section 3): this browser's Web Push token sealed under the
 * group's unlock key, as the subscription row this device keeps the id of.
 *
 * Returns `"deferred"` — and sends nothing — when this device holds no
 * secret for the group (never under the leaky `group_id`), or holds no Web
 * Push token yet (`subscribeToPush` sends every subscription once it has
 * one). Returns `"subscribed"` once the row landed.
 */
export async function subscribeGroupUnderPushGid(
  accountId: string,
  groupIdB64: string,
): Promise<"subscribed" | "deferred"> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return "deferred";
  const secret = await getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
  if (secret === null) return "deferred";
  const token = getDevicePushToken();
  if (token === null) return "deferred";
  const wasm = await loadWasm();
  const pushGid = wasm.derivePushGid(secret);
  const subId = await ensureRouteSubId(accountId, toHex(pushGid), groupIdB64);
  const tokens = new wasm.PushTokenList();
  tokens.add("web_push", token);
  const sealed = wasm.sealPushSubscription(secret, subId, tokens);
  const wakePk = wasm.derivePushWakePublicKey(secret);
  // Bounded: `rotatePushRoutingSecret` awaits this WHILE HOLDING the routing-row
  // lock, so a hung POST here would wedge the row (and with it every later
  // envelope) permanently. A timeout leaves this device unsubscribed under the
  // current id — repaired by the next provisioning pass, which re-subscribes
  // unconditionally.
  await withNetworkTimeout("subscribe", () =>
    new PushClient().subscribe({ subId, pushGid, wakePk, sealed }),
  );
  return "subscribed";
}

/**
 * ADR-0078 — send every subscription of `accountId` again, sealing the
 * current token. Called when this browser's Web Push token was taken or
 * changed: each row at push-gw holds the token sealed, so a new token reaches
 * push-gw only by re-sending each one (same ids, so each row is replaced).
 * Best-effort per group; never throws.
 */
export async function resubscribeAllForAccount(accountId: string): Promise<void> {
  let routes: Awaited<ReturnType<typeof listRoutesForAccount>>;
  try {
    routes = await listRoutesForAccount(accountId);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push re-subscribe: routes unreadable", err);
    return;
  }
  const groups = new Set(routes.map((r) => r.groupIdB64));
  for (const groupIdB64 of groups) {
    try {
      await subscribeGroupUnderPushGid(accountId, groupIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("push re-subscribe failed for one group", err);
    }
  }
}

/**
 * ADR-0078 — remove every push-gw subscription `accountId` holds on this
 * browser (sign-out, Forget device, another account signing in). Must run
 * BEFORE the account's route rows are wiped: the ids live there. Best-effort
 * per row; never throws.
 */
export async function unsubscribeAllForAccount(accountId: string): Promise<void> {
  let routes: Awaited<ReturnType<typeof listRoutesForAccount>>;
  try {
    routes = await listRoutesForAccount(accountId);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push unsubscribe-all: routes unreadable", err);
    return;
  }
  for (const route of routes) {
    if (route.subIdHex === undefined || !/^[0-9a-f]{32}$/.test(route.subIdHex)) continue;
    const subId = fromHex(route.subIdHex);
    try {
      await withNetworkTimeout("unsubscribe", () => new PushClient().unsubscribe(subId));
    } catch (err) {
      // A row left behind holds a token sealed under a key push-gw only sees
      // inside a wake; it goes when the provider reports the token invalid.
      // eslint-disable-next-line no-console
      console.warn("push unsubscribe failed for one subscription", err);
    }
  }
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
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
 * subscription under it, then delete `accountId`'s reverse-index row for it
 * (another account on this browser in the same group keeps its own row).
 *
 * Called on BOTH sides of a rotation with the OLD secret — by the rotator
 * after it re-arms under the new id, and by every remaining member when the
 * gated receive path applies a CHANGED secret. Both must, and for the same
 * reason: `POST /push/unsubscribe` deletes exactly the row named by its
 * subscription id (ADR-0078), and a client knows only its OWN ids, so nobody
 * can tear down anyone else's row. If only the
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
export async function retireOldPushGid(
  accountId: string,
  oldSecret: Uint8Array,
): Promise<void> {
  let oldGidHex: string;
  try {
    oldGidHex = toHex(await derivePushGid(oldSecret));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push-routing old push_gid derivation failed", err);
    return;
  }
  try {
    // Bounded for the same reason as the subscribe above: this runs under the
    // routing-row lock on both the rotate and the receive path. A timeout
    // lands in the existing catch, i.e. a stale row nothing notifies again.
    // ADR-0078: the row is named by this device's subscription id for the
    // old push_gid; none held means this device never subscribed under it.
    const subId = await getRouteSubId(accountId, oldGidHex);
    if (subId !== null) {
      await withNetworkTimeout("unsubscribe", () => new PushClient().unsubscribe(subId));
    }
  } catch (err) {
    // The new id is already armed; a failed unsubscribe leaves a row that
    // nothing notifies again, not a broken group.
    // eslint-disable-next-line no-console
    console.warn("push_gid unsubscribe failed", err);
  }
  try {
    await deleteRoute(accountId, oldGidHex);
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
 *
 * **ADR-0044's complementary fix rides on that same `distributed: true`.**
 * The ordering gate stops a stale secret from SPREADING but does not repair
 * the device still holding one: a device whose broadcast failed sits on a
 * `pending` row and keeps re-emitting a value the group has moved past. This
 * one row write closes that on web, because it is atomic over BOTH fields —
 * applying an accepted secret replaces the stored bytes AND clears the
 * pending flag in the same `putGroupRouting`. So after any accept there is
 * nothing stale left to re-emit: the flag no longer asks for a retry, and
 * even if it did the re-broadcast branch reads the stored secret, which is
 * now the accepted one. Pinned by a test rather than left incidental.
 */
export async function receivePushRoutingSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
  mintEpoch: bigint,
): Promise<void> {
  const pushGid = await persistGroupRoutingSecret(
    accountId,
    groupIdB64,
    secret,
    mintEpoch,
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
 * Outcome of the receive-side gate ({@link receivePushRoutingSecretGated}).
 * The five `accept-*` verdicts persist + re-subscribe; the four `reject-*`
 * verdicts drop the envelope entirely (no persist, no subscribe, no
 * teardown).
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
   *  member lookup / role-table read threw, the frame carried no sender
   *  leaf, or this device's own group epoch could not be read so the
   *  ADR-0044 clamp had nothing to check against) — dropped, fail-closed. */
  | "reject-unverifiable"
  /** ADR-0044 — the envelope's `mintEpoch` is ahead of this device's OWN
   *  current group epoch, which an honest stamp can never be
   *  (`stamp <= seal epoch <= receiver's epoch at dispatch`). Refused
   *  BEFORE authority is consulted, so not even the owner can stamp
   *  `u64::MAX` and freeze the group's routing id permanently — that would
   *  escalate G-36 residual 3 from a churn an owner rotation repairs into an
   *  unrecoverable freeze. */
  | "reject-future"
  /** ADR-0044 — an AUTHORISED envelope that is not strictly newer than what
   *  this device holds under the canonical `(mintEpoch, secretBytes)` order.
   *  The F-A replay lands here: a device whose broadcast failed re-emits the
   *  PRE-rotation secret, an authorised sender relays it, and without an
   *  ordering notion every member would accept it and retire the live gid —
   *  migrating the group back onto the id an evicted member still derives,
   *  permanently. Dropped: no persist, no subscribe, no teardown. */
  | "reject-stale";

/** True when the verdict lets the envelope through. */
export function verdictAccepts(v: RoutingSecretVerdict): boolean {
  return v.startsWith("accept");
}

/** True when `key` is byte-equal to one of `set`. */
function containsKey(set: readonly Uint8Array[], key: Uint8Array): boolean {
  return set.some((k) => bytesEqual(k, key));
}

/**
 * Pure decision core of the receive-side gate — no IO, so the whole
 * accept/reject matrix is directly testable.
 *
 * ## Two questions, in this order: WHO may write, then WHICH write survives
 *
 * ADR-0042 / ADR-0043 gave this gate an **authority** notion. ADR-0044 adds
 * the **ordering** notion it never had — the omission that made a retry of
 * the current secret and a replay of a superseded one byte-indistinguishable,
 * hence the F-A rotation undo (G-36 residual 9). Authority still decides who
 * may write; the order arbitrates only between envelopes that already
 * cleared it.
 *
 * The steps, in the order the ADR fixes them:
 *
 *   1. **identical bytes → `accept-identical`.** No authority, no ordering.
 *      UNCHANGED, and this is exactly what preserves retry semantics: the
 *      any-holder re-broadcast, the post-add redistribute and any relay
 *      re-emit the identical pair, so they never reach the comparison. It
 *      matches on the SECRET BYTES and leaves the stored `(secret,
 *      mintEpoch)` untouched — updating the epoch here would be an
 *      epoch-inflation channel reachable without clearing the authority gate.
 *   2. **`incomingMintEpoch > ownEpoch` → `reject-future`.** Evaluated
 *      BEFORE authority, so not even the owner can freeze the group's routing
 *      id by stamping `u64::MAX`. Sound because an honest stamp can never
 *      exceed the receiver's own epoch (`stamp <= seal <= receiver's epoch at
 *      dispatch`), so this never refuses an honest envelope.
 *   3. the ADR-0042 / ADR-0043 **authority** gate — owner / admin /
 *      no-role-table / unverifiable — UNCHANGED, including the rank, the
 *      owner-evaluated-first branch, the fail-closed refusals, and the legacy
 *      no-role-table acceptance.
 *   4. **no stored secret → accept** (the seed path; nothing to order
 *      against).
 *   5. **strictly newer under `(mintEpoch, secretBytes)` → accept**,
 *      otherwise **`reject-stale`**.
 *
 * The authority matrix, unchanged from ADR-0043:
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
 * ## The semantic narrowing, stated plainly
 *
 * A total order and unconditional owner-priority cannot both hold. The owner
 * branch is still decided before anything is read about what is stored, so
 * the owner still clears AUTHORITY first — but its envelope now also has to
 * win the order, and **at an exact epoch tie the bytes decide and the owner
 * can lose to an admin**. That is deliberate: the property owner-priority
 * existed to provide (arrival-order independence) is preserved by a strictly
 * stronger mechanism — a total order is arrival-order independent for every
 * participant, not merely for the owner's envelope. Under ADR-0043 option (b)
 * one removal yields N+1 same-epoch mints, so the tie is the common path, not
 * a corner case.
 *
 * @param isNewer MUST be the wasm {@link loadIsNewer} export. Threaded in
 *   rather than re-derived here so the order stays defined once, in the
 *   audited core — three clients each hand-rolling it is the drift class that
 *   produced F-A in the first place.
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
  /** ADR-0044 — the epoch the STORED secret was minted under (`0n` for a
   *  legacy row, the lowest position in the order). Ignored when
   *  `storedSecret` is `null`. */
  storedMintEpoch: bigint;
  /** Set when the local secret could NOT be read (store threw, or there is
   *  no device key). `storedSecret === null` then means "unknown", not
   *  "provably absent" — so an admin write is reported as a ROTATE rather
   *  than a seed. It no longer changes the accept/reject outcome (ADR-0043
   *  option (b) lets an admin overwrite either way); it keeps the verdict
   *  honest about what was actually observed. */
  storedUnreadable: boolean;
  /** The secret carried by the incoming envelope. */
  incomingSecret: Uint8Array;
  /** ADR-0044 — the `mintEpoch` carried by the incoming envelope. */
  incomingMintEpoch: bigint;
  /** ADR-0044 — this device's OWN current group epoch, the clamp ceiling.
   *  `undefined` when `group.epoch` could not be read, which fails closed:
   *  with no ceiling there is nothing to check a stamp against. */
  ownEpoch: bigint | undefined;
  /** Set when reading the role table / member snapshot threw — authorship
   *  cannot be established, so a CHANGED secret must be refused. */
  verifyFailed: boolean;
  /** The canonical order, from the wasm core. See {@link loadIsNewer}. */
  isNewer: PushRoutingSecretIsNewer;
}): RoutingSecretVerdict {
  // (1) Idempotent case first — a re-observed identical secret changes
  // nothing, so it needs no authority and no ordering. Deliberately matched
  // on the SECRET BYTES alone: a retry that reached us over a different route
  // is still the same value, whatever epoch it claims, and the stored pair is
  // left untouched.
  if (
    args.storedSecret !== null &&
    bytesEqual(args.storedSecret, args.incomingSecret)
  ) {
    return "accept-identical";
  }
  // From here the envelope would CHANGE this device's routing id.
  //
  // (2) Clamp the future BEFORE authority (ADR-0044 § 4). An unbounded
  // ordering rule would hand any authorised writer a permanent freeze —
  // strictly worse than the churn it already has, because a churn self-repairs
  // at the next owner rotation and a freeze does not.
  if (args.ownEpoch === undefined) return "reject-unverifiable";
  if (args.incomingMintEpoch > args.ownEpoch) return "reject-future";
  // (3) Authority — unchanged.
  if (args.verifyFailed) return "reject-unverifiable";
  // Legacy / open group: no role table, hence no owner or admin set to
  // authorise against. Keep today's behaviour rather than breaking those
  // groups.
  const authorised = ((): RoutingSecretVerdict => {
    if (args.ownerKey === undefined) return "accept-no-owner";
    if (args.senderKey === undefined) return "reject-unverifiable";
    // Owner outranks everything on AUTHORITY: it may write whatever is
    // stored, in any arrival order.
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
  })();
  if (!verdictAccepts(authorised)) return authorised;
  // (4) Nothing stored to order against — the seed path.
  if (args.storedSecret === null) return authorised;
  // (5) The order decides WHICH authorised write survives.
  return args.isNewer(
    args.incomingMintEpoch,
    args.incomingSecret,
    args.storedMintEpoch,
    args.storedSecret,
  )
    ? authorised
    : "reject-stale";
}

/**
 * The per-`(account, group)` routing-row queue. ONE lock per row, shared by
 * every path that reads-then-writes it. See {@link withRoutingRowLock}.
 */
const routingRowQueue = new Map<string, Promise<unknown>>();

/**
 * Compose the routing-row lock key. A space separator is unambiguous:
 * `accountId` and `groupIdB64` are both base64, whose alphabet contains no
 * space. (The store's own row key uses a NUL for the same reason; this one
 * stays printable so a lock key can be logged.)
 */
function routingRowLockKey(accountId: string, groupIdB64: string): string {
  return `${accountId} ${groupIdB64}`;
}

/**
 * Namespace for the cross-tab Web Lock names, so a routing-row lock can never
 * collide with any other `navigator.locks` user on the origin (the lock space
 * is per-ORIGIN and flat, shared with anything else the app might ever lock).
 */
const WEB_LOCK_PREFIX = "lakko/push-routing/";

/** The slice of `navigator.locks` this module uses. Declared structurally
 *  rather than relying on `LockManager` from `lib.dom`, so the module also
 *  type-checks under the tests' `node` environment. */
type WebLockManager = {
  request: <T>(name: string, callback: () => Promise<T>) => Promise<T>;
};

/**
 * The origin's `navigator.locks`, or `undefined` where the Web Locks API is
 * unavailable.
 *
 * Resolved **lazily, per call** — never captured at module load. Two reasons:
 * a test must be able to drive both branches, and `navigator.locks` is
 * SECURE-CONTEXT ONLY, so the absent branch is a real deployment state (a
 * plain-HTTP origin, an old browser, an embedded webview) rather than a
 * defensive stub.
 */
function webLockManager(): WebLockManager | undefined {
  const nav = (globalThis as { navigator?: { locks?: unknown } }).navigator;
  const locks = nav?.locks as WebLockManager | undefined;
  return typeof locks?.request === "function" ? locks : undefined;
}

/**
 * Run `task` holding the CROSS-TAB Web Lock named after the routing row, or
 * run it directly where the API is absent.
 *
 * `navigator.locks.request` resolves with the callback's return value and
 * releases the lock when the callback settles — including when it THROWS —
 * so no explicit release is needed and a rejecting task cannot wedge the lock
 * for the other tabs.
 *
 * The wait is deliberately UNBOUNDED (no `AbortSignal`): a bounded wait that
 * gave up and ran anyway would be running unserialized, which is the very
 * defect the lock exists to close. What makes an unbounded wait safe is that
 * every hold is itself bounded — see {@link ROUTING_NETWORK_TIMEOUT_MS}.
 */
function withWebLock<T>(lockKey: string, task: () => Promise<T>): Promise<T> {
  const locks = webLockManager();
  if (locks === undefined) return task();
  return locks.request(`${WEB_LOCK_PREFIX}${lockKey}`, task);
}

/**
 * Upper bound on any single network call made while holding the routing-row
 * lock. A policy constant, NOT a measurement: it is chosen to sit far above a
 * healthy round-trip and far below "forever", and its only job is to
 * guarantee the lock is eventually released. Nothing about correctness
 * depends on the value — see the module header for why a timeout here is
 * fail-closed for correctness and why bounding it is the safer side.
 */
export const ROUTING_NETWORK_TIMEOUT_MS = 30_000;

/**
 * Bound a network call so it cannot wedge the routing row.
 *
 * Races `op()` against a timer and rejects on expiry. The underlying request
 * is ABANDONED, not cancelled — neither the DS broadcast callback nor
 * {@link PushClient} accepts an `AbortSignal`, and threading one through both
 * would be a far larger change for no additional guarantee: a late-landing
 * request is byte-identical and idempotent (`accept-identical` for every
 * receiver, an upsert at push-gw), and no store write is made on the strength
 * of a call that timed out.
 *
 * The timer is always cleared, so a fast call leaves nothing pending that
 * could hold a test's (or a page's) event loop open.
 */
async function withNetworkTimeout<T>(
  label: string,
  op: () => Promise<T>,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      op(),
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => {
          reject(
            new Error(
              `push-routing ${label} timed out after ${ROUTING_NETWORK_TIMEOUT_MS} ms`,
            ),
          );
        }, ROUTING_NETWORK_TIMEOUT_MS);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/**
 * Run `task` with exclusive access to the `(account, group)` routing row.
 *
 * ## Why a lock at all
 *
 * Each entry point reads the stored `(secret, mintEpoch)` pair, decides
 * against it, and writes — with `await`s in between (the device key, the
 * IndexedDB read, the wasm load). The receive path's only production caller
 * is fire-and-forget inside a poll-batch loop that never awaits, so two
 * envelopes from one batch are launched in the same synchronous turn and both
 * complete their read of the PRE-BATCH row before either persists.
 *
 * That is not a theoretical race — it **restores F-A** (G-36 residual 9), the
 * finding this whole ADR-0044 arc exists to close, with no malice and no
 * crash. A device holding `S_OLD@10` polls `[rotation S1@11, stale S_OLD@10]`:
 * the rotation reads `(S_OLD, 10)`, accepts, persists `(S1, 11)` and retires
 * `gid(S_OLD)`; the replay reads that SAME pre-batch row, so its bytes are
 * byte-IDENTICAL to it and it takes the `accept-identical` fast path — which
 * by design short-circuits BOTH the future clamp and the order — re-persisting
 * `S_OLD` one microtask behind. The device ends on the superseded secret: the
 * id the evicted member still derives and is still subscribed to.
 *
 * It also undercuts G-36 residual 7. Under ADR-0043 option (b) ONE removal
 * yields N+1 same-epoch rotations, which typically arrive in a single batch,
 * so all N+1 gates compare against one snapshot and the winner is scheduling
 * order rather than the canonical `(mintEpoch, secretBytes)` total order.
 *
 * ## SERIALIZING, never coalescing
 *
 * A second caller **queues and then runs in full** against the first's
 * committed result. It is NOT folded into the in-flight run.
 *
 * That distinction is the security property. Coalescing here was
 * `lakko-security` **F1** in the ADR-0043 review: web and iOS folded a second,
 * DISTINCT removal into an in-flight rotation, so no rotation was ever sealed
 * after the second member left and that member kept a live subscription. The
 * same reasoning applies verbatim to the receive path — folding a second
 * envelope into the first would silently drop it, and a dropped envelope is a
 * device stranded on a superseded secret.
 *
 * ## The lock spans the READ as well as the write
 *
 * Acquiring it after the stored-record read would fix nothing: the read is
 * exactly what goes stale. `task` must therefore contain the whole
 * read-decide-write, which is why each entry point wraps its ENTIRE body.
 *
 * ## TWO layers, because a module `Map` is not the sharing boundary
 *
 * {@link routingRowQueue} is module-level state, so it orders callers within
 * ONE JS realm. The row it protects is not: `lakko-push-routing` is an
 * IndexedDB database shared by every tab on the origin. Two tabs open on the
 * same account therefore read-modify-write the same row with no mutual
 * exclusion at all, so the in-process queue alone leaves the race it was
 * added to close still reachable — usually as a lost update (G-36 residual 7),
 * and in the narrow interleaving above as the full F-A confidentiality shape:
 * the device ends on the PRE-rotation secret and keeps notifying an id the
 * evicted member still derives.
 *
 * So the task additionally runs under a **Web Lock** ({@link withWebLock}) of
 * the same name, which IS per-origin and therefore cross-tab.
 *
 * **Both layers are kept.** `navigator.locks` alone would in principle do the
 * whole job — it serializes same-realm callers too — but:
 *
 *   1. it is **secure-context only**, so `undefined` is a real deployment
 *      state, not a hypothetical. With one mechanism, that state is a silent
 *      unserialized run: the failure mode is invisible and is precisely the
 *      defect. The in-process queue is the floor that state falls back to —
 *      degraded (same-tab only) rather than absent;
 *   2. the queue's ordering is captured **synchronously**, so a burst of
 *      calls in one turn runs in CALL order — for the poll loop, DS journal
 *      order. That is a property this file states and tests assert, and it is
 *      cheaper to keep owning it than to re-derive it from another
 *      implementation's grant order.
 *
 * The two nest in a fixed order (queue, then Web Lock) under an identical key,
 * and no entry point re-enters another, so there is no lock-ordering hazard
 * and no reentrancy: the deadlock shapes both would need are unreachable.
 *
 * ## Ordering, failure isolation, release
 *
 *   - the predecessor is captured and the queue entry replaced
 *     **synchronously**, so runs execute in call order — for the poll loop
 *     that is DS journal order, the total order every client already agrees
 *     on;
 *   - a predecessor's rejection is swallowed HERE (`try`/`catch` around the
 *     wait) so it never becomes contagious — its own caller still observes it,
 *     and the queued task's trigger (an eviction, an arriving envelope) is
 *     still real regardless of why the previous one failed;
 *   - release is TAIL-GUARDED: the entry is dropped only if it is still this
 *     run's promise, so a later task that already replaced it keeps its place
 *     and a third caller queues behind IT rather than racing it. The
 *     `.catch()` before `.finally()` keeps this bookkeeping chain from
 *     surfacing an unhandled rejection (the caller still sees the real one).
 */
export function withRoutingRowLock<T>(
  accountId: string,
  groupIdB64: string,
  task: () => Promise<T>,
): Promise<T> {
  const lockKey = routingRowLockKey(accountId, groupIdB64);
  const previous = routingRowQueue.get(lockKey);

  const run = (async (): Promise<T> => {
    if (previous !== undefined) {
      try {
        await previous;
      } catch {
        /* the predecessor's own caller owns that failure */
      }
    }
    // Cross-tab layer INSIDE the in-realm one: the queue has already fixed
    // this realm's order, so the Web Lock is only ever contended by OTHER
    // tabs. Where the API is absent this is a direct `task()` call.
    return withWebLock(lockKey, task);
  })();

  routingRowQueue.set(lockKey, run);
  void run
    .catch(() => undefined)
    .finally(() => {
      if (routingRowQueue.get(lockKey) === run) routingRowQueue.delete(lockKey);
    });
  return run;
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
 * ADR-0044 — the envelope must also be strictly NEWER than what this device
 * holds, and no further ahead than this device's own group epoch. Both new
 * refusals return before the persist and before the teardown, so a refused
 * envelope changes nothing at all: no store write, no subscribe, and — the
 * one that carries the security value — no `retireOldPushGid`, so a stale
 * replay can never retire the live routing id.
 *
 * Never throws — the poll loop calls this per message.
 *
 * **SERIALIZED per `(account, group)`** ({@link withRoutingRowLock}). Without
 * it this whole function is a read-modify-write across `await` boundaries
 * whose only production caller is fire-and-forget inside a batch loop that
 * never awaits — so two envelopes from ONE batch both read the pre-batch row
 * and the survivor is decided by microtask scheduling. Concretely that
 * restores F-A: the stale replay is judged against the row the rotation has
 * not yet written, matches it byte-for-byte, and takes the `accept-identical`
 * fast path straight past the clamp and the order. The lock must span the
 * stored-record READ, which is why it wraps the entire body rather than just
 * the persist.
 *
 * @returns the verdict, so the caller can log / assert on it.
 */
export function receivePushRoutingSecretGated(
  accountId: string,
  group: WasmGroup,
  groupIdB64: string,
  senderLeaf: number | undefined,
  secret: Uint8Array,
  mintEpoch: bigint,
): Promise<RoutingSecretVerdict> {
  return withRoutingRowLock(accountId, groupIdB64, async () => {
    let storedSecret: Uint8Array | null = null;
    let storedMintEpoch = 0n;
    let storedUnreadable = false;
    try {
      const deviceKey = await getDeviceKey(accountId);
      if (deviceKey === null) {
        // Half-reset: the store cannot be read at all, so "this device holds
        // none" is unprovable. (A persist would no-op for the same reason.)
        storedUnreadable = true;
      } else {
        // ONE read for the pair — a torn (secret, epoch) read could rank a
        // superseded secret at a live epoch.
        const record = await getGroupRoutingRecord(
          accountId,
          groupIdB64,
          deviceKey,
        );
        storedSecret = record?.secret ?? null;
        storedMintEpoch = record?.mintEpoch ?? 0n;
      }
    } catch {
      // Unreadable local secret — cannot prove "identical" NOR "absent"; the
      // role check below still decides, fail-closed on the admin-seed branch.
      storedSecret = null;
      storedMintEpoch = 0n;
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
        // ADR-0050 — authority is the PERSON's: the role table names account
        // keys, so the sender leaf resolves to its account key.
        const members = group.members() as Array<{
          leafIndex: number;
          signaturePublicKey: Uint8Array;
          accountKey?: Uint8Array;
        }>;
        const hit = members.find((m) => m.leafIndex === senderLeaf);
        senderKey =
          hit === undefined
            ? undefined
            : hit.accountKey !== undefined && hit.accountKey.length === 32
              ? hit.accountKey
              : hit.signaturePublicKey;
      }
    } catch {
      // A torn group / unreadable role table — authorship is unverifiable.
      verifyFailed = true;
      adminKeys = [];
    }

    // ADR-0044 — the clamp ceiling. `undefined` (a torn group) fails closed in
    // the decision core, since with no ceiling a stamp cannot be bounded.
    const ownEpoch = readGroupEpoch(group);

    const verdict = routingSecretVerdict({
      ownerKey,
      adminKeys,
      senderKey,
      storedSecret,
      storedMintEpoch,
      storedUnreadable,
      incomingSecret: secret,
      incomingMintEpoch: mintEpoch,
      ownEpoch,
      verifyFailed,
      isNewer: await loadIsNewer(),
    });

    if (!verdictAccepts(verdict)) {
      // eslint-disable-next-line no-console
      console.warn(
        "push-routing secret REFUSED (ADR-0042 role gate / ADR-0044 order)",
        verdict,
        "senderLeaf",
        senderLeaf,
        // ADR-0044 open question: neither epoch is secret, and a stale replay
        // is invisible in the field without them.
        "mintEpoch",
        mintEpoch,
        "storedMintEpoch",
        storedMintEpoch,
      );
      return verdict;
    }

    // ADR-0044 — an `accept-identical` re-persist must leave the STORED epoch
    // untouched. It is the one accept that clears NEITHER the authority gate
    // nor the future clamp (it short-circuits both, deliberately, so a retry
    // stays idempotent for any sender), so writing the incoming stamp here
    // would be an **epoch-inflation channel open to a plain member**: relay the
    // group's current secret bytes stamped `u64::MAX` and every later rotation
    // — including the owner's — is refused `reject-stale` forever. That is
    // exactly the permanent freeze the clamp exists to prevent, reached around
    // it. The write itself is kept (it re-arms the subscription and is
    // otherwise a content no-op).
    const epochToPersist =
      verdict === "accept-identical" ? storedMintEpoch : mintEpoch;

    try {
      await receivePushRoutingSecret(
        accountId,
        groupIdB64,
        secret,
        epochToPersist,
      );
      // ADR-0043 — the applied secret SUPERSEDED one this device held, so the
      // id it was subscribed under is now dead. Retire it: this device is the
      // only party that can drop its own push-gw row. Read from the snapshot
      // taken BEFORE the persist above — the `secrets` row is overwritten in
      // place, so after the write the old bytes are unrecoverable.
      if (storedSecret !== null && !bytesEqual(storedSecret, secret)) {
        await retireOldPushGid(accountId, storedSecret);
      }
    } catch (err) {
      // Best-effort, exactly as before the gate — never throw out of the
      // poll loop.
      // eslint-disable-next-line no-console
      console.warn("push-routing secret receive failed", err);
    }
    return verdict;
  });
}

/**
 * Send the distribution envelope for a secret and, ONLY once the broadcast
 * resolves, record it as distributed. Never throws — a failure leaves the
 * flag `false`, which is precisely what makes the next provisioning pass
 * retry it.
 *
 * The envelope is recomposed from the secret on every attempt, so nothing
 * about the wire bytes needs to be spooled between attempts.
 *
 * ADR-0044 — `mintEpoch` is a PARAMETER, never read from the group here.
 * Every caller has to say explicitly which epoch it is replaying: a MINT
 * passes the epoch it just stamped, a RE-BROADCAST passes the STORED epoch.
 * Sourcing it inside this helper would silently re-stamp on every retry,
 * which is the one mistake that turns the whole ordering gate into a no-op.
 */
async function broadcastAndMarkDistributed(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
  mintEpoch: bigint,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<void> {
  try {
    // Compose (wasm, local) OUTSIDE the race; only the send is bounded. A
    // timeout skips `markGroupRoutingDistributed`, so the row stays `pending`
    // and the re-broadcast branch retries it — the failure path this helper
    // was already built around, reached by a new cause.
    const envelope = await composeSecretEnvelope(mintEpoch, secret);
    await withNetworkTimeout("broadcast", () => broadcast(envelope));
    await markGroupRoutingDistributed(accountId, groupIdB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push-routing secret broadcast failed", err);
    return;
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
 * **SERIALIZED per `(account, group)`** on the SAME {@link withRoutingRowLock}
 * every other routing-row writer takes. Provisioning can fire twice in one
 * session (the rehydrate legacy-sweep and the chat view's onMount), and it
 * previously held a private COALESCING lock: a second concurrent caller got
 * the in-flight promise back rather than running.
 *
 * Serializing preserves the mint-once property that lock existed for, and by
 * a stronger mechanism. `getGroupRoutingState` is read INSIDE the lock, so a
 * queued second pass observes the first pass's committed row: `absent` has
 * become `distributed`, the mint branch does not fire, and no second random
 * secret is generated. Coalescing only held while the two calls genuinely
 * overlapped; the in-lock re-read holds however they are staggered.
 *
 * Sharing the lock with the receive + rotate paths is what it buys: a
 * provisioning pass and an arriving envelope (or a rotation) can no longer
 * read the same row and write over each other.
 *
 * @param broadcast posts the distribution-envelope bytes to the group
 *                  journal as an E2EE MLS application message (persist-
 *                  before-publish). Invoked on the owner's mint branch and on
 *                  ANY holder's re-broadcast branch. Must not re-enter this
 *                  module's entry points — it is called under the lock.
 */
export function ensureGroupPushRouting(
  accountId: string,
  group: WasmGroup,
  identity: WasmIdentity,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<void> {
  const groupIdB64 = base64Encode(group.groupId);
  return withRoutingRowLock(accountId, groupIdB64, async (): Promise<void> => {
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
        // ADR-0044 — the ONE place in this function that reads the group's
        // epoch: a mint stamps the current one. A failed read fails closed
        // (no mint, hence no push) rather than stamping a guess, since an
        // unordered secret is exactly what v2 exists to eliminate.
        const mintEpoch = readGroupEpoch(group);
        if (mintEpoch === undefined) {
          // eslint-disable-next-line no-console
          console.warn("push-routing mint skipped: group epoch unreadable");
        } else {
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
            mintEpoch,
            false,
          );
          if (pushGid !== null) {
            // Distribute to founding members over the E2EE journal.
            await broadcastAndMarkDistributed(
              accountId,
              groupIdB64,
              secret,
              mintEpoch,
              broadcast,
            );
          }
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
      //
      // ADR-0044 — this branch REPLAYS the stored `mintEpoch` verbatim. It is
      // the exact site the F-A replay travels through, so re-stamping the
      // current epoch here would hand the stale secret a newer position than
      // the rotation that superseded it and reproduce the finding with the
      // ordering gate fully armed. Read as a PAIR from one row so the two can
      // never come from either side of a concurrent receive.
      let record: { secret: Uint8Array; mintEpoch: bigint } | null = null;
      try {
        const deviceKey = await getDeviceKey(accountId);
        record =
          deviceKey === null
            ? null
            : await getGroupRoutingRecord(accountId, groupIdB64, deviceKey);
      } catch (err) {
        // Half-reset or an undecryptable row — nothing to re-broadcast.
        // eslint-disable-next-line no-console
        console.warn(
          "push-routing secret unreadable, re-broadcast skipped",
          err,
        );
      }
      if (record !== null) {
        await broadcastAndMarkDistributed(
          accountId,
          groupIdB64,
          record.secret,
          record.mintEpoch,
          broadcast,
        );
      }
    }

    // Subscribe (or defer) under push_gid. Owner just minted → subscribes;
    // a member with no secret → deferred (never under group_id).
    //
    // Fire-and-forget, so it does NOT extend the lock — and it must not, since
    // `subscribeGroupUnderPushGid` re-reads the row and taking the lock inside
    // it would deadlock the paths that await it while holding it.
    subscribeGroupUnderPushGidBestEffort(accountId, groupIdB64);
  });
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
 *
 * ADR-0044 — this is a RE-BROADCAST of an already-minted secret, so it
 * replays the STORED `mintEpoch`. It must not stamp the post-add epoch: the
 * secret did not change, only its audience did, and re-stamping would let a
 * routine add-member out-rank a rotation minted at a higher epoch.
 *
 * **SERIALIZED per `(account, group)`** on the shared
 * {@link withRoutingRowLock}, for that same reason. This reads the stored
 * record, spends a network round-trip broadcasting it, then writes the
 * `distributed` flag — so unserialised it can read the PRE-rotation secret and
 * put a superseded value back on the wire while a rotation is in flight,
 * which is the F-A replay shape emitted by this device rather than relayed to
 * it. Under the lock it reads the rotation's committed output instead.
 *
 * Still rejects with the broadcast's error (the lock propagates it verbatim),
 * and a rejection here never poisons the queue — the next task swallows the
 * predecessor's failure.
 */
export async function redistributeSecretToGroup(
  accountId: string,
  group: WasmGroup,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<void> {
  // Read inside the async function so a torn `group.groupId` surfaces as a
  // REJECTION, exactly as before — the fire-and-forget call site catches a
  // rejected promise, not a synchronous throw. The lock is still acquired
  // synchronously relative to the call: nothing is awaited before it.
  const groupIdB64 = base64Encode(group.groupId);
  return withRoutingRowLock(accountId, groupIdB64, async (): Promise<void> => {
    const deviceKey = await getDeviceKey(accountId);
    if (deviceKey === null) return;
    const record = await getGroupRoutingRecord(
      accountId,
      groupIdB64,
      deviceKey,
    );
    if (record === null) return;
    try {
      const envelope = await composeSecretEnvelope(
        record.mintEpoch,
        record.secret,
      );
      // Bounded — this send is awaited under the routing-row lock. A timeout
      // takes the existing failure path: mark pending (so the re-broadcast
      // branch retries) and reject, exactly as a DS error already does.
      await withNetworkTimeout("redistribute", () => broadcast(envelope));
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
  });
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
// It cannot be closed by unsubscribing, because `POST /push/unsubscribe`
// deletes exactly the row named by its subscription id (ADR-0078) and a
// client knows only its OWN ids — no remaining member can reach the
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
  | "skip-not-authorized"
  /**
   * ADR-0045 UX §6.2.8 item 3 — the departing member is US. This device's own
   * membership has ended, so it must not rotate on its own eviction.
   *
   * It would otherwise, and that is the finding rather than a precaution: a
   * `Remove` rewrites no `GroupContext`, so an evicted admin's role table
   * still names it an admin, {@link membershipRotationDecision} sees a member
   * depart and returns `"rotate"` — and the rotation then throws at the
   * broadcast, because the envelope is an MLS application message and
   * `createApplicationMessage` is refused for an inactive group.
   *
   * The doc on {@link rotatePushRoutingSecretIfMembershipShrank} reasons
   * correctly about the device *performing* a removal (its baseline is already
   * post-removal, so it sees no shrink) and has no case at all for the device
   * *receiving its own*.
   */
  | "skip-self-evicted";

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
  | "skip-no-device-key"
  /** ADR-0044 — `group.epoch` could not be read, so the new secret could not
   *  be stamped. Fail-closed: an unstamped secret has no position in the
   *  total order, which is exactly what the v2 envelope exists to eliminate,
   *  so the rotation is abandoned rather than performed unordered. The old
   *  secret stays in place and a later trigger retries. */
  | "skip-unknown-epoch";

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
  /**
   * ADR-0045 UX §6.2.8 item 3 — has THIS device's own membership ended?
   *
   * Evaluated FIRST, ahead of the membership diff and ahead of the authority
   * check, because a device that is out of the group has no business rotating
   * whatever its stale role table says — and because the answer must not
   * depend on which of two skip reasons happened to be checked first.
   *
   * Optional so every existing caller and every existing test reads exactly as
   * it did; absent means "not evicted", which is the pre-ADR-0045-§6.2
   * behaviour byte for byte.
   */
  selfEvicted?: boolean;
}): PushRotationDecision {
  const { previousMemberKeys: before, currentMemberKeys: after } = args;
  if (args.selfEvicted === true) return "skip-self-evicted";
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

/*
 * ## Why rotation takes the routing-row lock, and why SERIALIZING
 *
 * (The mechanism is the shared {@link withRoutingRowLock}; this is the
 * rotation-specific case for it, kept beside the rotation because the X / Y
 * scenario below is what fixes the semantics as serializing rather than
 * coalescing.)
 *
 * Two triggers can fire for one group (this device's own `onRemoveMember` and
 * a concurrent poll-loop diff). Interleaving them is one damaging case: both
 * would read the SAME old secret, both mint, the second persist wins — leaving
 * the first new `push_gid` subscribed and indexed but never notified again,
 * i.e. exactly the orphan ADR-0043 exists to prevent.
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
 *
 * Since ADR-0044's serialization fix this is no longer a rotation-private
 * queue: rotation shares ONE lock per row with the receive + provisioning +
 * redistribute paths, so a rotation can no longer interleave with an arriving
 * envelope either (the recorded F-E residual).
 */

/**
 * ADR-0043 — re-mint the group's `push_routing_secret`, redistribute it, and
 * retire the superseded `push_gid`.
 *
 * **The caller MUST have already published the Remove commit** — C-61, and
 * under ADR-0044 it carries TWO guarantees, not one. The envelope is an MLS
 * application message: sealed at epoch N (pre-removal) the evicted member is
 * still a member of that epoch and can OPEN it, learning the new secret and
 * defeating the whole change; sealed at N+1 it is unreadable to the removed
 * leaf (RFC 9420 forward secrecy). ADR-0044 adds the second: the new secret
 * is STAMPED with `group.epoch` read here, so publishing the Remove first is
 * also what makes the rotation strictly out-rank the secret it replaces. Get
 * the order wrong and the stamp is the pre-removal epoch, which TIES with a
 * stale secret minted at the same epoch — the tie falls to the byte
 * comparison and the rotation may simply lose. The guarantee degrades from
 * "always wins" to "coin flip", silently. Use
 * {@link publishRemoveThenRotate} at the call site, which is where that
 * ordering is pinned by a test.
 *
 * The steps, in the one order that works:
 *
 *   1. read the OLD secret and 2. derive the OLD `push_gid` — **before** the
 *      store write, because the `secrets` row is keyed `(account, group)` and
 *      is clobbered in place. Persist first and the id being retired can no
 *      longer be named: an undeletable orphan that keeps receiving wakes,
 *      i.e. the exact defect being fixed;
 *   3. read the CURRENT (post-removal) epoch as the mint stamp;
 *   4. mint the NEW secret and persist it `distributed: false`;
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
 *
 * **SERIALIZED per `(account, group)`** on the shared
 * {@link withRoutingRowLock} — see the block above for the rotation-specific
 * case, and the helper for why it must serialize rather than coalesce. The
 * whole body runs after the queue wait, which is what lets the OLD-secret read
 * AND the epoch stamp observe the predecessor's committed output.
 */
export function rotatePushRoutingSecret(
  accountId: string,
  group: WasmGroup,
  ownRole: PushRotationRole,
  broadcast: (envelopeBytes: Uint8Array) => Promise<void>,
): Promise<PushRotationOutcome> {
  // The authority check needs no row state, so it stays OUTSIDE the lock — an
  // unauthorised trigger must not queue behind an in-flight rotation just to
  // report a verdict that never depended on it.
  if (ownRole !== "owner" && ownRole !== "admin") {
    return Promise.resolve("skip-not-authorized");
  }
  const groupIdB64 = base64Encode(group.groupId);

  return withRoutingRowLock(
    accountId,
    groupIdB64,
    async (): Promise<PushRotationOutcome> => {
      const deviceKey = await getDeviceKey(accountId);
      if (deviceKey === null) return "skip-no-device-key";

      // (1)+(2) — the OLD secret, read BEFORE anything overwrites it. The gid is
      // derived from these bytes at teardown time, in `retireOldPushGid`.
      let oldSecret: Uint8Array | null = null;
      try {
        oldSecret = await getGroupRoutingSecret(
          accountId,
          groupIdB64,
          deviceKey,
        );
      } catch (err) {
        // An unreadable row cannot be rotated: the old id could never be named,
        // so a rotation would strand it subscribed forever.
        // eslint-disable-next-line no-console
        console.warn("push-routing secret unreadable, rotation skipped", err);
        return "skip-no-secret";
      }
      if (oldSecret === null) return "skip-no-secret";

      // (3) — ADR-0044: stamp the mint with the group's CURRENT epoch, read
      // HERE, after the queue wait, so a rotation queued behind a predecessor
      // still stamps the epoch it actually mints at. C-61 is what makes this
      // stamp meaningful: the caller published the Remove commit first, so
      // `group.epoch` is already the POST-removal epoch and this mint strictly
      // out-ranks anything the evicted member could have been holding. If C-61
      // were violated the stamp would be the PRE-removal epoch, tie with the
      // stale secret, and fall to the byte tiebreak — a coin flip instead of a
      // guarantee. Fail-closed on an unreadable epoch.
      const mintEpoch = readGroupEpoch(group);
      if (mintEpoch === undefined) return "skip-unknown-epoch";

      // (4) — mint + persist PENDING. Persist before broadcasting for the
      // same reason the mint path does: two divergent random secrets are worse
      // than two identical envelopes.
      const next = mintPushRoutingSecret();
      const pushGid = await persistGroupRoutingSecret(
        accountId,
        groupIdB64,
        next,
        mintEpoch,
        false,
      );
      if (pushGid === null) return "skip-no-device-key";

      // (5) — redistribute at the POST-removal epoch. Failure leaves the row
      // pending for the existing retry branch; it must NOT abort the local
      // switch, which is already durable.
      await broadcastAndMarkDistributed(
        accountId,
        groupIdB64,
        next,
        mintEpoch,
        broadcast,
      );

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
      await retireOldPushGid(accountId, oldSecret);
      return "rotated";
    },
  );
}

/**
 * **C-61 — publish the Remove commit, THEN rotate.** The single call site for
 * this device's own removal path, and the reason that ordering is testable at
 * all rather than being two adjacent statements anyone can reorder.
 *
 * Both halves of what C-61 buys depend on `publishRemove` having RESOLVED
 * before `rotate` is even started:
 *
 *   - **Forward secrecy (ADR-0043).** The rotation envelope is an MLS
 *     application message. Sealed before the Remove commit it is sealed at
 *     the pre-removal epoch, where the evicted member is still a member and
 *     can open it — learning the very secret the rotation exists to deny it.
 *   - **Recency (ADR-0044).** {@link rotatePushRoutingSecret} stamps the new
 *     secret with `group.epoch` read at mint time. After the commit that is
 *     the post-removal epoch, so the rotation strictly out-ranks the secret
 *     it replaces and a stale re-broadcast of that older secret is refused
 *     `reject-stale` by every member. Before the commit it is the PRE-removal
 *     epoch — the same epoch the stale secret carries — so the two TIE, the
 *     tie falls to the 32 secret bytes, and the rotation wins or loses by a
 *     coin flip with no signal either way.
 *
 * ADR-0043 required this ordering but enforced it only structurally, by call-
 * site sequencing, with no test on any client. ADR-0044 promotes it from a
 * property that protects one guarantee to a precondition of two, so it is
 * pinned here — where a test can drive both callbacks and observe which epoch
 * the emitted envelope was stamped at.
 *
 * The rotation is deliberately fire-and-forget: push is best-effort and a
 * rotation failure must never unwind the membership change that triggered it.
 * "Ordered" means the rotation is not STARTED until the removal has landed,
 * not that the caller waits for it.
 *
 * @returns whatever `publishRemove` resolved to, so the caller keeps its
 *          commit id (echo suppression, notices).
 */
export async function publishRemoveThenRotate<T>(args: {
  /** Applies + publishes the Remove commit, advancing the local MLS epoch.
   *  Its rejection propagates — a removal that failed must not rotate. */
  publishRemove: () => Promise<T>;
  /** Started ONLY after `publishRemove` resolved. `undefined` skips the
   *  rotation entirely (no session — nothing to rotate for). */
  rotate?: (() => Promise<PushRotationOutcome>) | undefined;
  /** Receives a rotation failure. The removal itself is already durable. */
  onRotationError?: ((err: unknown) => void) | undefined;
}): Promise<T> {
  const result = await args.publishRemove();
  const rotate = args.rotate;
  if (rotate !== undefined) {
    void rotate().catch((err: unknown) => args.onRotationError?.(err));
  }
  return result;
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
  /** ADR-0045 UX §6.2.8 item 3 — see {@link membershipRotationDecision}. Read
   *  from `Group.isActive` on the SAME commit batch this diff was taken
   *  around, so the eviction and the shrink it causes are judged together. */
  selfEvicted?: boolean;
}): Promise<PushRotationOutcome> {
  const decision = membershipRotationDecision({
    previousMemberKeys: args.previousMemberKeys,
    currentMemberKeys: args.currentMemberKeys,
    ownRole: args.ownRole,
    ...(args.selfEvicted === undefined ? {} : { selfEvicted: args.selfEvicted }),
  });
  if (decision !== "rotate") return decision;
  return rotatePushRoutingSecret(
    args.accountId,
    args.group,
    args.ownRole,
    args.broadcast,
  );
}
