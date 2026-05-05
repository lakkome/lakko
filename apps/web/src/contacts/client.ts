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
import { base64Decode } from "../auth/account-id";
import { DsClient } from "../ds/client";
import type { IdentitySecrets } from "../auth/AuthContext";

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

/** Result of `ContactsClient.pollMailbox`. */
export interface PollResult {
  /** Verified contact requests, in the order DS returned them
   *  (oldest first by message id). */
  requests: IncomingContactRequest[];
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

export class ContactsClient {
  constructor(
    private readonly discovery: DiscoveryClient = new DiscoveryClient(),
    private readonly ds: DsClient = new DsClient(),
  ) {}

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
    if (identity === null) {
      throw ContactsError.noIdentitySecrets();
    }

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
    const recipientSignaturePub = base64Decode(bundle.signature_pubkey);
    const recipientX25519Pub = base64Decode(bundle.x25519_pubkey);

    // 3. Derive the recipient's mailbox pseudonym + the
    //    sender-side per-relationship pseudonym + the wire
    //    bytes. All three are WASM calls; we reconstruct the
    //    sender's Identity from the in-memory export key.
    const wasm = await loadWasm();
    const senderIdentity = wasm.Identity.fromSeed(identity.exportKey);
    const senderSigningSeed = senderIdentity.ed25519Seed;

    // The recipient's mailbox is keyed by their public-pubkey
    // pseudonym — anyone holding the recipient's Ed25519
    // identity pubkey can compute the same 32-byte value, by
    // design. This is the routing address for cold contact
    // requests (see ADR-0005 §13.7).
    const recipientPseudonym = wasm.IdentityPseudonym.fromIdentity(
      recipientSignaturePub,
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
    let wireBytes: Uint8Array;
    try {
      wireBytes = wasm.ContactRequest.compose(
        senderSigningSeed,
        recipientX25519Pub,
        senderPseudonym,
        validAfterMs,
        introMessage,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 4. Publish to the DS.
    let publish;
    try {
      publish = await this.ds.publishMailbox(recipientPseudonym, wireBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    return {
      recipientUsername: lookup.normalisedUsername,
      recipientAccountId,
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
    if (identity === null) {
      throw ContactsError.noIdentitySecrets();
    }

    const wasm = await loadWasm();
    const myIdentity = wasm.Identity.fromSeed(identity.exportKey);
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

    const requests: IncomingContactRequest[] = [];
    let cursor = after;
    for (const message of raw.messages) {
      cursor = message.id;
      let parsed;
      try {
        parsed = wasm.ContactRequest.decodeAndVerify(message.payload);
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

      requests.push({
        messageId: message.id,
        fromPubkey: parsed.fromPubkey,
        fromPseudonym: parsed.fromPseudonym,
        validAfterMs: parsed.validAfterMs,
        intro,
        createdAt: message.createdAt,
      });
    }

    return { requests, cursor, hasMore: raw.hasMore };
  }
}
