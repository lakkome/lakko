// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed persistence for the ADR-0042 opaque per-group push
 * routing id.
 *
 * ## What this store holds
 *
 * Three things, in ONE database (`lakko-push-routing`) so the background
 * service worker can open a single DB:
 *
 *   1. **`secrets`** — the per-`(account, group)` `push_routing_secret`
 *      (32 bytes of E2EE key material minted once by the group OWNER,
 *      ADR-0042). It derives the opaque `push_gid`, so it is sensitive
 *      and stored **device-key-encrypted at rest** (AES-256-GCM under
 *      the non-extractable device root key), the same posture as the MLS
 *      snapshot / account-identity seed / session token. Foreground-only
 *      — the SW never needs the raw secret. The row also carries a
 *      plaintext `distributed` flag recording whether the in-group
 *      distribution envelope was ever confirmed on the wire (see below).
 *   2. **`account_routes`** — the **`(account, push_gid) → group_id` reverse
 *      index**, keyed by the account AND the lowercase-hex `push_gid`
 *      (ADR-0042, amended 2026-09-30: two accounts in one group derive the
 *      same `push_gid`, so a `push_gid`-only key let the last writer take the
 *      row, and a wake could be decrypted as the wrong account). This is the map
 *      the ADR-0041 silent-push decrypt path reverse-resolves in Sprint 4
 *      (`resolveAccount(pushGidHex)` → the real `group_id` for the DS
 *      fetch). It is stored **plaintext**: `push_gid` is a one-way
 *      BLAKE3 derivation (reveals nothing) and `group_id` is a public DS
 *      routing address already shared with the server — exactly the
 *      rationale that keeps `knownGroupIds` unencrypted in the MLS
 *      snapshot store. Plaintext is REQUIRED so the SW can read it
 *      without the device key.
 *   3. **`wake_binding`** — ONE row naming the device push pseudonym this
 *      browser registered and the account it registered it for (ADR-0042,
 *      "A wake is shown only for the signed-in account"). The service worker
 *      handles a wake only when the wake's `d` equals that pseudonym, and
 *      only as that account. Written after a successful registration,
 *      cleared FIRST by Log out and Forget device. Plaintext: a random
 *      pseudonym and an account id, no key material.
 *
 * The first two stores age differently, which ADR-0043 makes load-bearing: a
 * rotation OVERWRITES the `secrets` row in place (same `(account, group)`
 * key) but only ADDS to `routes` (keyed by the freshly derived
 * `pushGidHex`). The superseded route entry is therefore retired explicitly,
 * via {@link deleteRoute}.
 *
 * ## The `distributed` flag — why a boolean, and not an outbox
 *
 * The owner persists its freshly minted secret BEFORE broadcasting the
 * distribution envelope (minting twice is worse than broadcasting twice).
 * If that broadcast then fails — offline, DS error — the secret is already
 * durable, so a presence-only guard (`is a secret held?`) short-circuits on
 * every later launch and the envelope is NEVER re-sent: the other members
 * defer forever and the whole group silently loses push.
 *
 * The fix records whether the envelope was **confirmed on the wire**. Unlike
 * the ADR-0040 commit outbox — which must store the commit BYTES, because
 * they are unrecoverable once the in-memory MLS state moves on — the
 * distribution envelope is **recomputable from the stored row at any time**
 * (`composePushRoutingSecret(mintEpoch, secret)` — both operands durable
 * here, which is exactly why the ADR-0044 stamp had to become a stored field
 * rather than a transient wire one). So one durable boolean is the whole
 * retry mechanism; there is nothing to spool.
 *
 * The flag is per-`(account, group)`, NOT per-member, so it must be cleared
 * again whenever a LATER send of the same envelope fails. The case that
 * matters is the post-add-member **redistribute**: the mint broadcast landed
 * (`distributed`), a member was added, and the redistribute that carries the
 * secret to that new member failed. Left `true` the row looks settled, no
 * pass ever re-sends, and the new member — for whom that redistribute was the
 * ONLY source of the secret — defers push forever. So a failed send flips the
 * row back to pending ({@link markGroupRoutingPending}) and the same
 * re-broadcast branch repairs it; the re-send is byte-identical, hence an
 * idempotent `accept-identical` for every existing member.
 *
 * A row written by a build that predates the flag carries no `distributed`
 * field. Such rows are read as **`true`**: those groups were provisioned by
 * the old code and are working today, so treating `undefined` as "not yet
 * distributed" would re-broadcast for every group on the first launch after
 * an upgrade. Only an explicit `false` — written by the mint path in this
 * build — means "pending".
 *
 * ## The `mintEpoch` field — ADR-0044, durable state, NOT a wire field
 *
 * The v2 distribution envelope carries an 8-byte `mint_epoch` the minter
 * stamps from `Group.epoch` at MINT time, and the receive gate orders
 * envelopes by the pair `(mintEpoch, secretBytes)`. That order is only sound
 * if the stamp **travels with the secret for that secret's whole life**: every
 * re-broadcast recomposes the envelope from the stored secret, so it must
 * replay the epoch the secret was MINTED under, never the current one. A
 * re-stamp at send time would make a stale secret out-rank the live one and
 * reproduce the very F-A replay the order exists to close — with the ordering
 * gate fully in place and working as specified.
 *
 * So the epoch is durable per-`(account, group)` state and lives here,
 * plaintext beside `distributed`: it is a monotone counter, not key material
 * (invariant #2 — nothing here is secret except the AES-GCM-wrapped secret
 * itself), and the ADR explicitly notes it may be logged.
 *
 * **No `DB_VERSION` bump.** An IndexedDB object store holds arbitrary
 * structured-cloneable values; only its `keyPath` and its indexes are schema.
 * `secrets` has `keyPath: "key"` and NO indexes, so adding a field is not a
 * schema change and needs no upgrade transaction. (A bump would also be
 * actively unhelpful: `public/sw.js` opens this database with a version-less
 * `indexedDB.open(name)`, so a foreground upgrade it does not participate in
 * buys nothing.) A row written before this field existed carries none and is
 * read as **`0n`** — the lowest position in the order, so any authorised
 * stamped mint supersedes a legacy secret, which is the direction that
 * converges.
 *
 * ```text
 *   db   "lakko-push-routing"  v2
 *   ├── store "secrets"  keyPath: "key"  (string = accountId "\0" groupIdB64)
 *   │     row {
 *   │       key:        <accountId "\0" groupIdB64>,
 *   │       accountId:  <b64-uuidv7>,
 *   │       groupIdB64: <b64 group_id>,
 *   │       ciphertext: Uint8Array,  // [12-byte IV | AES-GCM(secret)+tag]
 *   │       distributed?: boolean,   // absent (legacy) === true
 *   │       mintEpoch?: bigint       // absent (legacy) === 0n  (ADR-0044)
 *   │     }
 *   ├── store "account_routes"  keyPath: "key"  (accountId "\0" pushGidHex)
 *   │     row {
 *   │       key:        <accountId "\0" 64-hex push_gid>,  (SW lookup key)
 *   │       pushGidHex: <64 hex>,      // the opaque push_gid
 *   │       accountId:  <b64-uuidv7>,
 *   │       groupIdB64: <b64 group_id> // the REAL DS journal key (public)
 *   │       subIdHex?:  <32 hex>       // ADR-0078: this device's push-gw
 *   │                                  // subscription id for this push_gid
 *   │     }
 *   └── store "wake_binding"  keyPath: "slot"  (always "current")
 *         row {
 *           slot:      "current",
 *           accountId: <b64-uuidv7>
 *         }
 * ```
 *
 * ADR-0078 section 3: push-gw holds no device pseudonym. Each group
 * subscription has its own random id, kept here in the route row so the
 * service worker can check that a wake (which carries the id as `s`) is for
 * a subscription the bound account holds, and so this device can remove its
 * own rows. Adding the field is not a schema change (no index), so
 * `DB_VERSION` stays.
 *
 * v2 replaced the v1 `routes` store (keyed by `push_gid` alone); the upgrade
 * drops it, so its rows simply stop being read (no production data, no
 * migration) and a group re-indexes on its next persist.
 *
 * ## Invariants
 *
 * - #2 / #5 — the reverse index maps an OPAQUE `push_gid` to a PUBLIC
 *   `group_id`; no plaintext PII, no key material. The raw secret (the
 *   only sensitive value) is device-key-encrypted. Nothing here ever
 *   leaves the device.
 * - #7 — the secret is wrapped under the hardware-backed device root key.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";
