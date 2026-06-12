// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `ContactsClient` — orchestrates the send side of the
 * `lakko-contacts::ContactRequest` flow against the live
 * `lakko-svc-discovery` + `lakko-svc-ds` services.
 *
 * **Send flow (`sendContactRequest`):**
 *
 *   1. Resolve `@username` → `account_id` via discovery
 *      (`DiscoveryClient.lookup`).
 *   2. Fetch the recipient's identity bundle (Ed25519 + X25519
 *      pubkeys) via discovery
 *      (`DiscoveryClient.identityFetch`).
 *   3. Derive the recipient's mailbox pseudonym
 *      (`IdentityPseudonym.fromIdentity(recipient_pubkey)` —
 *      32 bytes, public, anyone with the recipient's identity
 *      pubkey can compute the same value).
 *   4. Compose the wire bytes via
 *      `ContactRequest.compose(senderSigningSeed,
 *      recipientX25519Pub, fromPseudonym, validAfterMs,
 *      introMessage)`.
 *   5. POST the bytes to
 *      `lakko-svc-ds`'s `/ds/mailbox/{recipient_pseudonym_hex}`
 *      endpoint.
 *
 * The receive flow + UI live in **1.16d3d**.
 *
 * ## Identity availability
 *
 * Composing a ContactRequest needs the sender's Ed25519
 * signing-seed bytes — recoverable from the OPAQUE export key
 * via the deterministic `Identity.fromSeed` (1.16d3b2). The
 * AuthContext (1.16d3c) holds the export key in a memory-only
 * signal during the current page session; if the user reloaded
 * the page and the session was restored from IndexedDB
 * (1.16c3c2) WITHOUT a fresh password entry, that signal is
 * `null` and `sendContactRequest` throws
 * `ContactsClientError::NoIdentitySecrets`. The UI is expected
 * to surface a "please re-enter your password" prompt in that
 * case.
 */
import { loadWasm } from "../wasm";
import { DiscoveryClient } from "../discovery/client";
import { base64Decode, base64Encode } from "../auth/account-id";
import { DsClient } from "../ds/client";
import type { IdentitySecrets } from "../auth/AuthContext";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import type { IdentityFetchResponse } from "../discovery/wire";
import { readPin, setPin, recordLogStatus, recordLogReset } from "./local-store";
import {
  verifyIdentityInLog,
  lookupServedKeyReset,
} from "../transparency/verify";
import type { LogVerifyStatus } from "../transparency/verify";
import {
  KIND_CONTACT_ACK,
  KIND_CONTACT_REQUEST,
  KIND_WELCOME,
  parseEnvelope,
  wrapEnvelope,
} from "./envelope";

/** Optional wires the route layer hands to ContactsClient
 *  (Phase 1.16e6c2). See `ChatClientOptions` in
 *  `chat/client.ts` for the rationale — same shape, same
 *  semantics, applied to a different surface. */
export interface ContactsClientOptions {
  /** Returns the **canonical** `WasmIdentity` for this session.
   *  See `<ChatProvider>` (1.16e6c1). When unset, the client
   *  falls back to building a throwaway `Identity.fromSeed` per
   *  call — only acceptable for tests; production paths MUST
   *  wire this so MLS state stays coherent across calls. */
  identityProvider?: () => WasmIdentity | null;
  /** Fires after every successful MLS state-mutating call so
   *  the route layer can persist the canonical Identity's
   *  provider snapshot to disk. Best-effort — exceptions are
   *  logged and swallowed; the in-memory state stays good. */
  onPersist?: () => Promise<void>;
}

/** Result of `ContactsClient.sendContactRequest`. */
export interface SendResult {
  /** The canonical (NFKC + lowercased + trimmed) form of the
   *  username that was resolved. */
  recipientUsername: string;
  /** Raw 16-byte recipient account_id. */
  recipientAccountId: Uint8Array;
  /** DS-assigned monotonic id for the published envelope. */
  messageId: number;
  /** ISO-8601 timestamp from the DS. */
  createdAt: string;
}

/**
 * One verified incoming `ContactRequest` decoded out of the
 * mailbox. The signature has already been checked; the intro
 * envelope is unsealed when present (and `null` when the
 * envelope didn't decode for our X25519 secret — typically a
 * malformed request that we'll just drop).
 */
export interface IncomingContactRequest {
  /** DS-assigned monotonic id — also serves as the cursor for
   *  the next poll. */
  messageId: number;
  /** Sender's Ed25519 identity public key (32 bytes). The
   *  signature on the request was verified against this. */
  fromPubkey: Uint8Array;
  /** Sender's `account_id` (16-byte UUIDv7). Present on every
   *  v2 envelope (i.e. anything composed by 1.16e3b+ clients);
   *  `null` for legacy v1 envelopes — the Accept-driven group
   *  creation path requires this and surfaces a "legacy
   *  request, please ask the sender to upgrade" UX when it's
   *  missing. */
  fromAccountId: Uint8Array | null;
  /** Sender's per-recipient pseudonym (32 bytes). UI doesn't
   *  display this directly; carry it forward in case future
   *  ContactAck flows need it. */
  fromPseudonym: Uint8Array;
  /** `valid_after_ms` from the request. */
  validAfterMs: bigint;
  /** UTF-8-decoded intro message, or `null` when there was no
   *  intro / it didn't decrypt. Empty string is a valid
   *  payload (means "no intro" in `compose`). */
  intro: string | null;
  /** ISO-8601 timestamp the DS recorded on receive. */
  createdAt: string;
}

/** A verified incoming Welcome (MLS group-join blob) decoded
 *  out of the mailbox (1.16e3). */
export interface IncomingWelcome {
  messageId: number;
  /** Raw MLS Welcome bytes — feed straight into
   *  `Group.joinFromWelcome(identity, ...)`. */
  welcomeBytes: Uint8Array;
  createdAt: string;
}

/** A verified incoming `ContactAck` (Phase 1.9b1) — the
 *  accept-reply that closes the `sendContactRequest` handshake
 *  from the original sender's perspective. The Ed25519 signature
 *  on the ack has already been verified against `accepterPubkey`;
 *  the caller still has to match `ackToPseudonym` against the
 *  pending-sent table to pair it with the original request the
 *  user fired off. */
export interface IncomingContactAck {
  /** DS-assigned monotonic id — also serves as the cursor for
   *  the next poll. */
  messageId: number;
  /** Accepter's Ed25519 identity public key (32 bytes). The
   *  signature on the ack was verified against this. */
  accepterPubkey: Uint8Array;
  /** Accepter's `account_id` (16-byte UUIDv7). The original
   *  sender needs this to fetch the accepter's KeyPackage from
   *  discovery if they want to drive `startGroupWith` from
   *  their side (mirror of the Accept-side group-creation
   *  flow). */
  accepterAccountId: Uint8Array;
  /** Accepter's X25519 HPKE public key (32 bytes). Stored
   *  alongside the accepter's pubkey in the local address
   *  book so future replies can be sealed without a fresh
   *  discovery round-trip. */
  accepterX25519Pubkey: Uint8Array;
  /** The original request's `from_pseudonym` field. The
   *  receiver matches this against their own identity
   *  pseudonym (and, in 1.9b1, against the pending-sent
   *  table) to confirm the ack is for them. */
  ackToPseudonym: Uint8Array;
  /** When the accepter pressed Accept (ms since epoch). The
   *  client may surface this as "accepted at hh:mm" — the
   *  value is sender-attested so don't treat it as
   *  authoritative; the DS `createdAt` is the trust anchor. */
  acceptedAtMs: bigint;
  /** ISO-8601 timestamp the DS recorded on receive. */
  createdAt: string;
}

