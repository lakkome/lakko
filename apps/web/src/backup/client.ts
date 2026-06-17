// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `BackupClient` — orchestrates the encrypted-backup upload and
 * restore flows (Phase 2a1c).
 *
 * The client glues four layers together:
 *
 *   1. **Phrase cache** (`phrase-store.ts`): the canonical
 *      recovery phrase, encrypted under the device-root key,
 *      persisted in IndexedDB.
 *   2. **`lakko-backup` wasm surface**: `buildBackupPayload`,
 *      `sealBackup`, `openBackup`, `parseBackupPayload`. The
 *      first two compose the upload path; the latter two the
 *      restore path.
 *   3. **`StorageClient`** (`storage/client.ts`): the three
 *      `/backup` HTTP routes (`PUT`, `GET`, `DELETE`).
 *   4. **Local state sources**: `IdentitySecrets` from the
 *      `AuthContext` + `loadAccepted()` from
 *      `contacts/local-store.ts`. These feed the payload's
 *      identity-secrets + accepted-contacts fields.
 *
 * The payload also carries a whole-storage MLS snapshot
 * (`Identity.snapshotStorage()`, Phase 2a3d3) so a restored device
 * lands with the same group state the upload-side device had at
 * backup time; it is empty only on the signup baseline (no MLS state
 * to publish yet) or when the backup pre-dates 2a3d.
 *
 * ## Two entry points
 *
 * - [`BackupClient.uploadNow`] — caller has the phrase available
 *   right now (e.g. signup just finished and we want a baseline
 *   backup). Skip the phrase-cache read; use the phrase directly.
 * - [`BackupClient.uploadFromCache`] — caller wants to back up
 *   on schedule / on local-state change. Reads the cached
 *   phrase, derives KEK, encrypts payload, PUTs. Returns a
 *   discriminated `UploadResult` so the route layer can surface
 *   the right UX: success vs cache-miss vs decrypt-failed.
 *
 * The restore-side entry point ([`BackupClient.restore`]) lives
 * here too. It's used by the post-signup / post-recovery flow:
 * GET the blob from server, decrypt with phrase, return the
 * structured fields ready to feed back into the
 * `AuthContext` + `contacts/local-store`.
 */
