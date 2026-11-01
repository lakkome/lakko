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
 * The payload carries NO MLS provider state (2026-10-02, payload v6). A
 * snapshot belongs to the device that made it: loaded on another device it
 * clones a leaf (ADR-0050), loaded on the same device after a reinstall it
 * rolls the device back behind commits it already published (ADR-0040) and
 * re-uses ratchet positions (G-34). A restored device is admitted to its
 * chats by the other members (ADR-0050 section 2.5a), never from the backup.
 * What the payload does carry about conversations is the list of their ids
 * (`knownGroupIds`), so a restore knows which conversations the account has.
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
 * here too. Every route that restores the identity from the phrase reaches
 * it through `restoreAccountBackup` (`restore.ts`): GET the blob from the
 * server, run the ADR-0037 head gate, decrypt with the phrase, return the
 * structured fields for that function to apply.
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { loadWasm } from "../wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import { currentSyncedStateBytes } from "../devices/sync/synced-state-source";
import { StorageClient } from "../storage/client";
import { requestConfirm } from "../ui/confirm-dialog";
import { staleRestoreConfirmation } from "./stale-restore-confirm";
import {
  loadAccepted,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import {
  accountKeyHexOfSeed,
  boundedOwnPredecessors,
  collectOwnPredecessors,
  type CarriedOwnPredecessor,
} from "../devices/state-bundle";
import { getPhrase } from "./phrase-store";
import { mintBackupHead } from "./head-mint";
import { evaluateRestoreBlob } from "./head-restore";
import { putFloor, type BackupFloor } from "./floor-store";
import type { BackupContact as WasmBackupContact } from "../../wasm/lakko_ffi_wasm";

/** Canonical account_id length (UUIDv7) the backup format round-trips. */
const ACCOUNT_ID_BYTES = 16;
/** The account seed of an own predecessor, as `Identity.fromSeed` takes it. */
const ACCOUNT_SEED_BYTES = 64;

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
      /** The ids of the conversations the account had when the backup was
       *  taken, base64. The backup carries no MLS state, so these name
       *  conversations and open none: the restoring device keeps them
       *  (`restored-known-groups.ts`) so its own later uploads still list
       *  them, and is admitted to each by the other members. Empty for an
       *  account that knew no groups. */
      readonly knownGroupIds: string[];
      /** ADR-0058 section 5.2 — the account's folded synced state (pins,
       *  mutes, archive, drafts, read cursors, chat folders) as
       *  `SyncedState.toBytes()` wrote it; empty when none was held. The
       *  restore seeds the sync runtime from it before its first poll. */
      readonly syncedState: Uint8Array;
      /** ADR-0074 section 3.7: the account's own predecessors, oldest first,
       *  with their adoption times; empty for an account that never rotated.
       *  The seeds go where a kept device holds them. Never log them. */
      readonly ownPredecessors: readonly CarriedOwnPredecessor[];
    }
  | { readonly kind: "no-backup" }
  | { readonly kind: "wrong-phrase-or-tampered" }
  /** The envelope opened under the phrase but its payload is not one this
   *  build reads (a payload version it refuses). Nothing was restored. */
  | { readonly kind: "unsupported-payload" }
  /** ADR-0037 §6 rollback gate refused the served blob. `hard` = an
   *  unambiguous tamper / corruption (no override); `hard: false` = a stale
   *  head the user was offered and DECLINED to override. Either way no
   *  restore happened — the caller alarms (hard) or notes (declined). */
  | { readonly kind: "rollback-blocked"; readonly hard: boolean; readonly reason: string }
  | { readonly kind: "transport-failed"; readonly cause: unknown };

/**
 * Context the restore seam threads into [`BackupClient.restore`] to arm the
 * ADR-0037 §6 rollback gate. **Required** (F2) — there is no ungated restore
 * path: a caller cannot omit it, so a served blob is always run through the
 * fail-open/closed policy before its envelope is opened.
 */
export interface RestoreGateContext {
  /** The 16-byte account_id the client OPAQUE-authenticated as (head binding). */
  readonly accountId: Uint8Array;
  /** Base64 account id — the floor-store key. */
  readonly accountIdB64: string;
  /** Non-extractable device root key for the version-floor store. */
  readonly deviceKey: DeviceCipher;
  /** Auth base for the attest-key fetch (default the shared `/api/auth`). */
  readonly authBaseUrl?: string;
  /** Injectable clock in ms (default `Date.now()`). */
  readonly now?: number;
  /** Injectable staleness window in ms (default from the env knob). */
  readonly stalenessMillis?: bigint;
  /** Confirmation for a stale head — MUST NOT auto-accept. Default is the
   *  app's in-page dialog; a headless env (or any state with no dialog host
   *  mounted) resolves `false` (fail-closed — never a silent downgrade). */
  readonly confirmStaleOverride?: (reason: string) => boolean | Promise<boolean>;
}

