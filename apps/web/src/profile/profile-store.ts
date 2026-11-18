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
import { avatarFromWasm, type AvatarData, type WasmProfileAvatarLike } from "./avatar-model";

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
   *  carried none (ADR-0048 v2). Never an identity on its own — the
   *  signature key is. (A call roster no longer reads it: since ADR-0078
   *  section 2 a peer is named by its own signed announcement.) */
  readonly accountIdHex: string | null;
  /** ADR-0060 — the username certificate the profile carried (standard
   *  base64), what `verifyUsernameBinding` checks the claimed username
   *  against; absent or `null` when none. Public (discovery signed it). */
  readonly usernameCertificateB64?: string | null;
  /** ADR-0084 — the profile photo references, absent or `null` for none.
   *  Carries blob keys (SECRET); persisted only in the encrypted row. */
  readonly avatar?: AvatarData | null;
  /** ADR-0090 section 7 — whether the person lets others share their
   *  contact card (off exactly when their "who can find me" setting is
   *  `Nobody`). Absent (rows filed before the field) reads as `true`. */
  readonly cardShareable?: boolean;
}

/** Whether a held profile lets its person's card be shared. No profile, or
 *  one filed before the flag existed, counts as shareable (ADR-0090 section
 *  7: a person who never sent a profile counts as shareable). */
export function profileCardShareable(p: StoredProfile | undefined): boolean {
  return p?.cardShareable !== false;
}

/** A decoded profile as the core hands it over (the wasm `Profile` getters). */
export interface IncomingProfile {
  readonly displayName: string | undefined | null;
  readonly username: string | undefined | null;
  readonly updatedAtMs: bigint;
  /** Exactly 16 bytes, or absent. Anything else is stored as "none". */
  readonly accountId?: Uint8Array | undefined | null;
  /** ADR-0060 — the username certificate, or absent. */
  readonly usernameCertificate?: Uint8Array | undefined | null;
  /** ADR-0084 — the photo, already copied out of the wasm handles
   *  (`AvatarData`), or the wasm `ProfileAvatar` itself, or absent. */
  readonly avatar?: AvatarData | WasmProfileAvatarLike | undefined | null;
  /** ADR-0090 section 7 — the "my card may be shared" flag; absent = true. */
  readonly cardShareable?: boolean | undefined | null;
}

function isAvatarData(a: unknown): a is AvatarData {
  return typeof a === "object" && a !== null && typeof (a as { still?: { blobIdB64?: unknown } }).still?.blobIdB64 === "string";
}

/** The photo of an incoming profile as plain data. */
export function incomingAvatar(p: IncomingProfile): AvatarData | null {
  const a = p.avatar;
  if (a === undefined || a === null) return null;
  return isAvatarData(a) ? a : avatarFromWasm(a);
}

/** Length of an auth account id, in bytes. */
export const ACCOUNT_ID_BYTES = 16;

/** The stamp a name found by search is filed at (`ProfileApi.fileFoundName`):
 *  any profile the person sent is strictly newer and replaces it. */
export const FOUND_NAME_STAMP = 0n;

/** Whether a row is only what a search found, with the person's own profile
 *  still to come. Nothing a person publishes looks like this: their own
 *  profile carries their account id. */
export function awaitsOwnProfile(p: StoredProfile | undefined): boolean {
  return (
    p !== undefined &&
    p.updatedAtMs === FOUND_NAME_STAMP &&
    p.accountIdHex === null &&
    (p.usernameCertificateB64 ?? null) === null
  );
}

/** A person who never set a name publishes at stamp 0, level with a name
 *  found for them: theirs still replaces it. A profile saying nothing does
 *  not: it would put the stand-in back. */
export function replacesFoundName(held: StoredProfile | undefined, incoming: StoredProfile): boolean {
  return (
    awaitsOwnProfile(held) &&
    !awaitsOwnProfile(incoming) &&
    (incoming.displayName !== null || incoming.username !== null)
  );
}

/** What a pairing state bundle carries of a row; `null` for nothing. A found
 *  username stands on this device's own verdict, which does not travel: with
 *  no certificate the new device would refuse it. */
export function forStateBundle(p: StoredProfile): StoredProfile | null {
  if (!awaitsOwnProfile(p) || p.username === null) return p;
  return p.displayName === null ? null : { ...p, username: null };
}

/** A not-newer re-send that carries an account id or a username
 *  certificate the held row lacks: the held row with those filled in, when
 *  the two are the same profile (same stamp); `null` when nothing is to be
 *  learned. */