/** Discriminated event returned by `pollMailbox`. The kind tag
 *  matches the on-the-wire envelope discriminator (Phase
 *  1.16e3). Callers `switch` on it to dispatch to the right
 *  handler. */
export type IncomingMailboxEvent =
  | { kind: "contact-request"; request: IncomingContactRequest }
  | { kind: "welcome"; welcome: IncomingWelcome }
  | { kind: "contact-ack"; ack: IncomingContactAck };

/** Result of `ContactsClient.pollMailbox`. */
export interface PollResult {
  /** Discriminated events, in the order DS returned them
   *  (oldest first by message id). Mixed kinds are interleaved
   *  by arrival order — the caller is expected to dispatch on
   *  `event.kind`. */
  events: IncomingMailboxEvent[];
  /** New cursor — the message id of the last processed entry,
   *  or the input `after` if the poll returned nothing. Use as
   *  the next `after` to skip already-seen rows. */
  cursor: number;
  /** `true` if the DS reported `has_more` — the caller should
   *  loop and poll again with the new cursor. */
  hasMore: boolean;
}

export type ContactsClientError =
  | { kind: "no-identity-secrets" }
  | { kind: "username-unknown"; username: string }
  | { kind: "no-identity-bundle"; accountId: Uint8Array }
  | { kind: "wrapped"; cause: unknown };

/**
 * Throwable wrapper around `ContactsClientError`. Code that
 * wants to branch on the failure mode catches this and
 * inspects `error.kind`; UI code that just wants a message can
 * fall back to `error.message`.
 */
export class ContactsError extends Error {
  override name = "ContactsError";
  constructor(public readonly variant: ContactsClientError, message: string) {
    super(message);
  }

  static noIdentitySecrets(): ContactsError {
    return new ContactsError(
      { kind: "no-identity-secrets" },
      "please re-enter your password to send messages",
    );
  }

  static usernameUnknown(username: string): ContactsError {
    return new ContactsError(
      { kind: "username-unknown", username },
      `no account is registered as @${username}`,
    );
  }

  static noIdentityBundle(accountId: Uint8Array): ContactsError {
    return new ContactsError(
      { kind: "no-identity-bundle", accountId },
      "the recipient has not published an identity bundle yet",
    );
  }

  static wrap(cause: unknown): ContactsError {
    return new ContactsError(
      { kind: "wrapped", cause },
      cause instanceof Error ? cause.message : String(cause),
    );
  }
}

/**
 * G-4 (ADR-0016) — thrown when a peer's fetched discovery
 * identity bundle carries an AccountIdentity `signature_pubkey`
 * that DIFFERS from the locally-pinned key AND the change cannot
 * be authorised by a valid supersession proof.
 *
 * This is the **fail-closed** path (ADR-0016 rule #2): we refuse
 * to send a ContactRequest / start a group / add a member to the
 * unverified new key. The UI is expected to catch this and prompt
 * the user to re-verify the peer **out of band** (compare safety
 * numbers, re-scan a QR, etc.) before proceeding. We never
 * blind-TOFU a CHANGED key.
 *
 * `reason` discriminates the failure mode for diagnostics — the
 * UX is the same OOB-reverify prompt regardless.
 */
export type SupersessionRefusedReason =
  /** The bundle carried no `supersession_proof` at all. */
  | "proof-absent"
  /** `verifySupersession` threw (malformed / wrong-old-key /
   *  expired / out-of-range window). */
  | "proof-invalid"
  /** The proof verified but authorised a DIFFERENT key than the
   *  one the bundle actually served (defence in depth). */
  | "key-mismatch"
  /** The marker would move backwards — a within-window rollback
   *  to an older successor (rule #4). */
  | "marker-backward";

export class SupersessionRefused extends Error {
  override name = "SupersessionRefused";

  /**
   * KT-log S4 — best-effort transparency-log context for the NEW
   * (refused) key, attached so the UI can distinguish an
   * owner-initiated identity RESET from an unexplained key change.
   *
   *   * `true`  — the log's latest entry commits to EXACTLY the served
   *     new key AND records it as an owner-initiated reset (ADR-0017
   *     lost-everything fresh-start).
   *   * `false` — the log's latest entry commits to the served new key
   *     but does NOT record a reset (an unexplained change → possible
   *     compromise).
   *   * `null`  — the log is off / unreachable / the account is absent
   *     / the log's latest pubkey does not match the served key (the
   *     marker is then about a different key, so it is unknown).
   *
   * This is **best-effort enrichment only**: a log failure NEVER
   * changes the refusal — gate 1 (the supersession/pin gate) throws
   * regardless. It only colours the OOB-reverify prompt.
   */
  public loggedReset: boolean | null = null;

  constructor(
    public readonly reason: SupersessionRefusedReason,
    /** base64 of the account_id whose key change was refused. */
    public readonly accountIdB64: string,
    message: string,
  ) {
    super(message);
  }
}

/**
 * KT-log S3c — thrown when the transparency log's signed root reports a
 * `log_size` SMALLER than the highest this client has ever seen. An
 * append-only log cannot shrink, so a rollback is an unambiguous attack
 * (a server trying to hide a leaf it later appended). This is the ONE
 * transparency-log verdict that hard-fails the identity-fetch path; all
 * other verdicts (`absent` / `unreachable` / `off` / `mismatch`) are
 * recorded + surfaced but do NOT block, because the log is
 * eventually-consistent and the supersession/pin gate is the primary
 * trust (see `verifyBundleAccountKey`).
 */
export class LogRollbackDetected extends Error {
  override name = "LogRollbackDetected";
  constructor(
    /** base64 of the account_id whose fetch tripped the rollback. */
    public readonly accountIdB64: string,
  ) {
    super(
      "the transparency log appears to have shrunk (a smaller log_size " +
        "than previously observed) — an append-only log cannot do this, " +
        "so this is an unambiguous rollback attack. Refusing to proceed.",
    );
  }
}

/** Fixed serialized length of a supersession proof — mirrors
 *  `lakko_transparency::supersession::SUPERSESSION_PROOF_LEN`
 *  (`version(1) ‖ old(32) ‖ new(32) ‖ issued(8) ‖ expires(8) ‖
 *  sig(64)` = 145). */
const SUPERSESSION_PROOF_LEN = 1 + 32 + 32 + 8 + 8 + 64;
/** Byte offset of the big-endian i64 `expires_at` field within a
 *  supersession proof: after version(1) + old(32) + new(32) +
 *  issued(8). */
const SUPERSESSION_EXPIRES_AT_OFFSET = 1 + 32 + 32 + 8;

/**
 * Read the big-endian i64 `expires_at_unix_secs` field out of a
 * supersession proof's wire bytes (G-4). Used ONLY after
 * `verifySupersession` has authenticated the proof, so the parse
 * runs on trusted, fixed-length data — we still bound-check the
 * length defensively.
 *
 * Returns the value as a JS `number`. Unix-second timestamps are
 * ~1.7e9 (and the verifier caps the window at 30 days from issue),
 * well within `Number.MAX_SAFE_INTEGER`, so no precision is lost.
 */
