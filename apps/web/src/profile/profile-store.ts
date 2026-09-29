// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §3 — the per-signature-key store of received profiles.
 *
 * ## Keyed on the authenticated sender, never on the envelope
 *
 * A received profile is filed under the key the CALLER proved it came from:
 *
 *   * in a group, the MLS sender's signature key resolved from the sender leaf
 *     at receipt (C-68, `chat/author-identity.ts`) — never a leaf index, which
 *     OpenMLS hands to the next member added;
 *   * on a contact request, `ContactRequest.fromPubkey`, which the request
 *     signature authenticates.
 *
 * The profile itself names nobody's key, and nothing in it is ever used to
 * choose where it is stored. A profile claiming `@alice` from key B is filed
 * under B and changes nothing about what is known for key A.
 *
 * ## Only strictly newer replaces
 *
 * `updated_at_ms` is the sender's clock. The comparison is the core's
 * `profileIsNewer` (injected): strictly greater wins, equal is not newer, so a
 * replay of the stored profile — or of an older one — is a no-op. A sender
 * whose clock runs backwards only delays its own update.
 *
 * ## Read-decide-write against current state
 *
 * The decision reads the in-memory map, which is authoritative once `load`
 * has run, and the map is written in the same synchronous step. Two envelopes
 * from one poll batch therefore cannot both decide against the same stale
 * snapshot (the ADR-0044 lesson). Persistence follows, serialized.
 *
 * ## Verification results live here too
 *
 * The `(signature key, username)` verification cache (ADR-0048 §3) is persisted
 * in the same row, so a reload does not re-spend a discovery lookup — and a
 * proof-of-work — per contact.
 *
 * The service worker only ever READS the persisted row (C-59); it has no
 * instance of this class.
 */

import type { VerificationState } from "./resolver";

/** One stored profile. */
export interface StoredProfile {
  /** The display name, or `null` for "no name". */
  readonly displayName: string | null;
  /** The CLAIMED username, canonical, or `null`. Not verified by being here. */
  readonly username: string | null;
  /** Sender-clock milliseconds. */
  readonly updatedAtMs: bigint;
  /** The CLAIMED 16-byte auth account id as 32 lower-case hex characters
   *  (the `AcceptedContact.accountIdHex` shape), or `null` when the envelope
   *  carried none (ADR-0048 v2). What the call roster derives the ADR-0021
   *  peer tag from; never an identity on its own — the signature key is. */
  readonly accountIdHex: string | null;
  /** ADR-0058 — the username certificate the profile carried (standard
   *  base64), what `verifyUsernameBinding` checks the claimed username
   *  against; absent or `null` when none. Public (discovery signed it). */
  readonly usernameCertificateB64?: string | null;
}

/** A decoded profile as the core hands it over (the wasm `Profile` getters). */
export interface IncomingProfile {
  readonly displayName: string | undefined | null;
  readonly username: string | undefined | null;
  readonly updatedAtMs: bigint;
  /** Exactly 16 bytes, or absent. Anything else is stored as "none". */
  readonly accountId?: Uint8Array | undefined | null;
  /** ADR-0058 — the username certificate, or absent. */
  readonly usernameCertificate?: Uint8Array | undefined | null;
}

/** Length of an auth account id, in bytes. */
export const ACCOUNT_ID_BYTES = 16;

/** A not-newer re-send that carries an account id the held row lacks: the
 *  held row with that id filled in, when the two are the same profile (same
 *  stamp); `null` when nothing is to be learned. */
export function mergeAccountId(
  held: StoredProfile | undefined,
  incoming: StoredProfile,
): StoredProfile | null {
  if (held === undefined) return null;
  if (held.accountIdHex !== null || incoming.accountIdHex === null) return null;
  if (held.updatedAtMs !== incoming.updatedAtMs) return null;
  return { ...held, accountIdHex: incoming.accountIdHex };
}

/** 32 lower-case hex characters — the only shape `accountIdHex` ever holds. */
export const ACCOUNT_ID_HEX_RE = /^[0-9a-f]{32}$/;

