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
 *     ({@link receivePushRoutingSecret}) — they never mint.
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
 * RECEIVE side — a non-owner (or any member) got the distribution
 * envelope: persist the secret (idempotent) + subscribe under the derived
 * `push_gid`. Best-effort; a persist that yields no `push_gid` (no device
 * key) skips the subscribe.
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