function readProofExpiresAtSecs(proofBytes: Uint8Array): number {
  if (proofBytes.length < SUPERSESSION_PROOF_LEN) {
    // Should be unreachable — the verifier already accepted these
    // bytes — but never index past the buffer.
    throw new Error(
      `supersession proof too short (${proofBytes.length} bytes)`,
    );
  }
  const view = new DataView(
    proofBytes.buffer,
    proofBytes.byteOffset,
    proofBytes.byteLength,
  );
  // `getBigInt64` reads a signed 64-bit big-endian integer at the
  // expires_at offset; convert to a JS number for the marker store.
  const expiresBig = view.getBigInt64(SUPERSESSION_EXPIRES_AT_OFFSET, false);
  return Number(expiresBig);
}

export class ContactsClient {
  private readonly identityProvider: (() => WasmIdentity | null) | undefined;
  private readonly onPersist: (() => Promise<void>) | undefined;

  constructor(
    private readonly discovery: DiscoveryClient = new DiscoveryClient(),
    private readonly ds: DsClient = new DsClient(),
    options: ContactsClientOptions = {},
  ) {
    this.identityProvider = options.identityProvider;
    this.onPersist = options.onPersist;
  }

  /**
   * Resolve the `WasmIdentity` to drive an operation that needs
   * Identity material. When the canonical accessor is wired
   * (production path via `<ChatProvider>`), every call shares
   * the same provider — MLS state mutations accumulate, and
   * `onPersist` snapshots them to disk.
   *
   * Falls back to a one-shot
   * `Identity.fromSeed(secrets.accountIdentitySeed)` (ADR-0017 —
   * the phrase seed, was the export key) when the canonical isn't
   * wired (tests, direct callers). MLS
   * mutations on the throwaway are **lost on return** — fine for
   * stateless ops (signing a ContactRequest, opening an HPKE
   * envelope), broken for `startGroupWith` / `joinGroupFromWelcome`,
   * which is why production MUST wire the canonical accessor.
   */
  private async resolveIdentity(
    secrets: IdentitySecrets | null,
  ): Promise<WasmIdentity> {
    if (this.identityProvider !== undefined) {
      const canonical = this.identityProvider();
      if (canonical !== null) return canonical;
      throw ContactsError.noIdentitySecrets();
    }
    if (secrets === null) {
      throw ContactsError.noIdentitySecrets();
    }
    const wasm = await loadWasm();
    // ADR-0017: the canonical AccountIdentity is seeded from the
    // BIP39 phrase seed, not the OPAQUE export key.
    return wasm.Identity.fromSeed(secrets.accountIdentitySeed);
  }

  /** Fire the persist hook, swallow + log failures. */
  private async persistBestEffort(): Promise<void> {
    if (this.onPersist === undefined) return;
    try {
      await this.onPersist();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ContactsClient: MLS snapshot persist failed", err);
    }
  }

  /**
   * Verify a freshly-fetched peer identity bundle before its keys are
   * used. Two layers, in order:
   *
   *   1. **Supersession/pin gate (ADR-0016, G-4)** — the primary,
   *      fail-closed trust. TOFU-pins on first sight; on a changed key
   *      REQUIRES + verifies a supersession proof and re-pins; throws
   *      `SupersessionRefused` otherwise. See `verifySupersessionGate`.
   *
   *   2. **Transparency-log inclusion (KT-log S3c)** — the ADDITIVE
   *      equivocation-detection / gossip layer. Best-effort: it HARD
   *      FAILS only on a `"rollback"` (the log shrank — an unambiguous
   *      attack, surfaced as `LogRollbackDetected`); every other
   *      verdict is recorded on the pin record + surfaced but does NOT
   *      block (the log is eventually-consistent). See
   *      `checkTransparencyLog`.
   *
   * MUST be called after every `discovery.identityFetch` and BEFORE
   * the bundle's keys are used to send a ContactRequest / start a group
   * / add a member. The supersession gate runs FIRST so a pin exists
   * for the log check to record its verdict on; the log check NEVER
   * weakens the supersession gate — a `SupersessionRefused` from gate 1
   * propagates before gate 2 runs at all.
   *
   * @param accountId  the peer's raw 16-byte account_id.
   * @param bundle     the fetched `IdentityFetchResponse`.
   * @throws `SupersessionRefused` on a fail-closed supersession branch,
   *   `LogRollbackDetected` on a transparency-log rollback.
   */
  private async verifyBundleAccountKey(
    accountId: Uint8Array,
    bundle: IdentityFetchResponse,
  ): Promise<void> {
    // Gate 1 (primary, fail-closed). A throw here short-circuits gate 2.
    try {
      await this.verifySupersessionGate(accountId, bundle);
    } catch (err) {
      // KT-log S4 — on a fail-closed refusal of an UNPROVEN CHANGED key
      // (the moment the reset-vs-compromise distinction matters most),
      // enrich the error with a BEST-EFFORT transparency lookup of the
      // NEW key's `is_reset` marker. This NEVER weakens the gate: gate 1
      // already threw, we re-throw it unchanged save for the added
      // context. Any log fault leaves `loggedReset` null.
      if (err instanceof SupersessionRefused) {
        err.loggedReset = await this.bestEffortServedKeyReset(
          accountId,
          bundle,
        );
      }
      throw err;
    }

    // Gate 2 (additive, best-effort). Hard-fails only on rollback.
    const accountIdB64 = base64Encode(accountId);
    const servedPubkey = base64Decode(bundle.signature_pubkey);
    await this.checkTransparencyLog(accountId, accountIdB64, servedPubkey);
  }

  /**
   * KT-log S4 — best-effort lookup of the served (refused) key's
   * `is_reset` marker, used ONLY to enrich a `SupersessionRefused`.
   *
   * Wraps `lookupServedKeyReset` and swallows any throw to a `null`
   * (the enrichment must never break the already-failing refusal path).
   * Returns `true` / `false` only when the log's latest entry commits
   * to exactly the served key; `null` otherwise (off / unreachable /
   * absent / pubkey-divergence).
   */
  private async bestEffortServedKeyReset(
    accountId: Uint8Array,
    bundle: IdentityFetchResponse,
  ): Promise<boolean | null> {
    try {
      const servedPubkey = base64Decode(bundle.signature_pubkey);
      return await lookupServedKeyReset(accountId, servedPubkey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "ContactsClient: best-effort transparency reset lookup threw " +
          "(refusal stands; loggedReset stays null)",
        err,
      );
      return null;
    }
  }