/** Hex of a 16-byte account id; `null` for any other length, so a malformed
 *  claim degrades to "no account id" rather than to a tag nobody derives. */
export function accountIdHexOfBytes(bytes: Uint8Array | null | undefined): string | null {
  if (bytes === null || bytes === undefined || bytes.length !== ACCOUNT_ID_BYTES) return null;
  let out = "";
  for (let i = 0; i < bytes.length; i += 1) {
    out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}

/** A persisted verification result. `pending` is never persisted. */
export interface StoredVerification {
  readonly state: Exclude<VerificationState, "pending">;
  readonly checkedAtMs: number;
}

/** The persisted form of one account's row. */
export interface ProfileRow {
  readonly profiles: ReadonlyMap<string, StoredProfile>;
  readonly verifications: ReadonlyMap<string, StoredVerification>;
}

/** Storage behind the store. The IndexedDB implementation encrypts the row
 *  under the device key (`profile-idb.ts`); tests use a map. */
export interface ProfileBackend {
  load(accountId: string): Promise<ProfileRow | null>;
  save(accountId: string, row: ProfileRow): Promise<void>;
}

/** The cache key for one `(signature key, username)` claim. `|` is outside
 *  both the base64 and the username alphabets. */
export function verificationKey(keyB64: string, username: string): string {
  return `${keyB64}|${username}`;
}

/**
 * The ingest decision, on its own so it can be tested without a backend.
 *
 * `store` when nothing is held for this key or the incoming stamp is strictly
 * newer by the core's order; `keep` otherwise.
 */
export function decideIngest(
  stored: StoredProfile | undefined,
  incomingUpdatedAtMs: bigint,
  isNewer: (incoming: bigint, stored: bigint) => boolean,
): "store" | "keep" {
  if (stored === undefined) return "store";
  return isNewer(incomingUpdatedAtMs, stored.updatedAtMs) ? "store" : "keep";
}

/** Normalise an incoming profile's optional fields to the stored shape. An
 *  empty string is "absent" — the core never produces one, but a hand-built
 *  test value might. */
function toStored(p: IncomingProfile): StoredProfile {
  const displayName =
    typeof p.displayName === "string" && p.displayName.length > 0 ? p.displayName : null;
  const username = typeof p.username === "string" && p.username.length > 0 ? p.username : null;
  return {
    displayName,
    username,
    updatedAtMs: p.updatedAtMs,
    accountIdHex: accountIdHexOfBytes(p.accountId),
    usernameCertificateB64:
      p.usernameCertificate === undefined || p.usernameCertificate === null
        ? null
        : bytesToB64(p.usernameCertificate),
  };
}

function bytesToB64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i] ?? 0);
  return btoa(bin);
}

export class ProfileStore {
  private profiles = new Map<string, StoredProfile>();
  private verifications = new Map<string, StoredVerification>();
  private ready: Promise<void> = Promise.resolve();
  private loadedAccount: string | null = null;
  private writeChain: Promise<void> = Promise.resolve();
  private readonly listeners = new Set<() => void>();

  constructor(
    private readonly backend: ProfileBackend,
    private readonly isNewer: (incoming: bigint, stored: bigint) => boolean,
    private readonly onPersistError: (err: unknown) => void = () => {},
  ) {}

  /** Bind the store to an account and load its row. Replaces whatever was in
   *  memory for another account. Safe to call repeatedly for the same one. */
  open(accountId: string): Promise<void> {
    if (this.loadedAccount === accountId) return this.ready;
    this.loadedAccount = accountId;
    this.profiles = new Map();
    this.verifications = new Map();
    this.notify();
    this.ready = (async () => {
      let row: ProfileRow | null = null;
      try {
        row = await this.backend.load(accountId);
      } catch (err) {
        this.onPersistError(err);
      }
      // A profile ingested while the load was in flight is newer information
      // than the row; never let the row overwrite it.
      if (row !== null && this.loadedAccount === accountId) {
        for (const [key, p] of row.profiles) {
          if (decideIngest(this.profiles.get(key), p.updatedAtMs, this.isNewer) === "store") {
            this.profiles.set(key, p);
          }
        }
        for (const [key, v] of row.verifications) {
          if (!this.verifications.has(key)) this.verifications.set(key, v);
        }
        this.notify();
      }
    })();
    return this.ready;
  }