/**
 * Default stale-override confirmation.
 *
 * ADR-0045 UX §14 — the app's in-page dialog, matching every other
 * destructive-action confirmation (passkey / member removal / leave / delete
 * chat) rather than a browser one.
 *
 * **The ADR-0037 F5a fail-closed posture is preserved, and by the same rule
 * rather than a special case here:** `requestConfirm` resolves `false`
 * whenever no dialog host is mounted, which covers the headless case the
 * previous `typeof window.confirm !== "function"` guard covered and also the
 * case it did not — a host that failed to mount in a real browser.
 *
 * The move matters most at this call site. A suppressed browser dialog
 * (*"prevent this page from creating additional dialogs"*) returned `false`
 * without rendering, so a user in that state could not complete a legitimate
 * restore and was given nothing to explain why.
 */
function defaultStaleConfirm(reason: string): Promise<boolean> {
  return requestConfirm(staleRestoreConfirmation({ reason }));
}

/**
 * `BackupClient` carries the storage client + the
 * session-token plumbing. Constructed lazily by the route layer
 * — same shape as `ContactsClient` / `ChatClient`.
 */
/** Lakko mints 32-byte MLS group ids; the payload carries them
 *  flat-concatenated, so the width is what splits them back apart. */
const GROUP_ID_LEN = 32;

/** Split the flat-concatenated ids the payload carries back into base64 rows.
 *  A trailing partial id is dropped: the codec never writes one, so its
 *  presence means damage, and half a group id names nothing. */
function splitGroupIds(flat: Uint8Array): string[] {
  const out: string[] = [];
  for (let i = 0; i + GROUP_ID_LEN <= flat.length; i += GROUP_ID_LEN) {
    out.push(base64Encode(flat.subarray(i, i + GROUP_ID_LEN)));
  }
  return out;
}

export class BackupClient {
  /** Bearer token for both the storage `/backup` routes and the
   *  `/auth/backup/head` mint (ADR-0037). Set via [`setSessionToken`]. */
  private sessionToken: string | null = null;

  constructor(
    private readonly storage: StorageClient = new StorageClient(),
  ) {}

  /** Bind / rebind the bearer token used for the `/backup`
   *  routes AND the `/auth/backup/head` mint (ADR-0037). Mirrors
   *  `ContactsClient.setSessionToken`. */
  setSessionToken(token: string | null): void {
    this.sessionToken = token;
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
    /** ADR-0037: the non-extractable device root key. When present, the
     *  auth-assigned `backup_version` floor is persisted into the device-key-
     *  encrypted store on a successful mint. Omitted → no floor persisted
     *  (the mint + framing still happen; a future restore falls back to the
     *  freshness window). */
    deviceKey?: DeviceCipher,
    /** The conversation ids the backup lists, base64: the groups this
     *  device holds plus the restored ones it has not joined yet
     *  (`backupKnownGroupIds`). */
    knownGroupIds: readonly string[] = [],
  ): Promise<UploadResult> {
    try {
      const envelope = await this.composeEnvelope(
        accountId,
        ed25519Seed,
        phraseBytes,
        knownGroupIds,
        deviceKey,
      );
      // ADR-0037: mint the auth-signed head, frame `TAG ‖ head ‖ envelope`,
      // and PUT the framed blob. Soft-fail — a mint failure writes the legacy
      // unframed envelope rather than failing the whole backup.
      const { blob, floor } = await this.frameWithHead(envelope);
      await this.storage.putBackup(blob);
      // Persist the floor ONLY on a successful mint (ADR-0037 §6) — this is
      // the monotonic anchor a future returning-client restore refuses to go
      // below. `putFloor` never lowers an existing floor.
      if (floor !== null && deviceKey !== undefined) {
        try {
          await putFloor(base64Encode(accountId), floor, deviceKey);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("BackupClient: floor persist failed", err);
        }
      }
      return { kind: "uploaded", bytes: blob.length, at: new Date() };
    } catch (err) {
      return { kind: "transport-failed", cause: err };
    }
  }