import { getDeviceKey } from "../auth/key-store";

const DB_NAME = "lakko-push-routing";
const DB_VERSION = 2;
/** Device-key-encrypted `push_routing_secret`, keyed per (account, group). */
const SECRETS_STORE = "secrets";
/** Plaintext `(account, push_gid) → group_id` reverse index (SW-readable). */
const ROUTES_STORE = "account_routes";
/** The v1 reverse index keyed by `push_gid` alone; dropped by the v2 upgrade. */
const LEGACY_ROUTES_STORE = "routes";
/** The one binding row naming the signed-in account (SW-readable). */
const WAKE_BINDING_STORE = "wake_binding";
/** The binding store's only key. */
const WAKE_BINDING_SLOT = "current";

/** AES-GCM IV length we prepend to each ciphertext. */
const IV_LEN = 12;

/** Composite `secrets` row key — `accountId` and `groupIdB64` are both
 *  base64 (never contain a NUL), so a NUL separator is unambiguous. */
function secretRowKey(accountIdB64: string, groupIdB64: string): string {
  return `${accountIdB64}\0${groupIdB64}`;
}

/** Composite `account_routes` row key — the same NUL-separated shape; the
 *  service worker builds the identical string (`routeRowKey` in
 *  `public/sw.js`), so the two must stay in step. */