  /** Forget the bound account's in-memory state (sign-out). */
  close(): void {
    this.loadedAccount = null;
    this.profiles = new Map();
    this.verifications = new Map();
    this.ready = Promise.resolve();
    this.notify();
  }

  /** The account the store is bound to, or `null`. */
  account(): string | null {
    return this.loadedAccount;
  }

  /**
   * File a received profile under `senderKeyB64` — the caller's AUTHENTICATED
   * sender key (see the module docs). Returns whether it replaced what was
   * held.
   */
  async ingest(accountId: string, senderKeyB64: string, profile: IncomingProfile): Promise<boolean> {
    await this.open(accountId);
    if (this.loadedAccount !== accountId) return false;
    const incoming = toStored(profile);
    const held = this.profiles.get(senderKeyB64);
    if (decideIngest(held, incoming.updatedAtMs, this.isNewer) === "keep") {
      // The same profile sent again (a growth or request answer carries the
      // stamp it was set at) is not newer, but it can carry an account id the
      // held row never had: rows filed before the envelope carried one, or by
      // an older sender. Take the id and nothing else, or the call roster can
      // never name a person whose profile predates the field.
      const learned = mergeAccountId(held, incoming);
      if (learned === null) return false;
      this.profiles.set(senderKeyB64, learned);
      this.notify();
      this.persist(accountId);
      return true;
    }
    this.profiles.set(senderKeyB64, incoming);
    this.notify();
    this.persist(accountId);
    return true;
  }

  /** The profile held for a key, or `undefined`. */
  get(keyB64: string): StoredProfile | undefined {
    return this.profiles.get(keyB64);
  }

  /** Every held profile (for iteration by the verifier). */
  entries(): ReadonlyMap<string, StoredProfile> {
    return this.profiles;
  }

  /** A persisted verification result, or `undefined`. */
  verification(keyB64: string, username: string): StoredVerification | undefined {
    return this.verifications.get(verificationKey(keyB64, username));
  }

  /** Record a verification result and persist it. */
  recordVerification(accountId: string, keyB64: string, username: string, v: StoredVerification): void {
    if (this.loadedAccount !== accountId) return;
    this.verifications.set(verificationKey(keyB64, username), v);
    this.notify();
    this.persist(accountId);
  }

  /** Re-write the bound account's row unchanged — for data the backend adds
   *  to the row from elsewhere (the rename mirror). */
  resave(accountId: string): void {
    if (this.loadedAccount !== accountId) return;
    void this.ready.then(() => {
      if (this.loadedAccount === accountId) this.persist(accountId);
    });
  }

  /** Subscribe to changes; returns the unsubscribe. */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Resolves once every queued write has settled. */
  flush(): Promise<void> {
    return this.writeChain;
  }

  private notify(): void {
    for (const l of this.listeners) l();
  }

  private persist(accountId: string): void {
    // Snapshot NOW, so the row written is the state this change produced;
    // serialize so an older snapshot can never land after a newer one.
    const row: ProfileRow = {
      profiles: new Map(this.profiles),
      verifications: new Map(this.verifications),
    };
    this.writeChain = this.writeChain
      .then(() => this.backend.save(accountId, row))
      .catch((err: unknown) => this.onPersistError(err));
  }
}

/** A backend that keeps rows in memory — tests and a session with no device
 *  key. */
export class MemoryProfileBackend implements ProfileBackend {
  readonly rows = new Map<string, ProfileRow>();
  async load(accountId: string): Promise<ProfileRow | null> {
    return this.rows.get(accountId) ?? null;
  }
  async save(accountId: string, row: ProfileRow): Promise<void> {
    this.rows.set(accountId, row);
  }
}
