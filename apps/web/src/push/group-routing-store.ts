// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed persistence for the ADR-0042 opaque per-group push
 * routing id.
 *
 * ## What this store holds
 *
 * Two things, in ONE database (`lakko-push-routing`) so the background
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
 *   2. **`routes`** — the **`push_gid → (account, group_id)` reverse
 *      index**, keyed by the lowercase-hex `push_gid`. This is the map
 *      the ADR-0041 silent-push decrypt path reverse-resolves in Sprint 4
 *      (`resolveAccount(pushGidHex)` → the real `group_id` for the DS
 *      fetch). It is stored **plaintext**: `push_gid` is a one-way
 *      BLAKE3 derivation (reveals nothing) and `group_id` is a public DS
 *      routing address already shared with the server — exactly the
 *      rationale that keeps `knownGroupIds` unencrypted in the MLS
 *      snapshot store. Plaintext is REQUIRED so the SW can read it
 *      without the device key.
 *
 * The two stores age differently, which ADR-0043 makes load-bearing: a
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
 * distribution envelope is **recomputable from the stored secret at any
 * time** (`composePushRoutingSecret(secret)`). So one durable boolean is the
 * whole mechanism; there is nothing to spool.
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
 * ```text
 *   db   "lakko-push-routing"  v1
 *   ├── store "secrets"  keyPath: "key"  (string = accountId "\0" groupIdB64)
 *   │     row {
 *   │       key:        <accountId "\0" groupIdB64>,
 *   │       accountId:  <b64-uuidv7>,
 *   │       groupIdB64: <b64 group_id>,
 *   │       ciphertext: Uint8Array,  // [12-byte IV | AES-GCM(secret)+tag]
 *   │       distributed?: boolean    // absent (legacy) === true
 *   │     }
 *   └── store "routes"  keyPath: "pushGidHex"  (string, 64 lowercase hex)
 *         row {
 *           pushGidHex: <64 hex>,      // the opaque push_gid (SW lookup key)
 *           accountId:  <b64-uuidv7>,
 *           groupIdB64: <b64 group_id> // the REAL DS journal key (public)
 *         }
 * ```
 *
 * ## Invariants
 *
 * - #2 / #5 — the reverse index maps an OPAQUE `push_gid` to a PUBLIC
 *   `group_id`; no plaintext PII, no key material. The raw secret (the
 *   only sensitive value) is device-key-encrypted. Nothing here ever
 *   leaves the device.
 * - #7 — the secret is wrapped under the hardware-backed device root key.
 */

import { getDeviceKey } from "../auth/key-store";

const DB_NAME = "lakko-push-routing";
const DB_VERSION = 1;
/** Device-key-encrypted `push_routing_secret`, keyed per (account, group). */
const SECRETS_STORE = "secrets";
/** Plaintext `push_gid → (account, group_id)` reverse index (SW-readable). */
const ROUTES_STORE = "routes";

/** AES-GCM IV length we prepend to each ciphertext. */
const IV_LEN = 12;

/** Composite `secrets` row key — `accountId` and `groupIdB64` are both
 *  base64 (never contain a NUL), so a NUL separator is unambiguous. */
function secretRowKey(accountIdB64: string, groupIdB64: string): string {
  return `${accountIdB64}\0${groupIdB64}`;
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

/** On-disk `routes` row — the plaintext reverse-index entry. */
export interface RouteRow {
  /** Lowercase-hex `push_gid` (64 chars). The SW's reverse-map key. */
  pushGidHex: string;
  /** base64 account_id. */
  accountId: string;
  /** base64 of the REAL MLS `group_id` (the DS journal key). */
  groupIdB64: string;
}

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
      if (!db.objectStoreNames.contains(ROUTES_STORE)) {
        db.createObjectStore(ROUTES_STORE, { keyPath: "pushGidHex" });
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
  deviceKey: CryptoKey,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: iv.slice() },
      deviceKey,
      plaintext.slice(),
    ),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decrypt(
  deviceKey: CryptoKey,
  blob: Uint8Array,
): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) {
    throw new Error("encrypted push-routing secret too short to be valid");
  }
  const iv = blob.slice(0, IV_LEN);
  const ct = blob.slice(IV_LEN);
  return new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
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
  },
  deviceKey: CryptoKey,
): Promise<void> {
  const ciphertext = await encrypt(deviceKey, args.secret);
  const secretRow: SecretRow = {
    key: secretRowKey(args.accountId, args.groupIdB64),
    accountId: args.accountId,
    groupIdB64: args.groupIdB64,
    ciphertext,
    distributed: args.distributed,
  };
  const routeRow: RouteRow = {
    pushGidHex: args.pushGidHex,
    accountId: args.accountId,
    groupIdB64: args.groupIdB64,
  };
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction([SECRETS_STORE, ROUTES_STORE], "readwrite");
    tx.objectStore(SECRETS_STORE).put(secretRow);
    tx.objectStore(ROUTES_STORE).put(routeRow);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Read + decrypt a group's `push_routing_secret`, or `null` when this
 * device holds none for the `(account, group)` pair (a member that has
 * not yet received the distribution envelope). Throws only on an actual
 * AES-GCM decrypt failure (key mismatch / tampered ciphertext).
 */
export async function getGroupRoutingSecret(
  accountId: string,
  groupIdB64: string,
  deviceKey: CryptoKey,
): Promise<Uint8Array | null> {
  const row = await readSecretRow(accountId, groupIdB64);
  if (row === undefined) return null;
  return decrypt(deviceKey, row.ciphertext);
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
 * Reverse-map a `push_gid` (lowercase hex) to its owning
 * `(account, group_id)`, or `null` when unknown. This is the read the
 * ADR-0041 silent-push decrypt path performs in Sprint 4 to turn an
 * opaque wake `g` back into the real DS `group_id`. Exposed here as the
 * canonical foreground reader; the SW hand-rolls its own read-only
 * mirror (it cannot import this module — same pattern as `sw.js`).
 */
export async function lookupPushRoute(
  pushGidHex: string,
): Promise<RouteRow | null> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(ROUTES_STORE, "readonly");
    const req = tx.objectStore(ROUTES_STORE).get(pushGidHex);
    return await new Promise<RouteRow | null>((resolve, reject) => {
      req.onsuccess = (): void =>
        resolve((req.result as RouteRow | undefined) ?? null);
      req.onerror = (): void =>
        reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
}

/**
 * Delete ONE reverse-index entry, by its `push_gid` (lowercase hex).
 *
 * ADR-0043. The asymmetry this closes: the `secrets` store is keyed
 * `(account, group)` and is **overwritten in place** by a rotation, but the
 * `routes` store is keyed by `pushGidHex`, so a rotation only ever **adds** a
 * row — the superseded `push_gid → (account, group_id)` entry survives
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
export async function deleteRoute(pushGidHex: string): Promise<void> {
  const db = await openPushRoutingDb();
  try {
    const tx = db.transaction(ROUTES_STORE, "readwrite");
    tx.objectStore(ROUTES_STORE).delete(pushGidHex);
    await awaitTx(tx);
  } finally {
    db.close();
  }
}

/**
 * Wipe every push-routing secret + reverse-index entry, regardless of
 * account. Called from `AuthProvider.logout` / `forgetDevice` so a device
 * wipe leaves no push-routing residue (mirrors `clearAllMlsSnapshots`).
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