export function routeRowKey(accountIdB64: string, pushGidHex: string): string {
  return `${accountIdB64}\0${pushGidHex.toLowerCase()}`;
}

/** On-disk `secrets` row. `ciphertext` = `[12-byte IV | AES-GCM+tag]`. */
interface SecretRow {
  key: string;
  accountId: string;
  groupIdB64: string;
  ciphertext: Uint8Array;
  /** Was the in-group distribution envelope confirmed on the wire?
   *  **Optional on purpose** — a row written before this field existed has
   *  none, and {@link getGroupRoutingState} reads that as `true` (see the
   *  module doc). Only an explicit `false` means "pending". */
  distributed?: boolean;
  /** ADR-0044 — the MLS group epoch this secret was MINTED under, replayed
   *  verbatim by every re-broadcast. **Optional on purpose** — a row written
   *  before this field existed has none, and {@link getGroupRoutingRecord}
   *  reads that as `0n` (see the module doc). */
  mintEpoch?: bigint;
}

/**
 * A stored secret together with the mint epoch it was stamped under —
 * ADR-0044's unit of state.
 *
 * Read as a PAIR, from ONE row, deliberately. Reading the secret and its
 * epoch through two separate transactions could tear across a concurrent
 * receive (secret from before the write, epoch from after), and a torn pair
 * composes an envelope claiming a live epoch for a superseded secret — which
 * is precisely the F-A shape the ordering exists to refuse.
 */
export interface GroupRoutingRecord {
  /** The 32-byte `push_routing_secret`. */
  secret: Uint8Array;
  /** The epoch this secret was minted under. `0n` for a legacy row. */
  mintEpoch: bigint;
}

/**
 * What this device holds for a `(account, group)` pair:
 *
 *   - `absent` — no secret at all (a member awaiting the envelope, or an
 *     owner that has never minted);
 *   - `pending` — a secret is held but its distribution envelope was never
 *     confirmed on the wire, so the owner must re-broadcast it;
 *   - `distributed` — a secret is held and the envelope landed (or the row
 *     predates the flag).
 */
export type GroupRoutingState = "absent" | "pending" | "distributed";

/** On-disk `account_routes` row — the plaintext reverse-index entry. */
export interface RouteRow {
  /** {@link routeRowKey}(accountId, pushGidHex) — the SW's lookup key. */
  key: string;
  /** Lowercase-hex `push_gid` (64 chars). */
  pushGidHex: string;
  /** base64 account_id. */
  accountId: string;
  /** base64 of the REAL MLS `group_id` (the DS journal key). */
  groupIdB64: string;
  /** ADR-0078 — lowercase hex of this device's 16-byte push-gw subscription
   *  id for this `push_gid`; absent until the first subscribe. */
  subIdHex?: string;
}