  /**
   * G-4 (ADR-0016) — verify a freshly-fetched peer identity
   * bundle's AccountIdentity key against the local pin, re-pinning
   * (via a supersession proof) when the key rotated. MUST be
   * called after every `discovery.identityFetch` and BEFORE the
   * bundle's keys are used to send a ContactRequest / start a
   * group / add a member.
   *
   * Decision table (ADR-0016 rule #2, fail-closed):
   *   1. No existing pin → TOFU-pin the fetched key (marker 0).
   *      Trust-on-first-use is unchanged for a first observation.
   *   2. Pin matches the fetched `signature_pubkey` → proceed (no
   *      key change; the common steady-state case).
   *   3. Pin DIFFERS → REQUIRE `bundle.supersession_proof`:
   *        a. absent → throw `SupersessionRefused("proof-absent")`.
   *        b. `verifySupersession(proof, pinnedOldKey, now)` throws
   *           → `SupersessionRefused("proof-invalid")`.
   *        c. the verifier's authorised new key !==
   *           `bundle.signature_pubkey` → `SupersessionRefused(
   *           "key-mismatch")` (defence in depth — the proof must
   *           authorise exactly the served key).
   *        d. re-pin advancing the monotonic marker to the proof's
   *           expiry; a backward marker → `SupersessionRefused(
   *           "marker-backward")`.
   *      Only on a clean verify do we proceed with the new key.
   *
   * NEVER blind-TOFU a CHANGED key — that is the whole point of
   * the gate.
   *
   * @param accountId  The peer's raw 16-byte account_id (the
   *                   stable pin key, NOT the pubkey — rule #3).
   * @param bundle     The fetched `IdentityFetchResponse`.
   * @throws `SupersessionRefused` on any fail-closed branch.
   */
  private async verifySupersessionGate(
    accountId: Uint8Array,
    bundle: IdentityFetchResponse,
  ): Promise<void> {
    const accountIdB64 = base64Encode(accountId);
    const fetchedPubkeyB64 = bundle.signature_pubkey;

    const existing = readPin(accountIdB64);

    // Case 1 — first-ever observation. TOFU-pin and proceed.
    if (existing === undefined) {
      setPin(accountIdB64, fetchedPubkeyB64, 0);
      return;
    }

    // Case 2 — unchanged key. Nothing to verify.
    if (existing.pinnedSignaturePubkeyB64 === fetchedPubkeyB64) {
      return;
    }

    // Case 3 — the AccountIdentity key changed. A proof is now
    // mandatory; absence is an immediate fail-closed refusal.
    if (
      bundle.supersession_proof === undefined ||
      bundle.supersession_proof.length === 0
    ) {
      throw new SupersessionRefused(
        "proof-absent",
        accountIdB64,
        "the peer's account key changed and no supersession proof " +
          "was provided — refusing to trust the new key. Please " +
          "re-verify this contact out of band.",
      );
    }

    const wasm = await loadWasm();
    const proofBytes = base64Decode(bundle.supersession_proof);
    const pinnedOldPubkey = base64Decode(existing.pinnedSignaturePubkeyB64);
    const nowSecs = BigInt(Math.floor(Date.now() / 1000));

    // The verifier returns the authorised NEW pubkey, or THROWS on
    // a malformed / wrong-old-key / expired / out-of-window proof.
    let authorisedNewPubkey: Uint8Array;
    try {
      authorisedNewPubkey = wasm.verifySupersession(
        proofBytes,
        pinnedOldPubkey,
        nowSecs,
      );
    } catch (err) {
      throw new SupersessionRefused(
        "proof-invalid",
        accountIdB64,
        `the peer's account key changed and the supersession proof ` +
          `did not verify against the pinned key (${String(err)}) — ` +
          `refusing the new key. Please re-verify out of band.`,
      );
    }

    // Defence in depth — the proof must authorise EXACTLY the key
    // the bundle served. A proof that verifies but names a third
    // key is rejected (a malicious server splicing an unrelated
    // valid proof onto a substituted key).
    const authorisedNewPubkeyB64 = base64Encode(authorisedNewPubkey);
    if (authorisedNewPubkeyB64 !== fetchedPubkeyB64) {
      throw new SupersessionRefused(
        "key-mismatch",
        accountIdB64,
        "the supersession proof verified but authorised a different " +
          "key than the bundle served — refusing. Please re-verify " +
          "out of band.",
      );
    }

    // Advance the monotonic marker (ADR-0016 rule #4) to the
    // proof's own `expires_at`. A genuine later rotation carries a
    // strictly-newer `issued_at` (hence a newer `expires_at` for a
    // fixed emit window), so a stale older successor's proof —
    // whose `expires_at` predates the current marker — is refused.
    // Re-observing the SAME transition re-pins at the same marker
    // (`>=`, allowed). We read `expires_at` from the already-
    // verified proof bytes (the verifier confirmed the signature
    // over exactly these fields, so the parse is on trusted data).
    const proofExpiresAt = readProofExpiresAtSecs(proofBytes);
    const result = setPin(accountIdB64, fetchedPubkeyB64, proofExpiresAt);
    if (result === "rejected-backward") {
      throw new SupersessionRefused(
        "marker-backward",
        accountIdB64,
        "the supersession proof would move the rotation marker " +
          "backwards (an older successor) — refusing. Please " +
          "re-verify out of band.",
      );
    }
  }

  /**
   * KT-log S3c — consult the operator-signed transparency log to
   * confirm the served identity is in the public append-only log
   * (equivocation detection on top of the supersession/pin gate).
   *
   * Runs AFTER `verifyBundleAccountKey`'s supersession gate has
   * TOFU-pinned / verified the key, so a pin always exists when we
   * record the verdict. Best-effort POSTURE (ADR-0016 stays the
   * primary, fail-closed trust):
   *
   *   * `"rollback"`  → HARD FAIL. Throw `LogRollbackDetected`. The
   *     log shrank — an unambiguous attack. This is the ONLY blocking
   *     verdict for S3c.
   *   * `"mismatch"`  → record + prominent `console.warn`, but DO NOT
   *     block (S3c). Hard-blocking on mismatch is the natural
   *     fail-closed refinement and is flagged for `lakko-security` to
   *     rule on.
   *   * `"absent"` / `"unreachable"` / `"off"` → record + a
   *     `console.warn` (for absent/unreachable) and proceed. The log
   *     is eventually-consistent; the supersession/pin gate already
   *     authorised the key.
   *
   * The recorded status travels on the account's pin record so the UI
   * can surface it. Any UNEXPECTED throw from the log machinery (it is
   * designed not to throw except for the deliberate rollback) is
   * swallowed and logged — the additive check must never break the
   * primary fetch path.
   *
   * @param accountId    the peer's raw 16-byte account_id.
   * @param accountIdB64 its base64 form (the pin-store key).
   * @param servedPubkey the 32-byte Ed25519 key discovery served.
   * @throws `LogRollbackDetected` ONLY on a `"rollback"` verdict.
   */
  private async checkTransparencyLog(
    accountId: Uint8Array,
    accountIdB64: string,
    servedPubkey: Uint8Array,
  ): Promise<void> {
    let status: LogVerifyStatus;
    let isReset: boolean | null;
    try {
      ({ status, isReset } = await verifyIdentityInLog(accountId, servedPubkey));
    } catch (err) {
      // The verifier is designed to map faults to a status and only
      // throw on `LogRollbackDetected` semantics (which it surfaces as
      // the `"rollback"` return, not a throw). An unexpected throw here
      // must NOT break the additive check — log + proceed.
      // eslint-disable-next-line no-console
      console.warn(
        "ContactsClient: transparency-log check threw unexpectedly " +
          "(proceeding; supersession/pin gate already authorised the key)",
        err,
      );
      return;
    }

    // Rollback is the one hard failure (unambiguous attack).
    if (status === "rollback") {
      throw new LogRollbackDetected(accountIdB64);
    }

    // Record the verdict on the pin record (best-effort; the pin
    // exists because the supersession gate ran first). KT-log S4 also
    // persists the `/latest` entry's `is_reset` marker next to the
    // status (a `null` clears any stale marker — no latest was read).
    recordLogStatus(accountIdB64, status);
    recordLogReset(accountIdB64, isReset);

    if (status === "mismatch") {
      // Surfaced-but-not-blocked for S3c. The natural fail-closed
      // refinement (hard-block on mismatch) is deferred to a
      // lakko-security ruling — see the method doc + the S3c report.
      // eslint-disable-next-line no-console
      console.warn(
        `ContactsClient: transparency-log MISMATCH for account ` +
          `${accountIdB64} — the log's latest key differs from what ` +
          `discovery served (or the inclusion proof failed). Proceeding ` +
          `for S3c (supersession/pin gate is primary); flag for review.`,
      );
    } else if (status === "absent" || status === "unreachable") {
      // eslint-disable-next-line no-console
      console.warn(
        `ContactsClient: transparency-log check inconclusive ` +
          `(${status}) for account ${accountIdB64} — proceeding; the ` +
          `log is eventually-consistent and the pin gate is primary.`,
      );
    }
    // `confirmed` / `off` are quiet (the happy path / disabled).
  }

