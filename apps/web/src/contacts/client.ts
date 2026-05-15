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
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
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
   * Falls back to a one-shot `Identity.fromSeed(secrets.exportKey)`
   * when the canonical isn't wired (tests, direct callers). MLS
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
    return wasm.Identity.fromSeed(secrets.exportKey);
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