/** Length of a subscription id in bytes (mirror of the Rust constant). */
const SUB_ID_LEN = 16;

/**
 * Open (creating on first use) the `lakko-push-routing` database with its
 * two stores. A single canonical opener so the `secrets` + `routes` writes
 * share a version and can run in one atomic transaction.
 */
export function openPushRoutingDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(SECRETS_STORE)) {
        db.createObjectStore(SECRETS_STORE, { keyPath: "key" });
      }
      // v2: the `push_gid`-only index is dropped rather than migrated.
      if (db.objectStoreNames.contains(LEGACY_ROUTES_STORE)) {
        db.deleteObjectStore(LEGACY_ROUTES_STORE);
      }
      if (!db.objectStoreNames.contains(ROUTES_STORE)) {
        db.createObjectStore(ROUTES_STORE, { keyPath: "key" });
      }
      if (!db.objectStoreNames.contains(WAKE_BINDING_STORE)) {
        db.createObjectStore(WAKE_BINDING_STORE, { keyPath: "slot" });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void =>
      reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

/** Resolve when `tx` completes; reject on abort / error. */
function awaitTx(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve();
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

async function encrypt(
  deviceKey: DeviceCipher,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, plaintext),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decrypt(
  deviceKey: DeviceCipher,
  blob: Uint8Array,
): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) {
    throw new Error("encrypted push-routing secret too short to be valid");
  }
  const iv = blob.slice(0, IV_LEN);
  const ct = blob.slice(IV_LEN);
  return new Uint8Array(
    await deviceKey.decrypt(iv, ct),
  );
}

/**
 * Persist a group's `push_routing_secret` (device-key-encrypted) AND its
 * `push_gid → (account, group_id)` reverse-index entry in ONE atomic
 * transaction. Idempotent: re-persisting the same `(account, group)` /
 * secret overwrites both rows in place (a no-op in effect).
 *
 * @param args.distributed whether the in-group distribution envelope for
 *                   this secret is already confirmed on the wire. `false`
 *                   on the owner's mint (the broadcast has not happened
 *                   yet); `true` on a RECEIVE (a secret that arrived over
 *                   the journal is trivially already on the wire).
 * @param args.mintEpoch ADR-0044 — the epoch this secret was MINTED under.
 *                   On a mint / rotation that is `Group.epoch` read at mint
 *                   time; on a RECEIVE it is the epoch carried by the
 *                   accepted envelope, NOT the receiver's current epoch —
 *                   the stamp identifies the secret, not the moment it was
 *                   applied.
 * @param deviceKey  the non-extractable AES-GCM device root key. When
 *                   `null` (half-reset — `forgetDevice` in flight) the
 *                   caller must not reach here; this throws rather than
 *                   silently dropping key material.
 */