  /**
   * Send a `ContactRequest` envelope to `@recipientUsername`,
   * sealed under the recipient's published X25519 key.
   *
   * @param identity     The current session's `IdentitySecrets`
   *                     from `useAuth().identitySecrets()`. Must
   *                     not be `null` — the caller has already
   *                     verified the user is authenticated and
   *                     has a fresh export key.
   * @param recipientUsername  The handle the user typed. The
   *                     normalised form is what gets queried;
   *                     casing / whitespace are forgiven.
   * @param introMessage Optional intro bytes (encoded by the
   *                     UI, typically UTF-8). Empty array sends
   *                     a "ping" with no intro payload.
   *
   * Throws `ContactsError`. Inspect `.variant.kind` for routing.
   */
  async sendContactRequest(
    identity: IdentitySecrets | null,
    recipientUsername: string,
    introMessage: Uint8Array,
  ): Promise<SendResult> {
    // 1. Resolve the username.
    let lookup;
    try {
      lookup = await this.discovery.lookup(recipientUsername);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (lookup.account === null) {
      throw ContactsError.usernameUnknown(lookup.normalisedUsername);
    }
    const recipientAccountId = lookup.account;

    // 2. Fetch the recipient's identity bundle.
    let bundle;
    try {
      bundle = await this.discovery.identityFetch(recipientAccountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (bundle === null) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }
    // G-4 (ADR-0016) — verify the recipient's AccountIdentity key
    // against the local pin BEFORE using it. TOFU-pins on first
    // contact; on a changed key, REQUIRES + verifies a supersession
    // proof and re-pins. Throws `SupersessionRefused` (fail-closed)
    // on a changed key with no valid proof — the UI surfaces an
    // OOB re-verify prompt. We never blind-TOFU a changed key.
    await this.verifyBundleAccountKey(recipientAccountId, bundle);
    const recipientSignaturePub = base64Decode(bundle.signature_pubkey);
    const recipientX25519Pub = base64Decode(bundle.x25519_pubkey);

    const publish = await this.composeAndPublishContactRequest(
      identity,
      recipientSignaturePub,
      recipientX25519Pub,
      introMessage,
    );

    return {
      recipientUsername: lookup.normalisedUsername,
      recipientAccountId,
      messageId: publish.messageId,
      createdAt: publish.createdAt,
    };
  }

  /**
   * Send a `ContactRequest` directly against a known recipient
   * bundle — i.e. **without** going through the discovery
   * `lookup → identityFetch` round-trip.
   *
   * The invite-link flow (1.18c) is the canonical caller: the
   * `InviteCapability` already carries `owner_account_id`,
   * `owner_signature_pubkey`, and `owner_x25519_pubkey`, so the
   * joiner has everything needed to seal an envelope without
   * touching `services/discovery`. Useful both for the happy
   * path (faster onboarding from a link) and for the censored-
   * region path (`/discovery/*` may be blocked while the link
   * still works because the bytes are inline).
   *
   * @param identity              Current session secrets — same
   *                              gate as `sendContactRequest`.
   * @param recipientAccountId    16-byte UUIDv7 (caller MUST
   *                              cross-check against the
   *                              capability's `ownerAccountId`).
   * @param recipientSignaturePubkey 32-byte Ed25519 pubkey from
   *                              the capability.
   * @param recipientX25519Pubkey 32-byte X25519 pubkey from the
   *                              capability — used to seal the
   *                              intro envelope.
   * @param introMessage          Optional intro bytes (UTF-8 or
   *                              empty array).
   *
   * Returns the same `SendResult` shape `sendContactRequest`
   * produces. `recipientUsername` is left empty — the joiner
   * does not have it (no discovery round-trip happened).
   */
  async sendContactRequestToBundle(
    identity: IdentitySecrets | null,
    recipientAccountId: Uint8Array,
    recipientSignaturePubkey: Uint8Array,
    recipientX25519Pubkey: Uint8Array,
    introMessage: Uint8Array,
  ): Promise<SendResult> {
    const publish = await this.composeAndPublishContactRequest(
      identity,
      recipientSignaturePubkey,
      recipientX25519Pubkey,
      introMessage,
    );
    return {
      recipientUsername: "",
      recipientAccountId,
      messageId: publish.messageId,
      createdAt: publish.createdAt,
    };
  }

  /** Inner helper extracted from `sendContactRequest` (and reused
   *  by `sendContactRequestToBundle`) — given a pre-resolved
   *  recipient bundle (Ed25519 + X25519 pubkeys), compose the
   *  envelope and publish it to the recipient's stranger-inbox.
   *  Returns the DS receipt fields (`messageId` + `createdAt`).
   *
   *  Pulled out so the two public flows don't drift on subtle
   *  details (e.g. the `from_pseudonym` derivation, the
   *  `KIND_CONTACT_REQUEST` envelope wrap, the canonical
   *  Identity gate from 1.16e6c2). */
  private async composeAndPublishContactRequest(
    identity: IdentitySecrets | null,
    recipientSignaturePubkey: Uint8Array,
    recipientX25519Pubkey: Uint8Array,
    introMessage: Uint8Array,
  ): Promise<{ messageId: number; createdAt: string }> {
    // The canonical Identity (1.16e6c2) is the same instance the
    // ChatProvider holds — using it here keeps Ed25519 + X25519
    // keys consistent across operations and avoids the
    // throwaway-provider trap.
    const wasm = await loadWasm();
    const senderIdentity = await this.resolveIdentity(identity);
    const senderSigningSeed = senderIdentity.ed25519Seed;
    const senderAccountId = identity?.accountId ?? null;
    if (senderAccountId === null) {
      throw ContactsError.noIdentitySecrets();
    }

    // The recipient's mailbox is keyed by their public-pubkey
    // pseudonym — anyone holding the recipient's Ed25519
    // identity pubkey can compute the same 32-byte value, by
    // design. This is the routing address for cold contact
    // requests (see ADR-0005 §13.7).
    const recipientPseudonym = wasm.IdentityPseudonym.fromIdentity(
      recipientSignaturePubkey,
    ).asBytes();

    // Sender's `from_pseudonym` field. For a first cold contact
    // we use the sender's identity pseudonym (public, derivable
    // by anyone holding the sender's pubkey); the receiver
    // uses it to address an ack. Per-relationship pseudonyms
    // (which rotate after the handshake) are 1.16d3d's job.
    const senderPseudonym = wasm.IdentityPseudonym.fromIdentity(
      senderIdentity.signaturePublicKey,
    ).asBytes();

    const validAfterMs = BigInt(Date.now());
    // 1.9b2: Privacy Pass / App-Attest / Play-Integrity token.
    // The web client has no platform attestation API in 2026 —
    // we ship an empty token, which lands the request in the DS
    // ingest layer's "no attestation tier" rate-limit bucket
    // (lowest ceiling). Production mobile builds populate this
    // from the platform attestation API.
    const privacyPassToken = new Uint8Array(0);
    let wireBytes: Uint8Array;
    try {
      // 1.16e3b: pass our own account_id so the receiver can
      // drive Accept-side group creation without a separate
      // reverse-lookup round-trip.
      wireBytes = wasm.ContactRequest.compose(
        senderSigningSeed,
        senderAccountId,
        recipientX25519Pubkey,
        senderPseudonym,
        validAfterMs,
        privacyPassToken,
        introMessage,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // Publish to the DS, wrapped with the kind discriminator so
    // the receiver's pollMailbox can distinguish a
    // ContactRequest from a Welcome (1.16e3).
    const envelope = wrapEnvelope(KIND_CONTACT_REQUEST, wireBytes);
    let publish;
    try {
      publish = await this.ds.publishMailbox(recipientPseudonym, envelope);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    return { messageId: publish.id, createdAt: publish.createdAt };
  }

  /**
   * Send a `ContactAck` envelope back to the original sender's
   * stranger inbox (Phase 1.9b1).
   *
   * Called from the recipient's "Accept" handler **after** they
   * have decided to accept an incoming `ContactRequest`. The ack
   * is independent of group creation — the recipient still
   * drives `startGroupWith(sender)` separately to spin the 1×1
   * MLS group up. Splitting the two surfaces keeps the
   * "I want to know you exist" signal cheap (one signed envelope,
   * no MLS state change) and lets the original sender's UI flip
   * from "pending" to "accepted" without waiting on the joiner
   * to complete the more expensive Group / Welcome dance.
   *
   * **Routing:** the ack body carries `ackToPseudonym = original
   * request's fromPseudonym`. The publish target is the sender's
   * stranger-inbox mailbox — derived from `original.fromPubkey`
   * via `IdentityPseudonym.fromIdentity` (the same public-pubkey
   * derivation the contact-request flow uses for the recipient's
   * mailbox).
   *
   * **Threat-model note:** the ack carries only public material
   * (pubkeys + account_id + a self-attested timestamp). The
   * Ed25519 signature proves "this came from `accepterPubkey`";
   * the freshness of `ackToPseudonym` (matched against the
   * sender's pending-sent table on receive) drops replays and
   * unrelated acks from a malicious accepter spamming arbitrary
   * pseudonyms.
   */
  async sendContactAck(
    identity: IdentitySecrets | null,
    originalRequest: IncomingContactRequest,
  ): Promise<{ messageId: number; createdAt: string }> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveIdentity(identity);

    const myAccountId = identity?.accountId ?? null;
    if (myAccountId === null) {
      throw ContactsError.noIdentitySecrets();
    }

    // Sender's mailbox = `IdentityPseudonym.fromIdentity(sender_pubkey)`.
    // The original request's `fromPubkey` is the routing handle.
    const senderMailbox = wasm.IdentityPseudonym.fromIdentity(
      originalRequest.fromPubkey,
    ).asBytes();

    const acceptedAtMs = BigInt(Date.now());

    let wireBytes: Uint8Array;
    try {
      wireBytes = wasm.ContactAck.compose(
        myIdentity.ed25519Seed,
        myAccountId,
        myIdentity.x25519PublicKey,
        originalRequest.fromPseudonym,
        acceptedAtMs,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    const envelope = wrapEnvelope(KIND_CONTACT_ACK, wireBytes);
    let publish;
    try {
      publish = await this.ds.publishMailbox(senderMailbox, envelope);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    return { messageId: publish.id, createdAt: publish.createdAt };
  }

  /**
   * Send a Welcome (MLS group-join blob) to the recipient via
   * their stranger-inbox mailbox. Used by `startGroupWith`
   * once the sender has called `Group.addMember(recipientKp)`
   * and gotten the welcome bytes back.
   *
   * Routing target: same `IdentityPseudonym.fromIdentity(...)`
   * mailbox the ContactRequest flow uses — the receiver
   * polls one place and dispatches by the kind byte.
   *
   * The Welcome blob is itself authenticated by the sender's
   * KeyPackage signature (RFC 9420 §12.4); our envelope just
   * routes it.
   *
   * @param recipientSignaturePubkey  Recipient's Ed25519
   *                                  identity public key (32
   *                                  bytes) — the same value
   *                                  the receiver hands to
   *                                  `Identity.fromSeed`'s
   *                                  upload bundle. The
   *                                  mailbox pseudonym is
   *                                  derived from this pubkey
   *                                  via `IdentityPseudonym.fromIdentity`.
   * @param welcomeBytes              `welcome` field of the
   *                                  `AddMember` result.
   */
  async sendWelcome(
    recipientSignaturePubkey: Uint8Array,
    welcomeBytes: Uint8Array,
  ): Promise<{ messageId: number; createdAt: string }> {
    const wasm = await loadWasm();
    const recipientPseudonym = wasm.IdentityPseudonym.fromIdentity(
      recipientSignaturePubkey,
    ).asBytes();
    const envelope = wrapEnvelope(KIND_WELCOME, welcomeBytes);
    let publish;
    try {
      publish = await this.ds.publishMailbox(recipientPseudonym, envelope);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    return {
      messageId: publish.id,
      createdAt: publish.createdAt,
    };
  }

  /**
   * Poll the user's stranger-inbox mailbox for new
   * ContactRequests, decode + verify each, return the
   * processed list plus a fresh cursor.
   *
   * @param identity   Current session secrets — used to derive
   *                   the mailbox pseudonym (from the sender's
   *                   own Ed25519 pubkey via
   *                   `IdentityPseudonym.fromIdentity`) and
   *                   the X25519 secret needed to unseal each
   *                   envelope.
   * @param after      Cursor from the previous poll. Pass `0`
   *                   on first call to fetch the entire
   *                   backlog.
   * @param waitSecs   Long-poll budget in seconds (default 0 —
   *                   immediate return). The DS clamps to its
   *                   own ceiling.
   *
   * Malformed entries on the wire are **dropped silently** —
   * the verifier rejects bad signatures, decoding errors are
   * logged via `console.warn` and skipped. We don't surface a
   * "garbage on the wire" error to the UI because every honest
   * client just wants to keep moving past corrupted entries.
   */
  async pollMailbox(
    identity: IdentitySecrets | null,
    after: number,
    waitSecs = 0,
  ): Promise<PollResult> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveIdentity(identity);
    const myPseudonym = wasm.IdentityPseudonym.fromIdentity(
      myIdentity.signaturePublicKey,
    ).asBytes();
    const myX25519Secret = myIdentity.x25519SecretKey;

    let raw;
    try {
      raw = await this.ds.fetchMailbox(myPseudonym, { after, waitSecs });
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    const events: IncomingMailboxEvent[] = [];
    let cursor = after;
    for (const message of raw.messages) {
      cursor = message.id;

      // Strip the envelope kind byte (1.16e3) and dispatch on
      // the discriminator. Unknown kinds are dropped with a
      // warn; cursor still advances so we don't re-process.
      const env = parseEnvelope(message.payload);
      if (env.kind === "unknown") {
        // eslint-disable-next-line no-console
        console.warn(
          "mailbox envelope: unknown kind",
          message.id,
          env.rawKind,
        );
        continue;
      }
      if (env.kind === KIND_WELCOME) {
        events.push({
          kind: "welcome",
          welcome: {
            messageId: message.id,
            welcomeBytes: env.body,
            createdAt: message.createdAt,
          },
        });
        continue;
      }
      if (env.kind === KIND_CONTACT_ACK) {
        // Phase 1.9b1: signature is verified inside the WASM
        // surface; bad-signature / malformed-wire entries drop
        // with a warn and the cursor still advances.
        let ack;
        try {
          ack = wasm.ContactAck.decodeAndVerify(env.body);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("ContactAck verify failed", message.id, err);
          continue;
        }
        events.push({
          kind: "contact-ack",
          ack: {
            messageId: message.id,
            accepterPubkey: ack.accepterPubkey,
            accepterAccountId: ack.accepterAccountId,
            accepterX25519Pubkey: ack.accepterX25519Pubkey,
            ackToPseudonym: ack.ackToPseudonym,
            acceptedAtMs: ack.acceptedAtMs,
            createdAt: message.createdAt,
          },
        });
        continue;
      }
      // env.kind === KIND_CONTACT_REQUEST
      let parsed;
      try {
        parsed = wasm.ContactRequest.decodeAndVerify(env.body);
      } catch (err) {
        // Bad signature, malformed wire bytes, or some other
        // verifier failure. Drop and move on — the cursor
        // advances so we won't re-process the same row.
        // eslint-disable-next-line no-console
        console.warn("ContactRequest verify failed", message.id, err);
        continue;
      }

      let intro: string | null = null;
      try {
        const introBytes = parsed.openEnvelope(myX25519Secret);
        intro = new TextDecoder("utf-8", { fatal: false }).decode(introBytes);
      } catch (err) {
        // Envelope didn't decrypt for our X25519 secret. Could
        // be a malicious request, could be a bug; either way
        // we still surface the request to the user (signature
        // is verified) but with no intro body.
        // eslint-disable-next-line no-console
        console.warn(
          "ContactRequest envelope unseal failed",
          message.id,
          err,
        );
      }

      // `fromAccountId` is `Uint8Array | undefined` on the
      // wasm-bindgen surface — normalise to `null` for the
      // discriminated TS API (the UI distinguishes
      // present-but-empty from absent).
      const fromAccountId =
        parsed.fromAccountId === undefined ? null : parsed.fromAccountId;
      events.push({
        kind: "contact-request",
        request: {
          messageId: message.id,
          fromPubkey: parsed.fromPubkey,
          fromAccountId,
          fromPseudonym: parsed.fromPseudonym,
          validAfterMs: parsed.validAfterMs,
          intro,
          createdAt: message.createdAt,
        },
      });
    }

    return { events, cursor, hasMore: raw.hasMore };
  }

  /**
   * Start a new 1×1 MLS group with the given recipient (the
   * accepter side of a ContactRequest handshake, or any
   * unilateral group-creation flow).
   *
   * Steps:
   *   1. Fetch the recipient's KeyPackage from
   *      `/discovery/keypackage/{acct}` (Phase 1.16e1).
   *   2. Build our own `Group` with the local Identity
   *      derived from the OPAQUE export key.
   *   3. `group.addMember(keyPackageBytes)` produces the
   *      `(commit, welcome)` pair. Per MLS, `commit` would
   *      normally be broadcast to all existing members — but
   *      our group is one-of-one before this call, so there's
   *      no one to broadcast to. The commit is dropped.
   *   4. POST the welcome bytes to the recipient's mailbox
   *      via `sendWelcome`.
   *
   * Returns the freshly-created `Group` reference. The caller
   * is responsible for keeping it alive for the duration of
   * the chat session — Phase-1 does NOT persist MLS state, so
   * dropping the reference loses the group on this device
   * (the recipient still has theirs from `joinFromWelcome`).
   */
  async startGroupWith(
    identity: IdentitySecrets | null,
    recipientAccountId: Uint8Array,
    recipientSignaturePubkey: Uint8Array,
  ): Promise<{ group: WasmGroup; welcomeMessageId: number }> {
    const wasm = await loadWasm();

    // 1. Fetch recipient's KeyPackage.
    let keyPackageBytes;
    try {
      keyPackageBytes = await this.discovery.fetchKeyPackage(
        recipientAccountId,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (keyPackageBytes === null) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }

    // 2. Resolve our canonical Identity (1.16e6c2 — same instance
    //    every Group on this device shares; mutations to its
    //    provider survive across calls and get snapshotted to
    //    IDB after this method returns).
    const myIdentity = await this.resolveIdentity(identity);
    const group = new wasm.Group(myIdentity);

    // 3. Add the recipient. `addMember` takes the local
    //    Identity as its first argument (the wasm-bindgen
    //    surface decouples Group state from the Identity that
    //    operates on it). The commit would normally fan out to
    //    existing members; we have none, so we drop it.
    let added;
    try {
      added = group.addMember(myIdentity, keyPackageBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    // `commit` returned by addMember — unused in 1-of-1, but
    // grab it explicitly so the dropped value is documented.
    void added.commit;
    const welcomeBytes = added.welcome;

    // 4. Ship the welcome to the recipient via DS.
    let publish;
    try {
      publish = await this.sendWelcome(recipientSignaturePubkey, welcomeBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 1.16e6c2: Group::create + addMember mutated the canonical
    // provider's storage (initial epoch state, leaf private key,
    // own KeyPackage init key, queued Welcome state). Snapshot
    // it so the next reload can `Group::load` this group_id and
    // pick up where we left off.
    await this.persistBestEffort();

    return { group, welcomeMessageId: publish.messageId };
  }

  /**
   * Convenience wrapper around `startGroupWith` that resolves
   * `@username` first.
   *
   * Phase-1 the natural Accept-driven path (Bob accepts
   * Alice's ContactRequest → Bob spins up the group →
   * Bob ships a Welcome to Alice) is blocked by a missing
   * piece: the incoming ContactRequest carries Alice's
   * Ed25519 pubkey but NOT her account_id, so Bob can't
   * `fetchKeyPackage` to seed the group. Closing that gap
   * needs either a wire-format extension on ContactRequest
   * or a server-side reverse lookup
   * `signature_pubkey → account_id` — both tracked as
   * **1.16e3b** work.
   *
   * Until then this method gives the UI an explicit
   * "Start group with @bob" button that drives the full flow
   * from the sender side. The receive side picks up the
   * Welcome via `pollMailbox` and dispatches via
   * `joinGroupFromWelcome`.
   */
  async startGroupWithUsername(
    identity: IdentitySecrets | null,
    username: string,
  ): Promise<{
    group: WasmGroup;
    recipientUsername: string;
    recipientAccountId: Uint8Array;
    welcomeMessageId: number;
  }> {
    if (identity === null) {
      throw ContactsError.noIdentitySecrets();
    }
    let lookup;
    try {
      lookup = await this.discovery.lookup(username);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (lookup.account === null) {
      throw ContactsError.usernameUnknown(lookup.normalisedUsername);
    }

    let bundle;
    try {
      bundle = await this.discovery.identityFetch(lookup.account);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (bundle === null) {
      throw ContactsError.noIdentityBundle(lookup.account);
    }
    // G-4 (ADR-0016) — gate the group-init key the same way as the
    // contact-request path: TOFU on first sight, verified re-pin on
    // a key change, fail-closed (`SupersessionRefused`) otherwise.
    await this.verifyBundleAccountKey(lookup.account, bundle);
    const recipientSignaturePubkey = base64Decode(bundle.signature_pubkey);

    const out = await this.startGroupWith(
      identity,
      lookup.account,
      recipientSignaturePubkey,
    );
    return {
      group: out.group,
      recipientUsername: lookup.normalisedUsername,
      recipientAccountId: lookup.account,
      welcomeMessageId: out.welcomeMessageId,
    };
  }

  /**
   * Receive side of `startGroupWith`. Decodes the Welcome
   * blob into a fully-initialised `Group` for the local
   * Identity.
   *
   * Pure WASM call; no network round-trips. Throws
   * `ContactsError.wrap(...)` if the Welcome decode fails
   * (malformed bytes, init_key mismatch, etc.).
   */
  async joinGroupFromWelcome(
    identity: IdentitySecrets | null,
    welcomeBytes: Uint8Array,
  ): Promise<WasmGroup> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveIdentity(identity);
    let group: WasmGroup;
    try {
      // wasm-bindgen surface: `joinFromWelcome(welcome, identity)`.
      group = wasm.Group.joinFromWelcome(welcomeBytes, myIdentity);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 1.16e6c2: joinFromWelcome bootstrapped a brand-new group
    // state into the canonical provider (epoch keys, ratchet
    // tree, our leaf, the queued ack to the welcomer). Snapshot
    // it so the next reload can `Group::load` this group_id.
    await this.persistBestEffort();

    return group;
  }

  /**
   * Add a new member to an **existing** MLS group (Phase 1.18g).
   *
   * Differs from [`startGroupWith`] in two crucial ways:
   *
   *   1. The group is already populated — the commit returned by
   *      `Group.addMember(...)` MUST be broadcast to the DS group
   *      journal so every existing member's next `pollMessages`
   *      advances their local epoch. (In 1-of-1 group creation the
   *      commit is dropped; here it's load-bearing.)
   *   2. The Welcome still ships to the new member's
   *      stranger-inbox, same as `startGroupWith`. The new member
   *      lands at the post-commit epoch — they're a leaf at the
   *      tail of the ratchet tree.
   *
   * Returns the DS message id of the **commit** publish — the
   * caller (typically `Chat.tsx`'s "Add member" handler) must
   * register this id with the `ChatClient.markOwnPublishedCommit(id)`
   * to suppress the echo on the sender's own next poll. Without
   * that suppression `processIncoming` raises "cannot process own
   * outgoing commit at the same epoch" and the timeline shows a
   * spurious warning row.
   *
   * The Welcome message id is also returned for UI logging
   * (mirror of `startGroupWith`'s `welcomeMessageId`).
   */
  async addMemberToGroup(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    recipientAccountId: Uint8Array,
    recipientSignaturePubkey: Uint8Array,
  ): Promise<{ commitMessageId: number; welcomeMessageId: number }> {
    // 1. Fetch the new member's KeyPackage from discovery.
    //    Identical to `startGroupWith`'s step 1.
    let keyPackageBytes;
    try {
      keyPackageBytes = await this.discovery.fetchKeyPackage(recipientAccountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (keyPackageBytes === null) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }

    // 2. Resolve the canonical Identity — same instance every
    //    Group on this device uses (1.16e6c2). The `addMember`
    //    call below mutates this Identity's provider storage:
    //    appends the new member's leaf, increments the epoch,
    //    queues the Welcome state for the recipient.
    const myIdentity = await this.resolveIdentity(identity);

    // 3. Drive the MLS add. Returns `{ commit, welcome }` — the
    //    commit is the wire bytes existing members process to
    //    advance their tree; the welcome is what the new member
    //    decodes via `joinFromWelcome`.
    let added;
    try {
      added = group.addMember(myIdentity, keyPackageBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    const commitBytes = added.commit;
    const welcomeBytes = added.welcome;

    // 4. **Broadcast the commit** to the DS group journal. This
    //    is the new step that differentiates this method from
    //    `startGroupWith` — without it the existing members' local
    //    state never moves past the pre-add epoch and the new
    //    member's Welcome can't be reconciled when it arrives.
    let commitPublish;
    try {
      commitPublish = await this.ds.publishGroup(group.groupId, commitBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 5. Ship the Welcome to the new member's stranger inbox.
    //    Identical to `startGroupWith`'s step 4.
    let welcomePublish;
    try {
      welcomePublish = await this.sendWelcome(
        recipientSignaturePubkey,
        welcomeBytes,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 6. Snapshot the post-add provider state: new leaf, new
    //    epoch keys, pruned init keys for the consumed KeyPackage.
    //    Auto-persist follows the 1.16e6c2 best-effort policy —
    //    a failure here doesn't unwind the network publishes
    //    (those are already public on the DS).
    await this.persistBestEffort();

    return {
      commitMessageId: commitPublish.id,
      welcomeMessageId: welcomePublish.messageId,
    };
  }

  /**
   * Remove a member from an MLS group by leaf index (Phase 1.18h).
   *
   * Symmetric to [`addMemberToGroup`] minus the Welcome step —
   * the kicked member receives nothing direct from us; their
   * own next `pollMessages` lands the commit they cannot decrypt
   * (their leaf was removed) which surfaces as a generic
   * "you're no longer in this group" signal at the application
   * layer. The Welcome path simply doesn't exist for removes.
   *
   * Steps:
   *   1. Resolve the canonical Identity.
   *   2. `group.removeMember(identity, leafIndex)` returns the
   *      wire-encoded commit bytes. The local group state is
   *      already at the post-remove epoch.
   *   3. Broadcast the commit via `ds.publishGroup` — every
   *      remaining member processes it on their next poll and
   *      advances locally.
   *   4. `persistBestEffort` snapshots the new state.
   *
   * Caller (typically `Chat.tsx`'s "Remove" button per leaf row)
   * must register the returned `commitMessageId` with
   * `ChatClient.markOwnPublishedCommit` to suppress the echo —
   * same rationale as `addMemberToGroup`.
   */
  async removeMemberFromGroup(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    leafIndex: number,
  ): Promise<{ commitMessageId: number }> {
    const myIdentity = await this.resolveIdentity(identity);

    let commitBytes: Uint8Array;
    try {
      commitBytes = group.removeMember(myIdentity, leafIndex);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    let commitPublish;
    try {
      commitPublish = await this.ds.publishGroup(group.groupId, commitBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    await this.persistBestEffort();

    return { commitMessageId: commitPublish.id };
  }

  /** Convenience wrapper around [`addMemberToGroup`] that
   *  resolves `@username` first. Mirror of
   *  [`startGroupWithUsername`] for the existing-group path. */
  async addMemberToGroupByUsername(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    username: string,
  ): Promise<{
    recipientUsername: string;
    recipientAccountId: Uint8Array;
    commitMessageId: number;
    welcomeMessageId: number;
  }> {
    if (identity === null) {
      throw ContactsError.noIdentitySecrets();
    }
    let lookup;
    try {
      lookup = await this.discovery.lookup(username);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (lookup.account === null) {
      throw ContactsError.usernameUnknown(lookup.normalisedUsername);
    }

    let bundle;
    try {
      bundle = await this.discovery.identityFetch(lookup.account);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (bundle === null) {
      throw ContactsError.noIdentityBundle(lookup.account);
    }
    // G-4 (ADR-0016) — gate the new member's AccountIdentity key
    // through the pin store before adding them: TOFU on first
    // sight, verified re-pin on a key change, fail-closed
    // (`SupersessionRefused`) on a changed key with no valid proof.
    await this.verifyBundleAccountKey(lookup.account, bundle);
    const recipientSignaturePubkey = base64Decode(bundle.signature_pubkey);

    const out = await this.addMemberToGroup(
      identity,
      group,
      lookup.account,
      recipientSignaturePubkey,
    );
    return {
      recipientUsername: lookup.normalisedUsername,
      recipientAccountId: lookup.account,
      commitMessageId: out.commitMessageId,
      welcomeMessageId: out.welcomeMessageId,
    };
  }
}