  /**
   * Mint the auth-signed backup head for `envelope`, then frame
   * `TAG ‖ head ‖ envelope` (ADR-0037 §5). Soft-fail: a mint / framing
   * failure (network, 5xx, no session token) falls back to the UNFRAMED
   * legacy envelope so a backup never fails because the head could not be
   * minted — the mirror image of the restore-side fail-open on an absent head.
   */
  private async frameWithHead(
    envelope: Uint8Array,
  ): Promise<{ blob: Uint8Array; floor: BackupFloor | null }> {
    const wasm = await loadWasm();
    try {
      const hash = wasm.backupEnvelopeHash(envelope);
      const { head, version } = await mintBackupHead(this.sessionToken, hash);
      const blob = wasm.frameBackupBlob(head, envelope);
      return { blob, floor: { version, envelopeHash: hash } };
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "BackupClient: backup-head mint failed, writing legacy envelope",
        err,
      );
      return { blob: envelope, floor: null };
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
    deviceKey: DeviceCipher,
    /** As in [`uploadNow`]. */
    knownGroupIds: readonly string[] = [],
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
      return await this.uploadNow(
        accountId,
        ed25519Seed,
        cached.phrase,
        deviceKey,
        knownGroupIds,
      );
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
  async restore(
    phraseBytes: Uint8Array,
    gate: RestoreGateContext,
  ): Promise<RestoreResult> {
    let blob: Uint8Array | null;
    try {
      blob = await this.storage.getBackup();
    } catch (err) {
      return { kind: "transport-failed", cause: err };
    }
    if (blob === null) return { kind: "no-backup" };

    const wasm = await loadWasm();

    // ADR-0037 §6 (F2): the rollback gate is MANDATORY — always peel `TAG ‖ head`
    // and decide proceed / fail-open / hard-fail / stale-override BEFORE opening
    // the envelope. There is no ungated bypass that could strip a head unchecked.
    let envelope: Uint8Array;
    let cleanProceed: BackupFloor | null = null;
    {
      const evaluation = await evaluateRestoreBlob({
        blob,
        accountId: gate.accountId,
        accountIdB64: gate.accountIdB64,
        deviceKey: gate.deviceKey,
        authBaseUrl: gate.authBaseUrl,
        now: gate.now,
        stalenessMillis: gate.stalenessMillis,
      });
      const verdict = evaluation.verdict;
      if (verdict.kind === "hard-fail") {
        // Refuse + alarm, no override (invariant #4 — no silent downgrade).
        return { kind: "rollback-blocked", hard: true, reason: verdict.reason };
      }
      if (verdict.kind === "stale-override") {
        // Never silent-accept a stale head — require an explicit user ack.
        const confirmFn = gate.confirmStaleOverride ?? defaultStaleConfirm;
        const accepted = await confirmFn(verdict.reason);
        if (!accepted) {
          return { kind: "rollback-blocked", hard: false, reason: verdict.reason };
        }
        // Accepted: proceed with the (older) envelope, but DO NOT advance the
        // floor down — `putFloor` is monotone, so a deliberate downgrade never
        // lowers this device's future protection.
      }
      envelope = evaluation.envelope;
      if (
        verdict.kind === "proceed" &&
        evaluation.verifiedVersion !== null &&
        evaluation.envelopeHash !== null
      ) {
        cleanProceed = {
          version: evaluation.verifiedVersion,
          envelopeHash: evaluation.envelopeHash,
        };
      }
    }

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
    let parsed: ReturnType<typeof wasm.parseBackupPayload>;
    try {
      parsed = wasm.parseBackupPayload(payloadBytes);
    } catch (err) {
      // A payload version this build refuses (v5 and older carried an MLS
      // snapshot and are not read). Nothing to restore from it.
      // eslint-disable-next-line no-console
      console.warn("BackupClient: parseBackupPayload failed", err);
      return { kind: "unsupported-payload" };
    }

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
    // ADR-0037 §6: on a clean verified proceed, advance the monotone floor so
    // subsequent restores on this device are hard-protected (returning-client
    // path). Skipped on fail-open (no verified head) and stale-override
    // (never lower the floor).
    if (cleanProceed !== null) {
      try {
        await putFloor(gate.accountIdB64, cleanProceed, gate.deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("BackupClient: floor persist (restore) failed", err);
      }
    }

    return {
      kind: "restored",
      accountId: parsed.accountId,
      createdAtMs: Number(parsed.createdAtMs),
      ed25519Seed: parsed.ed25519Seed,
      acceptedContacts,
      knownGroupIds: splitGroupIds(parsed.knownGroupIds),
      syncedState: parsed.syncedState,
      ownPredecessors: splitOwnPredecessors(
        parsed.ownPredecessorSeeds,
        parsed.ownPredecessorAdoptedAtMs,
      ),
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
    knownGroupIds: readonly string[],
    deviceKey: DeviceCipher | undefined,
  ): Promise<Uint8Array> {
    const wasm = await loadWasm();

    // The contacts of the account this backup is for, never whichever
    // account happens to be bound.
    const accepted = loadAccepted(base64Encode(accountId));
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

    // Flat-concatenated 32-byte ids: the list of the account's conversations.
    // An id of the wrong width is dropped rather than failing the backup - a
    // malformed local row must not cost the user the whole envelope - and the
    // payload codec caps the rest.
    const idBytes = knownGroupIds
      .map((id) => {
        try {
          return base64Decode(id);
        } catch {
          return null;
        }
      })
      .filter((b): b is Uint8Array => b !== null && b.length === GROUP_ID_LEN);
    const flatGroupIds = new Uint8Array(idBytes.length * GROUP_ID_LEN);
    idBytes.forEach((b, i) => flatGroupIds.set(b, i * GROUP_ID_LEN));

    // ADR-0074 section 3.7: the keys the conversations still name. A row that
    // cannot be read fails this upload: a blob without them would replace one
    // that has them, and a device restored from it would hold a key no
    // conversation knows.
    const own =
      deviceKey === undefined
        ? []
        : boundedOwnPredecessors(
            await collectOwnPredecessors(base64Encode(accountId), deviceKey, (seed) =>
              accountKeyHexOfSeed(wasm, seed),
            ),
          );
    const { seeds, adoptedAtMs } = flattenOwnPredecessors(own);
    try {
      const payload = wasm.buildBackupPayload(
        accountId,
        BigInt(Date.now()),
        ed25519Seed,
        contacts,
        // Payload v6: no MLS snapshot argument. The position it held now takes
        // the group ids, and both are `Uint8Array`, so this list is positional
        // by hand.
        flatGroupIds,
        // ADR-0058 section 5.2 — the folded synced state, so a restore with no
        // other device alive still gets its pins, drafts and folders back.
        currentSyncedStateBytes() ?? undefined,
        own.length === 0 ? undefined : seeds,
        own.length === 0 ? undefined : adoptedAtMs,
      );
      return wasm.sealBackup(phraseBytes, payload);
    } finally {
      seeds.fill(0);
    }
  }
}

/** The wire form of the own predecessors: flat 64-byte seeds, one adoption
 *  time each. The caller zeroes `seeds`. */
export function flattenOwnPredecessors(own: readonly CarriedOwnPredecessor[]): {
  readonly seeds: Uint8Array;
  readonly adoptedAtMs: BigUint64Array;
} {
  const seeds = new Uint8Array(own.length * ACCOUNT_SEED_BYTES);
  const adoptedAtMs = new BigUint64Array(own.length);
  own.forEach((p, i) => {
    const seed = base64Decode(p.seedB64);
    seeds.set(seed, i * ACCOUNT_SEED_BYTES);
    seed.fill(0);
    adoptedAtMs[i] = BigInt(Math.max(0, Math.floor(p.adoptedAtMs)));
  });
  return { seeds, adoptedAtMs };
}

/** {@link flattenOwnPredecessors} reversed. Zeroes `flatSeeds`. */
export function splitOwnPredecessors(
  flatSeeds: Uint8Array,
  adoptedAtMs: ArrayLike<bigint>,
): CarriedOwnPredecessor[] {
  const out: CarriedOwnPredecessor[] = [];
  for (let i = 0; i < adoptedAtMs.length; i += 1) {
    const seed = flatSeeds.subarray(i * ACCOUNT_SEED_BYTES, (i + 1) * ACCOUNT_SEED_BYTES);
    out.push({ seedB64: base64Encode(seed), adoptedAtMs: Number(adoptedAtMs[i] ?? 0n) });
  }
  flatSeeds.fill(0);
  return out;
}

/** Convenience: upsert each contact from a restore result into
 *  `localStorage`. The route layer uses this after a successful
 *  `restore()` to land the address book on the new device. */
export function applyRestoredContacts(
  contacts: AcceptedContact[],
  accountIdB64: string,
): void {
  for (const c of contacts) {
    upsertAccepted(c, accountIdB64);
  }
}