import { loadWasm } from "../wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import { StorageClient } from "../storage/client";
import {
  loadAccepted,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { getPhrase } from "./phrase-store";
import type { BackupContact as WasmBackupContact } from "../../wasm/lakko_ffi_wasm";

/** Canonical account_id length (UUIDv7) the backup format round-trips. */
const ACCOUNT_ID_BYTES = 16;

/**
 * Decode a `AcceptedContact.accountIdHex` (32 hex chars) into the 16-byte
 * account_id for the `BackupContact` constructor. Returns `undefined`
 * (-> None on the wasm side) for an absent or malformed value, so a
 * pre-ADR-0021 contact composes a backup with no account_id rather than
 * failing the whole upload.
 */
function accountIdFromHex(hex: string | undefined): Uint8Array | undefined {
  if (hex === undefined || hex.length !== ACCOUNT_ID_BYTES * 2) {
    return undefined;
  }
  const out = new Uint8Array(ACCOUNT_ID_BYTES);
  for (let i = 0; i < ACCOUNT_ID_BYTES; i += 1) {
    const byte = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    if (Number.isNaN(byte)) {
      return undefined;
    }
    out[i] = byte;
  }
  return out;
}

/** Discriminated upload-flow outcome. */
export type UploadResult =
  | { readonly kind: "uploaded"; readonly bytes: number; readonly at: Date }
  | { readonly kind: "no-cached-phrase" }
  | { readonly kind: "decrypt-failed" }
  | { readonly kind: "transport-failed"; readonly cause: unknown };

/** Discriminated restore-flow outcome. */
export type RestoreResult =
  | {
      readonly kind: "restored";
      readonly accountId: Uint8Array;
      readonly createdAtMs: number;
      readonly ed25519Seed: Uint8Array;
      readonly acceptedContacts: AcceptedContact[];
      /** Phase 2a3d3: whole-storage MLS snapshot from
       *  `Identity.snapshotStorage()`. Empty when the backup
       *  pre-dates 2a3d or no MLS state was published yet. The
       *  recover handler feeds this into
       *  `Identity.restoreStorage(...)` on the freshly-derived
       *  Identity so the new device lands with the same group
       *  state the upload-side device had at backup time. */
      readonly wholeStorageSnapshot: Uint8Array;
    }
  | { readonly kind: "no-backup" }
  | { readonly kind: "wrong-phrase-or-tampered" }
  | { readonly kind: "transport-failed"; readonly cause: unknown };

/**
 * `BackupClient` carries the storage client + the
 * session-token plumbing. Constructed lazily by the route layer
 * — same shape as `ContactsClient` / `ChatClient`.
 */
export class BackupClient {
  constructor(
    private readonly storage: StorageClient = new StorageClient(),
  ) {}

  /** Bind / rebind the bearer token used for the `/backup`
   *  routes. Mirrors `ContactsClient.setSessionToken`. */
  setSessionToken(token: string | null): void {
    this.storage.setSessionToken(token);
  }

  /**
   * Upload a fresh backup blob using a phrase the caller has on
   * hand right now. The signup + recovery flows hand the phrase
   * directly because it was just on screen; no need to round
   * through the encrypted cache.
   *
   * @param accountId    Raw 16-byte UUIDv7.
   * @param ed25519Seed  32-byte identity signing seed.
   * @param phraseBytes  UTF-8 bytes of the canonical recovery
   *                     phrase. Caller is responsible for zeroing
   *                     after this resolves.
   */
  async uploadNow(
    accountId: Uint8Array,
    ed25519Seed: Uint8Array,
    phraseBytes: Uint8Array,
    /** Phase 2a3d3: whole-storage MLS snapshot from
     *  `Identity.snapshotStorage()`. Empty / omitted is fine —
     *  on signup baseline there is no MLS state to publish
     *  yet. Recover-time + scheduled auto-backup paths pass
     *  the current snapshot here. */
    wholeStorageSnapshot: Uint8Array = new Uint8Array(0),
  ): Promise<UploadResult> {
    try {
      const envelope = await this.composeEnvelope(
        accountId,
        ed25519Seed,
        phraseBytes,
        wholeStorageSnapshot,
      );
      await this.storage.putBackup(envelope);
      return { kind: "uploaded", bytes: envelope.length, at: new Date() };
    } catch (err) {
      return { kind: "transport-failed", cause: err };
    }
  }

  /**
   * Upload using the phrase from the IDB-backed cache. Pre-flight
   * the cache read; if missing, surface `no-cached-phrase` so the
   * UI can show a "go through recovery to enable backups" hint.
   *
   * @param accountId  Raw 16-byte UUIDv7 (the IDB row key when
   *                   base64-encoded).
   * @param ed25519Seed 32-byte identity signing seed.
   * @param deviceKey  Non-extractable AES-GCM CryptoKey from
   *                   `device-key.ts`'s `deriveDeviceRootKey`.
   */
  async uploadFromCache(
    accountId: Uint8Array,
    ed25519Seed: Uint8Array,
    deviceKey: CryptoKey,
  ): Promise<UploadResult> {
    const accountIdB64 = base64Encode(accountId);
    let cached;
    try {
      cached = await getPhrase(accountIdB64, deviceKey);
    } catch (err) {
      // Decrypt failed — typically device-key rotation.
      // eslint-disable-next-line no-console
      console.warn("BackupClient: phrase cache decrypt failed", err);
      return { kind: "decrypt-failed" };
    }
    if (cached === null) {
      return { kind: "no-cached-phrase" };
    }
    try {
      return await this.uploadNow(accountId, ed25519Seed, cached.phrase);
    } finally {
      // Best-effort scrub — JS doesn't give us guaranteed zeroize
      // but we can at least clear the Uint8Array contents.
      cached.phrase.fill(0);
    }
  }

  /**
   * Fetch + decrypt + parse the server-stored backup for the
   * authenticated account. The caller passes the phrase
   * directly — restore is always interactive (user has just
   * gone through recovery), so there is no phrase-cache hop.
   */
  async restore(phraseBytes: Uint8Array): Promise<RestoreResult> {
    let envelope: Uint8Array | null;
    try {
      envelope = await this.storage.getBackup();
    } catch (err) {
      return { kind: "transport-failed", cause: err };
    }
    if (envelope === null) return { kind: "no-backup" };

    const wasm = await loadWasm();
    let payloadBytes: Uint8Array;
    try {
      payloadBytes = wasm.openBackup(phraseBytes, envelope);
    } catch (err) {
      // Wrong phrase or tampered blob — wasm surface folds both
      // into one JsError, mirroring the Rust-side fold in
      // `lakko-backup::BackupError`.
      // eslint-disable-next-line no-console
      console.warn("BackupClient: openBackup failed", err);
      return { kind: "wrong-phrase-or-tampered" };
    }
    const parsed = wasm.parseBackupPayload(payloadBytes);

    // Translate the wasm `BackupContact[]` into the TS-side
    // `AcceptedContact` shape `contacts/local-store.ts` expects.
    const acceptedContacts: AcceptedContact[] = [];
    for (let i = 0; i < parsed.contacts.length; i += 1) {
      const c: WasmBackupContact = parsed.contacts[i] as WasmBackupContact;
      const ed = c.ed25519Pubkey;
      const x = c.x25519Pubkey;
      // ADR-0021: restore the per-contact account_id (16 bytes -> hex) when
      // the backup carried one; a v1/v2 backup (or absent field) leaves it
      // unset, so the contact degrades to the peer-tag until re-resolved.
      const accountId = c.accountId;
      acceptedContacts.push({
        signaturePubkeyB64: base64Encode(ed),
        x25519PubkeyB64: base64Encode(x),
        displayName: c.displayName,
        acceptedAtMs: Number(parsed.createdAtMs),
        ...(accountId !== undefined && accountId.length === ACCOUNT_ID_BYTES
          ? { accountIdHex: groupIdHex(accountId) }
          : {}),
      });
    }
    return {
      kind: "restored",
      accountId: parsed.accountId,
      createdAtMs: Number(parsed.createdAtMs),
      ed25519Seed: parsed.ed25519Seed,
      acceptedContacts,
      wholeStorageSnapshot: parsed.wholeStorageSnapshot,
    };
  }

  /**
   * Build the wire envelope: pull the local accepted-contacts
   * list, hand to `wasm.buildBackupPayload`, then `wasm.sealBackup`.
   * Caller already has the phrase + identity bytes; this just
   * does the assembly + AEAD.
   */
  private async composeEnvelope(
    accountId: Uint8Array,
    ed25519Seed: Uint8Array,
    phraseBytes: Uint8Array,
    wholeStorageSnapshot: Uint8Array,
  ): Promise<Uint8Array> {
    const wasm = await loadWasm();

    const accepted = loadAccepted();
    const contacts: WasmBackupContact[] = accepted.map((c) => {
      const ed = base64Decode(c.signaturePubkeyB64);
      const x = c.x25519PubkeyB64 === undefined
        ? new Uint8Array(32) // zero pubkey when legacy ref carries no X25519
        : base64Decode(c.x25519PubkeyB64);
      // ADR-0021: round-trip the per-contact account_id so a restored
      // contact resolves in a group-call roster. A pre-ADR-0021 contact
      // (or a malformed hex) carries no account_id -> None.
      const accountId = accountIdFromHex(c.accountIdHex);
      return new wasm.BackupContact(ed, x, c.displayName, accountId);
    });

    const payload = wasm.buildBackupPayload(
      accountId,
      BigInt(Date.now()),
      ed25519Seed,
      contacts,
      wholeStorageSnapshot,
    );
    return wasm.sealBackup(phraseBytes, payload);
  }
}

/** Convenience: upsert each contact from a restore result into
 *  `localStorage`. The route layer uses this after a successful
 *  `restore()` to land the address book on the new device. */
export function applyRestoredContacts(contacts: AcceptedContact[]): void {
  for (const c of contacts) {
    upsertAccepted(c);
  }
}