export function mergeAccountId(
  held: StoredProfile | undefined,
  incoming: StoredProfile,
): StoredProfile | null {
  if (held === undefined) return null;
  if (held.updatedAtMs !== incoming.updatedAtMs) return null;
  const learnId = held.accountIdHex === null && incoming.accountIdHex !== null;
  const heldCert = held.usernameCertificateB64 ?? null;
  const incomingCert = incoming.usernameCertificateB64 ?? null;
  // Only for the username the held row claims: a certificate binds one name.
  const learnCert = heldCert === null && incomingCert !== null && incoming.username === held.username;
  if (!learnId && !learnCert) return null;
  // The card flag is not learned here: a switch to or from `Nobody` restamps
  // the sender's profile (ADR-0090 section 7), so only a strictly newer
  // profile changes it.
  return {
    ...held,
    accountIdHex: learnId ? incoming.accountIdHex : held.accountIdHex,
    usernameCertificateB64: learnCert ? incomingCert : heldCert,
  };
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
    ...avatarField(incomingAvatar(p)),
    ...(p.cardShareable === false ? { cardShareable: false } : {}),
  };
}

/** The `avatar` field of a stored profile: absent for no photo, so a row
 *  without one has the shape it always had. */
export function avatarField(a: AvatarData | null): { readonly avatar?: AvatarData } {
  return a === null ? {} : { avatar: a };
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
  /** The account whose row has finished loading; an ingest for it is filed
   *  in the same turn (see `ingest`). */
  private settledAccount: string | null = null;
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
    this.settledAccount = null;
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
          const held = this.profiles.get(key);
          if (replacesFoundName(held, p) || decideIngest(held, p.updatedAtMs, this.isNewer) === "store") {
            this.profiles.set(key, p);
          }
        }
        for (const [key, v] of row.verifications) {
          if (!this.verifications.has(key)) this.verifications.set(key, v);
        }
        this.notify();
      }
      if (this.loadedAccount === accountId) this.settledAccount = accountId;
    })();
    return this.ready;
  }

  /** Forget the bound account's in-memory state (sign-out). */
  close(): void {
    this.loadedAccount = null;
    this.settledAccount = null;
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
    // Filed in the SAME turn once the row is loaded: the receive loop files a
    // profile and then draws the rows of its batch, and a message after the
    // profile must be drawn under the name, not under a placeholder for a
    // microtask (rows are applied in journal order).
    if (this.settledAccount !== accountId) await this.open(accountId);
    if (this.loadedAccount !== accountId) return false;
    const incoming = toStored(profile);
    const held = this.profiles.get(senderKeyB64);
    if (!replacesFoundName(held, incoming) && decideIngest(held, incoming.updatedAtMs, this.isNewer) === "keep") {
      // The same profile sent again (a growth or request answer carries the
      // stamp it was set at) is not newer, but it can carry an account id the
      // held row never had: rows filed before the envelope carried one, or by
      // an older sender. Take the id and nothing else, or the call roster can
      // never name a person whose profile predates the field.
      const learned = mergeAccountId(held, incoming);
      if (learned === null) return false;
      this.forgetVerificationIfCertificateChanged(senderKeyB64, held, learned);
      this.profiles.set(senderKeyB64, learned);
      this.notify();
      this.persist(accountId);
      return true;
    }
    this.forgetVerificationIfCertificateChanged(senderKeyB64, held, incoming);
    this.profiles.set(senderKeyB64, incoming);
    this.notify();
    this.persist(accountId);
    return true;
  }

  /** A verdict reached on a row without the certificate (or with another one)
   *  says nothing about the row that has it: drop it, so the claim is checked
   *  again. A profile that arrived without a certificate was cached as a
   *  mismatch for a day, and its certificate, arriving later, never cleared
   *  it. */
  private forgetVerificationIfCertificateChanged(
    senderKeyB64: string,
    held: StoredProfile | undefined,
    next: StoredProfile,
  ): void {
    const before = held?.usernameCertificateB64 ?? null;
    const after = next.usernameCertificateB64 ?? null;
    if (before === after || next.username === null) return;
    this.verifications.delete(verificationKey(senderKeyB64, next.username));
  }

  /** The photo held for a key, or `null`. */
  avatarOf(keyB64: string): AvatarData | null {
    return this.profiles.get(keyB64)?.avatar ?? null;
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