export async function putGroupRouting(
  args: {
    accountId: string;
    groupIdB64: string;
    secret: Uint8Array;
    pushGidHex: string;
    distributed: boolean;
    mintEpoch: bigint;
  },
  deviceKey: DeviceCipher,
): Promise<void> {
  const ciphertext = await encrypt(deviceKey, args.secret);
  const secretRow: SecretRow = {
    key: secretRowKey(args.accountId, args.groupIdB64),
    accountId: args.accountId,
    groupIdB64: args.groupIdB64,
    ciphertext,
    distributed: args.distributed,
    mintEpoch: args.mintEpoch,
  };
  const routeRow: RouteRow = {
    key: routeRowKey(args.accountId, args.pushGidHex),
    pushGidHex: args.pushGidHex.toLowerCase(),
    accountId: args.accountId,
    groupIdB64: args.groupIdB64,
  };
  const db = await openPushRoutingDb();
  try {
    if (!accountStoresWritable(args.accountId)) return;
    const tx = db.transaction([SECRETS_STORE, ROUTES_STORE], "readwrite");
    tx.objectStore(SECRETS_STORE).put(secretRow);
    // Keep the subscription id a re-persist of the same push_gid finds
    // (ADR-0078): it names this device's row at push-gw.
    const routes = tx.objectStore(ROUTES_STORE);
    const existing = routes.get(routeRow.key);
    existing.onsuccess = (): void => {
      const prior = existing.result as RouteRow | undefined;
      routes.put(
        prior?.subIdHex !== undefined ? { ...routeRow, subIdHex: prior.subIdHex } : routeRow,
      );
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * ADR-0044 — read + decrypt a group's `push_routing_secret` TOGETHER with the
 * mint epoch it was stamped under, or `null` when this device holds none for
 * the `(account, group)` pair. Throws only on an actual AES-GCM decrypt
 * failure (key mismatch / tampered ciphertext).
 *
 * This is the reader every ordering-sensitive caller must use — the compose
 * sites (which replay the stored epoch verbatim) and the receive gate (which
 * compares against it). One `get`, so the pair can never tear; see
 * {@link GroupRoutingRecord}.
 *
 * A legacy row carrying no `mintEpoch` reports `0n`, the lowest position in
 * the order.
 */
export async function getGroupRoutingRecord(
  accountId: string,
  groupIdB64: string,
  deviceKey: DeviceCipher,
): Promise<GroupRoutingRecord | null> {
  const row = await readSecretRow(accountId, groupIdB64);
  if (row === undefined) return null;
  return {
    secret: await decrypt(deviceKey, row.ciphertext),
    mintEpoch: row.mintEpoch ?? 0n,
  };
}

/**
 * Read + decrypt a group's `push_routing_secret`, or `null` when this
 * device holds none for the `(account, group)` pair (a member that has
 * not yet received the distribution envelope). Throws only on an actual
 * AES-GCM decrypt failure (key mismatch / tampered ciphertext).
 *
 * Prefer {@link getGroupRoutingRecord} anywhere the epoch matters — i.e.
 * anywhere an envelope is composed or an incoming one is ordered. This
 * remains for the callers that genuinely only need the bytes (deriving the
 * `push_gid`).
 */
export async function getGroupRoutingSecret(
  accountId: string,
  groupIdB64: string,
  deviceKey: DeviceCipher,
): Promise<Uint8Array | null> {
  return (await getGroupRoutingRecord(accountId, groupIdB64, deviceKey))
    ?.secret ?? null;
}

/** Read the raw `secrets` row (still encrypted), or `undefined`. */
async function readSecretRow(
  accountId: string,
  groupIdB64: string,
): Promise<SecretRow | undefined> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(SECRETS_STORE, "readonly");
    const req = tx
      .objectStore(SECRETS_STORE)
      .get(secretRowKey(accountId, groupIdB64));
    return await new Promise<SecretRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as SecretRow | undefined);
      req.onerror = (): void =>
        reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
}

/**
 * What this device holds for the `(account, group)` pair — the guard the
 * owner's provisioning funnel branches on. No device key needed (the flag
 * and the row's presence are both plaintext), so it works during a
 * half-reset.
 *
 * A row with no `distributed` field predates the flag and reads as
 * `"distributed"` — see the module doc for why that direction is the safe
 * one.
 */
export async function getGroupRoutingState(
  accountId: string,
  groupIdB64: string,
): Promise<GroupRoutingState> {
  const row = await readSecretRow(accountId, groupIdB64);
  if (row === undefined) return "absent";
  return row.distributed === false ? "pending" : "distributed";
}

/**
 * Cheap existence check — does this device already hold a
 * `push_routing_secret` for the `(account, group)` pair? No device key
 * needed. Prefer {@link getGroupRoutingState} when the answer drives the
 * owner's mint / re-broadcast decision: presence alone cannot tell a
 * distributed secret from one whose envelope never reached the wire.
 */
export async function hasGroupRoutingSecret(
  accountId: string,
  groupIdB64: string,
): Promise<boolean> {
  return (await getGroupRoutingState(accountId, groupIdB64)) !== "absent";
}

/**
 * Set a held secret's `distributed` flag, leaving the ciphertext untouched.
 *
 * Read-modify-write inside ONE transaction, chained through the `get`'s
 * `onsuccess` rather than across an `await`: an IndexedDB transaction
 * auto-commits once it has no in-flight request, so awaiting between the
 * read and the write would commit the tx and write into a dead one.
 *
 * A no-op when the row is absent (wiped by a concurrent `forgetDevice`) —
 * it must never resurrect a deleted secret — and when the flag already holds
 * the target value. A LEGACY row (no field at all) is written either way:
 * it reads as `distributed`, but that is an inference, not a recorded value,
 * so an explicit write is always a refinement.
 */
async function setDistributedFlag(
  accountId: string,
  groupIdB64: string,
  distributed: boolean,
): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    if (!accountStoresWritable(accountId)) return;
    const tx = db.transaction(SECRETS_STORE, "readwrite");
    const store = tx.objectStore(SECRETS_STORE);
    const req = store.get(secretRowKey(accountId, groupIdB64));
    req.onsuccess = (): void => {
      const row = req.result as SecretRow | undefined;
      if (row === undefined || row.distributed === distributed) return;
      store.put({ ...row, distributed });
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Flip a held secret's `distributed` flag to `true` — called only after the
 * distribution envelope is CONFIRMED on the wire (the broadcast resolved).
 *
 * A no-op when the row is absent (wiped by a concurrent `forgetDevice`).
 */
export async function markGroupRoutingDistributed(
  accountId: string,
  groupIdB64: string,
): Promise<void> {
  return setDistributedFlag(accountId, groupIdB64, true);
}

/**
 * Flip a held secret's `distributed` flag back to `false` — called when a
 * send of the distribution envelope FAILED, so the owner's next provisioning
 * pass re-broadcasts it.
 *
 * This is what makes the retry cover a failed REDISTRIBUTE and not only a
 * failed MINT broadcast. The flag is per-`(account, group)`, not per-member:
 * once the mint broadcast lands the row is `distributed`, so a later
 * `redistributeSecretToGroup` that fails (offline / DS error) after an
 * add-member would otherwise never be retried, and the NEW member — whose
 * only source of the secret is that redistribute — would defer push forever.
 * Marking the row pending again re-arms the existing re-broadcast branch.
 *
 * A no-op when the row is absent (wiped by a concurrent `forgetDevice`).
 */
export async function markGroupRoutingPending(
  accountId: string,
  groupIdB64: string,
): Promise<void> {
  return setDistributedFlag(accountId, groupIdB64, false);
}

/**
 * Reverse-map `(account, push_gid)` (lowercase hex) to its row, or `null`
 * when this account holds no such route. This is the read the ADR-0041
 * silent-push decrypt path performs to turn an opaque wake `g` back into the
 * real DS `group_id`, for the BOUND account only. Exposed here as the
 * canonical foreground reader; the SW hand-rolls its own read-only mirror
 * (it cannot import this module — same pattern as `sw.js`).
 *
 * A row naming another account is a miss, even if the key somehow matched.
 */
export async function lookupPushRoute(
  accountId: string,
  pushGidHex: string,
): Promise<RouteRow | null> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(ROUTES_STORE, "readonly");
    const req = tx
      .objectStore(ROUTES_STORE)
      .get(routeRowKey(accountId, pushGidHex));
    return await new Promise<RouteRow | null>((resolve, reject) => {
      req.onsuccess = (): void => {
        const row = req.result as RouteRow | undefined;
        resolve(row !== undefined && row.accountId === accountId ? row : null);
      };
      req.onerror = (): void =>
        reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
}

/**
 * Delete ONE reverse-index entry, by `(account, push_gid)` (lowercase hex).
 * Another account's row for the same `push_gid` (both in one group) is left
 * alone.
 *
 * ADR-0043. The asymmetry this closes: the `secrets` store is keyed
 * `(account, group)` and is **overwritten in place** by a rotation, but the
 * `account_routes` store is keyed by `(account, pushGidHex)`, so a rotation
 * only ever **adds** a row — the superseded `(account, push_gid) → group_id`
 * entry survives
 * forever. Left behind, each rotation grows the reverse map by one dead entry
 * whose `push_gid` nothing will ever notify again.
 *
 * A no-op when the row is absent (IndexedDB `delete` on an unknown key
 * succeeds), so the caller never has to check first — a rotation on a device
 * that had not yet indexed the old gid is simply a no-op.
 *
 * Scoped to the ONE key: never touches the `secrets` store, so it can never
 * strand a group by deleting the material a live `push_gid` is derived from.
 */
export async function deleteRoute(
  accountId: string,
  pushGidHex: string,
): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(ROUTES_STORE, "readwrite");
    tx.objectStore(ROUTES_STORE).delete(routeRowKey(accountId, pushGidHex));
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * ADR-0078 — this device's subscription id for `(account, push_gid)`, minting
 * and recording a random one when none is held. Creates the route row when it
 * is missing (a secret persisted before the row existed). Returns the 16 id
 * bytes.
 */
export async function ensureRouteSubId(
  accountId: string,
  pushGidHex: string,
  groupIdB64: string,
): Promise<Uint8Array> {
  const db = await openPushRoutingDb();
  try {
    if (!accountStoresWritable(accountId)) {
      throw new Error("account stores sealed");
    }
    const tx = db.transaction(ROUTES_STORE, "readwrite");
    const routes = tx.objectStore(ROUTES_STORE);
    const key = routeRowKey(accountId, pushGidHex);
    const req = routes.get(key);
    let subIdHex = "";
    req.onsuccess = (): void => {
      const prior = req.result as RouteRow | undefined;
      if (prior?.subIdHex !== undefined && /^[0-9a-f]{32}$/.test(prior.subIdHex)) {
        subIdHex = prior.subIdHex;
        return;
      }
      subIdHex = toHex(crypto.getRandomValues(new Uint8Array(SUB_ID_LEN)));
      const row: RouteRow = {
        key,
        pushGidHex: pushGidHex.toLowerCase(),
        accountId,
        groupIdB64: prior?.groupIdB64 ?? groupIdB64,
        subIdHex,
      };
      routes.put(row);
    };
    await awaitTx(tx);
    return fromHex(subIdHex);
  } finally {
    db.close();
  }
}

/** ADR-0078 — this device's subscription id for `(account, push_gid)`, or
 *  `null` when it never subscribed under it. */
export async function getRouteSubId(
  accountId: string,
  pushGidHex: string,
): Promise<Uint8Array | null> {
  const row = await lookupPushRoute(accountId, pushGidHex);
  if (row?.subIdHex === undefined || !/^[0-9a-f]{32}$/.test(row.subIdHex)) return null;
  return fromHex(row.subIdHex);
}

/** Every reverse-index row of `accountId` (re-subscribing after a token
 *  change; removing the account's subscriptions on sign-out). */
export async function listRoutesForAccount(accountId: string): Promise<RouteRow[]> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(ROUTES_STORE, "readonly");
    const req = tx.objectStore(ROUTES_STORE).getAll();
    return await new Promise<RouteRow[]>((resolve, reject) => {
      req.onsuccess = (): void =>
        resolve(((req.result ?? []) as RouteRow[]).filter((r) => r.accountId === accountId));
      req.onerror = (): void => reject(req.error ?? new Error("idb getAll failed"));
    });
  } finally {
    db.close();
  }
}

function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

/**
 * Delete one group's routing secret and every reverse-index row that points
 * at it (ADR-0087 section 9). The caller retired the subscription first
 * (`retireOldPushGid`). No-op when absent.
 */
export async function deleteGroupRouting(accountId: string, groupIdB64: string): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction([SECRETS_STORE, ROUTES_STORE], "readwrite");
    tx.objectStore(SECRETS_STORE).delete(secretRowKey(accountId, groupIdB64));
    const routes = tx.objectStore(ROUTES_STORE);
    const rows = routes.getAll();
    rows.onsuccess = (): void => {
      for (const row of rows.result as RouteRow[]) {
        if (row.accountId === accountId && row.groupIdB64 === groupIdB64) {
          routes.delete(routeRowKey(accountId, row.pushGidHex));
        }
      }
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Forget device: wipe `accountId`'s push-routing secrets and the reverse-index
 * entries that name it, in one transaction. Another account's rows on this
 * browser are left alone (2026-09-30 — Forget device used to call
 * {@link clearAllGroupRouting}, which stopped every other account's wakes
 * until each re-provisioned). Whole-row deletes of an account that is being
 * wiped, so, like `clearAllGroupRouting`, it does not take the per-row lock.
 */
export async function clearGroupRoutingForAccount(accountId: string): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction([SECRETS_STORE, ROUTES_STORE], "readwrite");
    const secrets = tx.objectStore(SECRETS_STORE);
    const secretRows = secrets.getAll();
    secretRows.onsuccess = (): void => {
      for (const row of (secretRows.result ?? []) as SecretRow[]) {
        if (row.accountId === accountId) secrets.delete(row.key);
      }
    };
    const routes = tx.objectStore(ROUTES_STORE);
    const routeRows = routes.getAll();
    routeRows.onsuccess = (): void => {
      for (const row of (routeRows.result ?? []) as RouteRow[]) {
        if (row.accountId === accountId) routes.delete(row.key);
      }
    };
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Wipe every push-routing secret + reverse-index entry, regardless of
 * account. Leaves the wake binding alone ({@link clearWakeBinding}). Not called by `AuthProvider.forgetDevice`, which clears only the
 * forgotten account's rows ({@link clearGroupRoutingForAccount}).
 */
export async function clearAllGroupRouting(): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction([SECRETS_STORE, ROUTES_STORE], "readwrite");
    tx.objectStore(SECRETS_STORE).clear();
    tx.objectStore(ROUTES_STORE).clear();
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Convenience — read + decrypt a group's secret using the account's device
 * root key (fetched internally). Returns `null` when there is no device key
 * (half-reset) or no secret held. The device-key-less callers (orchestration)
 * use this so they don't each re-fetch the key.
 */
export async function getGroupRoutingSecretForAccount(
  accountId: string,
  groupIdB64: string,
): Promise<Uint8Array | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  return getGroupRoutingSecret(accountId, groupIdB64, deviceKey);
}

/**
 * The account this browser registered for push — ADR-0042, "A wake is shown
 * only for the signed-in account". Since ADR-0078 there is no device
 * pseudonym: a wake names its subscription id, which the service worker
 * matches against the bound account's route rows.
 */
export interface WakeBinding {
  /** base64 account_id of the account that registered. */
  accountId: string;
}

/** On-disk `wake_binding` row. */
interface WakeBindingRow extends WakeBinding {
  slot: string;
}

/**
 * Record that `accountId` is the account registered for push on this browser,
 * replacing any earlier binding (a device has at most one active account).
 * The service worker reads this row on every wake.
 */
export async function putWakeBinding(binding: WakeBinding): Promise<void> {
  const row: WakeBindingRow = {
    slot: WAKE_BINDING_SLOT,
    accountId: binding.accountId,
  };
  const db = await openPushRoutingDb();
  try {
    if (!accountStoresWritable(binding.accountId)) return;
    const tx = db.transaction(WAKE_BINDING_STORE, "readwrite");
    tx.objectStore(WAKE_BINDING_STORE).put(row);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/** The current binding, or `null` when none is recorded (signed out). */
export async function getWakeBinding(): Promise<WakeBinding | null> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(WAKE_BINDING_STORE, "readonly");
    const req = tx.objectStore(WAKE_BINDING_STORE).get(WAKE_BINDING_SLOT);
    const row = await new Promise<WakeBindingRow | undefined>(
      (resolve, reject) => {
        req.onsuccess = (): void =>
          resolve(req.result as WakeBindingRow | undefined);
        req.onerror = (): void =>
          reject(req.error ?? new Error("idb get failed"));
      },
    );
    if (row === undefined || typeof row.accountId !== "string") {
      return null;
    }
    return { accountId: row.accountId };
  } finally {
    db.close();
  }
}

/**
 * Drop the binding, so the service worker handles no further wake. Log out
 * and Forget device call this before anything else they do.
 */
export async function clearWakeBinding(): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(WAKE_BINDING_STORE, "readwrite");
    tx.objectStore(WAKE_BINDING_STORE).delete(WAKE_BINDING_SLOT);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}
