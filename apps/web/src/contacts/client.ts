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
import type { Wasm } from "../wasm";
import { DiscoveryClient, DiscoveryNetworkError } from "../discovery/client";
import { base64Decode, base64Encode } from "../auth/account-id";
import {
  appendGroupInviteLinkSecret,
  composeInlineGroupInviteUrl,
  composePointerGroupInviteUrl,
  parseGroupInviteSlug,
  splitGroupInviteLink,
} from "./group-invite";
import { inviteLinkWidths } from "./invite-link";
import { DsClient } from "../ds/client";
import { withGroupMlsLock } from "../mls/group-lock";
import type { IdentitySecrets } from "../auth/AuthContext";
import type {
  GroupInviteMint as WasmGroupInviteMint,
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import type { IdentityFetchResponse } from "../discovery/wire";
import type { ReceivedProfile } from "../profile/profile-envelope";
import type { AccountPubkeyPin } from "./local-store";
import {
  readPin,
  setPin,
  recordLogStatus,
  recordLogReset,
  recordLogVersion,
} from "./local-store";
import {
  verifyIdentityInLog,
  verifyRotationChain,
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
import {
  deleteEntry,
  loadUnacked,
  markAcked,
  markWelcomesOwed,
} from "../chat/commit-outbox";
import type {
  NewCommitEntry,
  PendingCommitEntry,
} from "../chat/commit-outbox";
import {
  resolveOwedVerdict,
  resolveOwedWelcomes,
  runCommitterSweep,
  type SweepCursorRef,
  type SweepDurableEntry,
  type SweepOutcome,
  type SweepPorts,
  type StagedSweepArtifacts,
} from "../chat/sweep";
import type { ActingCommitter } from "../chat/committer-beacon";
// ADR-0045 — the ONE dispatch ladder that turns a processed MLS message into a
// timeline event. The sweep's verdict apply consumes journal rows the poll will
// never fetch again, so it has to build events for them, and it has to build
// them the same way the poll does.
import { rosterOf, toEvent } from "../chat/client";
import { scanJournal } from "../chat/journal-head-epoch";
import {
  healAfterApply,
  knockToJoin,
  registerBeforePublish,
  takeSigner,
  type ProofSigner,
} from "../ds/access";
import { resolveLeafSignatureKey, resolveMemberIdentity } from "../chat/author-identity";
import type { IncomingChatEvent } from "../chat/client";
// ADR-0045 UX §6 — the seventh refusal string, defined ONCE beside the rest of
// the §6.1 copy. Both C-65 directions reuse it verbatim (a pending role change
// refuses a transfer, a pending transfer refuses a role change), so a single
// definition is what keeps the table at seven strings rather than drifting into
// two near-identical ones.
import { ROLE_CHANGE_ALREADY_PENDING_KEY } from "../chat/ownership-transfer";
import { KeyedError } from "../l10n/keyed-error";

/** Optional wires the route layer hands to ContactsClient
 *  (Phase 1.16e6c2). See `ChatClientOptions` in
 *  `chat/client.ts` for the rationale — same shape, same
 *  semantics, applied to a different surface. */
/** What a group invite link's sealed preview says (ADR-0028 v2): the group's
 *  name and the inviter's own display name as the device knows them at mint
 *  time (blank / absent = none), and the roster size. */
export interface GroupInviteMintPreview {
  readonly groupName?: string | undefined;
  readonly inviterName?: string | undefined;
  readonly memberCount: number;
}

export interface ContactsClientOptions {
  /** Returns the **canonical** `WasmIdentity` for this session.
   *  See `<ChatProvider>` (1.16e6c1). When unset, the client
   *  falls back to building a throwaway `Identity.fromSeed` per
   *  call — only acceptable for tests; production paths MUST
   *  wire this so MLS state stays coherent across calls. */
  identityProvider?: () => WasmIdentity | null;
  /** ADR-0050 — the ACCOUNT identity (phrase-seeded), for everything that
   *  is about the person rather than a leaf: signing contact requests and
   *  acks, opening sealed envelopes, minting invite capabilities, the
   *  account mailbox. When unset, falls back to `identityProvider`, then to
   *  a throwaway `Identity.fromSeed(accountIdentitySeed)`. */
  accountIdentityProvider?: () => WasmIdentity | null;
  /** Fires after every successful MLS state-mutating call so
   *  the route layer can persist the canonical Identity's
   *  provider snapshot to disk. Best-effort — exceptions are
   *  logged and swallowed; the in-memory state stays good. */
  onPersist?: () => Promise<void>;
  /** ADR-0040 (G-35) — the FAIL-CLOSED persist used by the four
   *  group-membership commit paths BEFORE they publish: persist a durable
   *  pending-commit outbox entry (the commit + Welcome / rendezvous wire-bytes)
   *  AND the N+1 MLS snapshot in ONE atomic transaction. A throw MUST abort the
   *  commit op (no publish). Wired to `ChatApi.persistSnapshotWithCommit`. When
   *  unset (tests / standalone callers with no reload-from-IDB path) the commit
   *  paths treat the persist as a no-op — such callers persist nothing, so a
   *  reload restores no group and cannot desync: they are outside this bug's
   *  blast radius, mirroring `ChatClient.persistDurableOrThrow`. */
  onPersistWithCommit?: (entry: NewCommitEntry) => Promise<void>;
  /** ADR-0040 F1 (lakko-security) — reset the in-memory group back to the last
   *  durable snapshot after a FAIL-CLOSED commit persist-abort. The core
   *  create-commit eager-merged epoch N -> N+1 in the shared provider BEFORE the
   *  persist; when the persist throws the op aborts (no publish) but the
   *  in-memory group is left at the un-published N+1. Wired to
   *  `ChatApi.resetGroupToDurableSnapshot` (the route layer owns the group cache,
   *  so the reload/replace is delegated). A throw here is a fail-safe HARD error
   *  (the reset itself failed — the group may still be poisoned; do not proceed
   *  as if the abort were clean). When unset (tests / standalone callers with no
   *  group cache) it is a no-op — such callers persist nothing and hold no cached
   *  group, so they cannot desync, mirroring `onPersistWithCommit`. */
  onCommitPersistAbort?: (groupIdB64: string) => Promise<void>;
  /** ADR-0048 §2 — this account's composed profile envelope, carried inside
   *  every outgoing contact request's sealed plaintext so the recipient can
   *  see who is asking before accepting. `null` (or unset) sends none. A throw
   *  is treated as `null`: a profile is never a reason not to send a
   *  request. */
  ownProfileEnvelope?: () => Promise<Uint8Array | null>;
  /** ADR-0048 §3 — a username lookup this client ran resolved `username` to
   *  the account publishing `signatureKeyB64`. That is exactly the binding a
   *  username verification checks, so the caller may record it instead of
   *  spending another lookup later. It shows nothing by itself. */
  onUsernameResolved?: (signatureKeyB64: string, username: string) => void;
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
  /** ADR-0048 — the sender's profile from the sealed plaintext, or `null`
   *  when the request carried none, carried one out of bounds, or did not
   *  decrypt. The sender's CLAIM, bound to `fromPubkey` (which the request
   *  signature authenticates): file it under that key and verify the
   *  username before showing it. */
  profile: ReceivedProfile | null;
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
  /** ADR-0053 — the Welcome arrived in the ACCOUNT mailbox rather than the
   *  device mailbox. Since ADR-0050 a conversation Welcome is addressed to a
   *  device leaf, so the one Welcome still addressed to the account key is a
   *  channel co-publisher's (the PublisherGroup lives in the account
   *  identity's provider), which the co-publisher flow opens itself. The
   *  mailbox handler, whose join uses the device identity, cannot open it and
   *  must not report that as a failed join. */
  fromAccountMailbox?: boolean;
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
/** ADR-0050 — every leaf index in `group` whose account is the account of
 *  `leafIndex`, the named leaf first. Falls back to `[leafIndex]` when the
 *  roster cannot be read or the leaf is not in it (the core then refuses). */
/** The leaf that holds `signatureKey` in `group`, or `null`. A torn roster
 *  read answers `null` too: the callers treat that as "not admitted yet" and a
 *  duplicate Add is refused by the core, never merged. */
function leafIndexOfSignatureKey(group: WasmGroup, signatureKey: Uint8Array): number | null {
  let roster: Array<{ leafIndex: number; signaturePublicKey: Uint8Array }>;
  try {
    roster = group.members() as Array<{ leafIndex: number; signaturePublicKey: Uint8Array }>;
  } catch {
    return null;
  }
  const hit = roster.find((m) => bytesEqualCc(m.signaturePublicKey, signatureKey));
  return hit === undefined ? null : hit.leafIndex;
}

function leavesOfSameAccount(group: WasmGroup, leafIndex: number): number[] {
  let roster: Array<{ leafIndex: number; signaturePublicKey: Uint8Array; accountKey?: Uint8Array }>;
  try {
    roster = group.members() as Array<{
      leafIndex: number;
      signaturePublicKey: Uint8Array;
      accountKey?: Uint8Array;
    }>;
  } catch {
    return [leafIndex];
  }
  const named = roster.find((m) => m.leafIndex === leafIndex);
  if (named === undefined) return [leafIndex];
  const person = (m: (typeof roster)[number]): string =>
    base64Encode(
      m.accountKey !== undefined && m.accountKey.length === 32
        ? m.accountKey
        : m.signaturePublicKey,
    );
  const account = person(named);
  const rest = roster
    .filter((m) => m.leafIndex !== leafIndex && person(m) === account)
    .map((m) => m.leafIndex);
  return [leafIndex, ...rest];
}

/** Constant-shape byte equality for two public keys. */
function bytesEqualCc(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a[i]! ^ b[i]!;
  return diff === 0;
}

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

/** ADR-0040 (G-35) — a fresh random id (16 bytes hex) for one outbox entry.
 *  Built from `crypto.getRandomValues` (present in browsers + the vitest node
 *  env) rather than `crypto.randomUUID` for maximal runtime compatibility. */
function newOutboxEntryId(): string {
  const b = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

/**
 * ADR-0045 — what a membership action did.
 *
 * The two arms are not two flavours of success, they are two different things
 * that happened, and the UI has to say so: a `committed` action took effect
 * across the group, a `proposed` one is a request that lands when the committer
 * sweeps. Collapsing them into one shape would force the panel to assert an
 * effect that has not happened, which is exactly the optimistic-update lie the
 * UX spec forbids.
 */
export type MembershipOutcome =
  /** A direct commit landed. Only reachable in a group with NO role table,
   *  where ADR-0045's proposal path does not apply — see
   *  {@link ContactsClient.usesProposalPath}. */
  | {
      readonly kind: "committed";
      readonly commitMessageId: number;
      readonly welcomeMessageId: number | undefined;
    }
  /** A proposal was published. It takes effect when a committer sweeps it. */
  | { readonly kind: "proposed"; readonly proposalMessageId: number };

/**
 * ADR-0045 — does this group route membership through proposals?
 *
 * A group with **no role table** keeps direct commits, unchanged: it has no
 * owner, therefore no rank 0, therefore no committer, and every peer's receipt
 * gate drops proposals in such a group — so the propose path would silently
 * discard everything. ADR-0045 says this explicitly, and `Group::create` has
 * seeded a role table since ADR-0022, so no freshly created group reaches it.
 *
 * This is NOT a dual path in ADR-0045's forbidden sense. The ADR rules out a
 * "legacy direct-commit fallback" — a second, ungated route into a group that
 * *has* moved to proposals. This is the disjoint case: a group with no role
 * table has no gate to bypass, because there is no table to read a role from.
 *
 * A torn read is treated as "no role table", which keeps the direct path
 * working rather than routing into a proposal path whose gate cannot resolve.
 */
function usesProposalPath(group: WasmGroup): boolean {
  try {
    return group.roleTableOwner() !== undefined;
  } catch {
    return false;
  }
}

/**
 * Adapt a wasm `Group` + this client's DS / outbox wiring onto the
 * {@link SweepPorts} the ADR-0045 sweep orchestration runs over.
 *
 * The adapter is deliberately thin and does no deciding: every rule lives in
 * `chat/sweep.ts`, where it is host-testable without wasm, IndexedDB or a
 * network.
 */
function sweepPortsFor(
  wasm: Wasm,
  identity: WasmIdentity,
  group: WasmGroup,
  cursor: SweepCursorRef,
  markOwnPublished: (messageId: number) => void,
  ds: DsClient,
  onPersistWithCommit: ((entry: NewCommitEntry) => Promise<void>) | undefined,
  persist: () => Promise<void>,
  persistApplied: () => Promise<void>,
  actingCommitter: (ownRank: number) => Promise<ActingCommitter>,
  onSweptEvent: (event: IncomingChatEvent) => void,
): SweepPorts {
  const groupIdBytes = group.groupId;
  const groupIdB64 = base64Encode(groupIdBytes);
  return {
    actingCommitter,
    group: {
      groupId: groupIdBytes,
      get epoch(): bigint {
        return group.epoch;
      },
      ownCommitterRank: () => group.ownCommitterRank(identity),
      pendingProposalCount: () => group.pendingProposals().length,
      hasStagedSweep: () => group.hasStagedSweep(),
      stagePendingProposals: (): StagedSweepArtifacts | null => {
        const staged = group.stagePendingProposals(identity);
        if (staged === undefined) return null;
        // `swept()` reports what the commit REALLY carries, read back off the
        // staged commit by the core rather than predicted by the caller — which
        // matters here because it is what addresses the Welcome.
        const added: Uint8Array[] = [];
        for (const raw of staged.swept()) {
          const record = raw as { kind: string; addSignatureKey?: Uint8Array };
          if (record.kind === "add" && record.addSignatureKey !== undefined) {
            added.push(record.addSignatureKey);
          }
        }
        return {
          commit: staged.commit,
          welcome: staged.welcome,
          addedSignatureKeys: added,
        };
      },
      mergeStagedSweep: () => {
        group.mergeStagedSweep(identity);
      },
      abandonStagedSweep: () => {
        group.abandonStagedSweep(identity);
      },
      // ADR-0045 — apply AND surface. The verdict consumes every row the DS
      // ordered below our own commit and the shared cursor then advances past
      // them, so a row applied here is a row the poll will never fetch again:
      // discarding what it decrypted loses an ordinary chat message from the
      // timeline permanently and silently. Routed through the SAME `toEvent`
      // the poll uses, so every envelope kind is dispatched by one ladder.
      applyIncoming: async (row) => {
        // The MLS apply is what the verdict reads, so it is deliberately OUTSIDE
        // the guard below: a throw here must reach `adjudicate`, which skips the
        // row exactly as the poll loop does.
        const processed = group.processIncomingVerifyingRoles(identity, row.payload);
        try {
          // The roster is read HERE, one line after the apply, because that
          // is the epoch this row decrypted at — a leaf resolved any later
          // can name whoever was added into a vacated slot since.
          onSweptEvent(
            await toEvent(
              row.id,
              row.createdAt,
              processed,
              (leaf) => resolveMemberIdentity(rosterOf(group), leaf),
              (leaf) => resolveLeafSignatureKey(rosterOf(group), leaf),
            ),
          );
        } catch (err) {
          // The ratchet has already moved and the row is consumed, so an event
          // that could not be built costs one timeline row — never the verdict,
          // which is why this is not allowed to look like a failed apply.
          // eslint-disable-next-line no-console
          console.warn("ContactsClient: swept row not surfaced", row.id, err);
        }
      },
    },
    journal: {
      fetch: async (after) => {
        const res = await ds.fetchGroup(groupIdBytes, { after, waitSecs: 0 });
        return res.messages.map((m) => ({
          id: m.id,
          payload: m.payload,
          // ADR-0045 — carried so an applied row can become a timeline event.
          createdAt: m.createdAt,
        }));
      },
      publishGroup: async (bytes) => (await ds.publishGroup(groupIdBytes, bytes)).id,
      publishMailbox: async (pseudonym, bytes) =>
        (await ds.publishMailbox(pseudonym, bytes)).id,
    },
    outbox: {
      newEntryId: newOutboxEntryId,
      record: async (entry) => {
        if (onPersistWithCommit === undefined) return;
        const next: NewCommitEntry = {
          id: entry.id,
          groupId: groupIdB64,
          // The epoch the sweep was BUILT AGAINST: a staged sweep has not
          // merged, so unlike every direct-path entry this is N, not N+1. It is
          // diagnostics only — `sweepVerdictPending` is what the restart sweep
          // branches on.
          targetEpoch: group.epoch.toString(),
          commitB64: entry.commitB64,
          sweepVerdictPending: true,
          welcomeMailboxesB64: [...entry.welcomeMailboxesB64],
        };
        if (entry.welcomeEnvelopeB64 !== undefined) {
          next.secondArtifactB64 = entry.welcomeEnvelopeB64;
          next.secondArtifactKind = "welcome";
        }
        await onPersistWithCommit(next);
      },
      findOwed: async () => {
        // A welcomes-owed entry has already reached a verdict, so it is not
        // this finder's business — re-adjudicating it would re-publish a commit
        // whose order was already read.
        const owed = (await loadUnacked(groupIdB64)).find(
          (e) => e.sweepVerdictPending === true && e.sweepWelcomesOwed !== true,
        );
        if (owed === undefined) return null;
        return toSweepEntry(owed);
      },
      markWelcomesOwed: async (id, commitMessageId) => {
        // Deliberately NOT routed through `onPersistWithCommit`: that co-writes
        // the MLS snapshot, which `persist()` has already done one line earlier
        // in the sweep. This is outbox bookkeeping only.
        await markWelcomesOwed(id, commitMessageId);
      },
      findOwedWelcomes: async () => {
        const owed = (await loadUnacked(groupIdB64)).find(
          (e) => e.sweepWelcomesOwed === true,
        );
        if (owed === undefined) return null;
        return toSweepEntry(owed);
      },
      ack: async (id, commitMessageId, welcomeMessageId) => {
        try {
          await markAcked(id, commitMessageId, welcomeMessageId);
        } catch (err) {
          // An un-acked entry whose verdict already landed is re-adjudicated by
          // the next restart sweep, which finds nothing staged and drops it.
          // eslint-disable-next-line no-console
          console.warn("ContactsClient: sweep outbox ack failed", id, err);
        }
      },
      discard: async (id) => {
        try {
          await deleteEntry(id);
        } catch (err) {
          // A surviving entry is re-adjudicated by the next restart sweep, which
          // will find nothing staged and simply drop it — so a failure here is
          // untidy rather than unsafe.
          // eslint-disable-next-line no-console
          console.warn("ContactsClient: sweep outbox discard failed", id, err);
        }
      },
    },
    cursor,
    welcomeMailbox: (signatureKey) =>
      wasm.IdentityPseudonym.fromIdentity(signatureKey).asBytes(),
    wrapWelcome: (welcome) => wrapEnvelope(KIND_WELCOME, welcome),
    // ADR-0053 contract point 3 — the staged epoch's key, proven under the
    // current one, registered before the sweep is published.
    registerStagedEpoch: async () => {
      const signerN = takeSigner(() => group.dsAccessSigner(identity));
      const staged = takeSigner(() => {
        const next = group.dsStagedAccessSigner(identity);
        if (next === undefined) throw new Error("no staged sweep");
        return next;
      });
      try {
        await registerBeforePublish(ds, groupIdBytes, signerN, staged);
      } finally {
        signerN?.free();
        staged?.free();
      }
    },
    // ADR-0053 contract point 4 — heal after the verdict apply.
    beginApply: () => {
      const before = takeSigner(() => group.dsAccessSigner(identity));
      return async (heal: boolean) => {
        if (!heal) {
          before?.free();
          return;
        }
        const after = takeSigner(() => group.dsAccessSigner(identity));
        try {
          await healAfterApply(ds, groupIdBytes, before, after);
        } finally {
          before?.free();
          after?.free();
        }
      };
    },
    markOwnPublished,
    persist,
    persistApplied,
    encodeB64: base64Encode,
    decodeB64: base64Decode,
  };
}

/**
 * ADR-0045 — is a role-change proposal already waiting to be swept?
 *
 * A torn read is treated as "no", which lets the request through to the core's
 * own authorisation rather than refusing on a transient store failure — the
 * core is the enforcement either way, and this check exists only to convert a
 * silent loss into a clear message.
 */
function hasPendingRoleChange(group: WasmGroup): boolean {
  try {
    return (group.pendingProposals() as Array<{ kind: string }>).some(
      (p) => p.kind === "role-change",
    );
  } catch {
    return false;
  }
}

/** Project a stored outbox row onto the sweep's own entry shape. */
function toSweepEntry(row: PendingCommitEntry): SweepDurableEntry {
  const entry: SweepDurableEntry = {
    id: row.id,
    commitB64: row.commitB64,
    welcomeEnvelopeB64: row.secondArtifactB64,
    welcomeMailboxesB64: row.welcomeMailboxesB64 ?? [],
  };
  // Built without assigning `undefined` (exactOptionalPropertyTypes).
  if (row.commitMessageId !== undefined) {
    return { ...entry, commitMessageId: row.commitMessageId };
  }
  return entry;
}

export class ContactsClient {
  private readonly identityProvider: (() => WasmIdentity | null) | undefined;
  private readonly accountIdentityProvider: (() => WasmIdentity | null) | undefined;
  private readonly onPersist: (() => Promise<void>) | undefined;
  private readonly onPersistWithCommit:
    | ((entry: NewCommitEntry) => Promise<void>)
    | undefined;
  private readonly onCommitPersistAbort:
    | ((groupIdB64: string) => Promise<void>)
    | undefined;
  private readonly ownProfileEnvelope: (() => Promise<Uint8Array | null>) | undefined;
  private readonly onUsernameResolved:
    | ((signatureKeyB64: string, username: string) => void)
    | undefined;

  constructor(
    private readonly discovery: DiscoveryClient = new DiscoveryClient(),
    private readonly ds: DsClient = new DsClient(),
    options: ContactsClientOptions = {},
  ) {
    this.identityProvider = options.identityProvider;
    this.accountIdentityProvider = options.accountIdentityProvider;
    this.onPersist = options.onPersist;
    this.onPersistWithCommit = options.onPersistWithCommit;
    this.onCommitPersistAbort = options.onCommitPersistAbort;
    this.ownProfileEnvelope = options.ownProfileEnvelope;
    this.onUsernameResolved = options.onUsernameResolved;
  }

  /** ADR-0048 §3 — report a lookup's binding to the verification cache.
   *  Best-effort: a throwing listener never fails the lookup's own flow. */
  private reportUsernameResolved(signatureKey: Uint8Array, username: string): void {
    try {
      this.onUsernameResolved?.(base64Encode(signatureKey), username);
    } catch {
      // Diagnostic cache only.
    }
  }

  /** ADR-0048 §2 — the profile envelope to seal into a contact request, or an
   *  empty array for none. */
  private async profileEnvelopeForRequest(): Promise<Uint8Array> {
    if (this.ownProfileEnvelope === undefined) return new Uint8Array(0);
    try {
      return (await this.ownProfileEnvelope()) ?? new Uint8Array(0);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ContactsClient: own profile envelope unavailable; sending the request without it", err);
      return new Uint8Array(0);
    }
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

  /** ADR-0050 — the ACCOUNT identity for person-level operations (see
   *  `ContactsClientOptions.accountIdentityProvider`). */
  private async resolveAccountIdentity(
    secrets: IdentitySecrets | null,
  ): Promise<WasmIdentity> {
    if (this.accountIdentityProvider !== undefined) {
      const account = this.accountIdentityProvider();
      if (account !== null) return account;
      throw ContactsError.noIdentitySecrets();
    }
    // Never fall back to `resolveIdentity`: since ADR-0050 the canonical
    // identity is this DEVICE's leaf identity, and a contact request signed
    // with it names the device key as `from_pubkey`, so the acceptor routes
    // its ContactAck to the device mailbox, where only Welcomes are read.
    // Seen live on 2026-09-16 (web request, Android accept, no contact on
    // web). The account identity is always derivable from the phrase seed.
    if (secrets === null) throw ContactsError.noIdentitySecrets();
    const wasm = await loadWasm();
    return wasm.Identity.fromSeed(secrets.accountIdentitySeed);
  }

  /** ADR-0050 — this account's OTHER devices' KeyPackages, so a group this
   *  device creates includes the person's other devices from the first
   *  epoch. Best-effort: with no secrets (no account id) or a directory
   *  failure it is an empty list, and the ADR-0050 re-admission arm adds
   *  the missing devices later. */
  private async ownOtherDevices(
    secrets: IdentitySecrets | null,
    myIdentity: WasmIdentity,
  ): Promise<readonly { keyPackage: Uint8Array }[]> {
    if (secrets === null) return [];
    const ownKey = myIdentity.signaturePublicKey;
    try {
      const list = await this.discovery.fetchKeyPackages(secrets.accountId);
      if (list === null) return [];
      return list.filter((d) => !bytesEqualCc(d.deviceId, ownKey));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("own devices lookup failed; adding this device only", err);
      return [];
    }
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
   * Fire the persist hook and RE-THROW on failure — the fail-closed variant the
   * ADR-0045 sweep's verdict apply needs.
   *
   * That path applies journal rows and advances the SAME cursor the poll drives
   * over them, so a swallowed failure there would leave the cursor ahead of the
   * durable MLS state, which since ADR-0045 strands the device permanently. See
   * `chat/durable-cursor.ts`. A no-op resolve when `onPersist` is unset, for
   * the same reason `persistDurableOrThrow` is: such callers persist nothing
   * and reload nothing.
   */
  private async persistDurableOrThrow(): Promise<void> {
    if (this.onPersist === undefined) return;
    await this.onPersist();
  }

  /**
   * ADR-0040 (G-35) — the FAIL-CLOSED persist a commit path runs BEFORE any
   * publish. Durably records the pending-commit outbox entry (commit + Welcome /
   * rendezvous bytes) AND the N+1 MLS snapshot in one atomic write; a throw
   * propagates so the caller ABORTS the publish (no half-applied membership
   * change leaves the device).
   *
   * A no-op resolve when `onPersistWithCommit` is unset — the degenerate
   * test / standalone path (no reload-from-IDB), outside the desync blast radius,
   * mirroring `ChatClient.persistDurableOrThrow`.
   */
  private async persistCommitOrThrow(entry: NewCommitEntry): Promise<void> {
    if (this.onPersistWithCommit === undefined) return;
    await this.onPersistWithCommit(entry);
  }

  /**
   * ADR-0040 F1 (lakko-security) — RESET the in-memory group back to the last
   * durable snapshot (epoch N) after a FAIL-CLOSED commit persist-abort, BEFORE
   * the abort re-throws.
   *
   * The core create-commit eager-merged N -> N+1 in the shared in-memory provider
   * BEFORE `persistCommitOrThrow`. When that atomic persist THROWS the op aborts
   * (no publish) and — because the write is atomic — the durable snapshot stays at
   * N with NO outbox entry. But the in-memory group (and the shared provider) is
   * left at the un-published N+1. Left alone, a SECOND same-session commit would
   * build on N+1 and publish commit(N+1 -> N+2) while peers are still at N (they
   * never saw an N -> N+1 commit) — they drop it as wrong-epoch and the WHOLE
   * group is permanently stranded ("device-ahead", worse than a single-device
   * brick). The outbox sweep cannot catch this: a clean atomic abort wrote no
   * entry, so there is nothing to re-publish. So the abort must be "as if the op
   * never happened" IN MEMORY too — reload the group from the durable N snapshot
   * and replace the cached reference, dropping the un-published eager merge.
   *
   * The route layer owns the group cache, so the reload/replace is delegated
   * through `onCommitPersistAbort` (-> `ChatApi.resetGroupToDurableSnapshot`).
   *
   * FAIL-SAFE: if the reset itself fails (no durable snapshot for an existing
   * group — should be impossible on these four created-and-persisted paths), the
   * in-memory group is STILL poisoned at N+1, which is worse than a clean abort;
   * surface a HARD, distinct error rather than swallow it. No-op when
   * `onCommitPersistAbort` is unset (tests / standalone callers hold no cache).
   */
  private async resetGroupAfterPersistAbort(group: WasmGroup): Promise<void> {
    if (this.onCommitPersistAbort === undefined) return;
    // Read group_id BEFORE the reset — it is epoch-invariant and the old (N+1)
    // group object is not freed by the reload, so this stays valid.
    const groupIdB64 = base64Encode(group.groupId);
    try {
      await this.onCommitPersistAbort(groupIdB64);
    } catch (resetErr) {
      throw ContactsError.wrap(
        new Error(
          "ADR-0040 F1: in-memory group reset FAILED after a commit persist-abort; " +
            "the group may be desynced ahead at an un-published N+1 — reload the app " +
            `to recover. Cause: ${String(resetErr)}`,
        ),
      );
    }
  }

  /** ADR-0040 (G-35) — mark an outbox entry acked after BOTH artifacts have
   *  published. Best-effort: a failure here just leaves an acked-worthy entry
   *  un-acked, so the next restart sweep re-publishes it idempotently (peers
   *  drop the wrong-epoch duplicate; the committer's echo filter is re-armed). */
  /**
   * ADR-0053 contract point 3 — register the key of the epoch a direct commit
   * just moved this group to, proven under `signerN` (the key of the epoch it
   * left, taken BEFORE the create call). Called inside each commit path's
   * fail-closed region, before the ADR-0040 outbox write: a refusal throws, and
   * the caller resets the group to N exactly as for a failed outbox write, so a
   * commit is never on the wire before the server knows its epoch.
   */
  private async registerNextEpochOrThrow(
    group: WasmGroup,
    myIdentity: WasmIdentity,
    signerN: ProofSigner | null,
  ): Promise<void> {
    const signerN1 = takeSigner(() => group.dsAccessSigner(myIdentity));
    try {
      await registerBeforePublish(this.ds, group.groupId, signerN, signerN1);
    } finally {
      signerN1?.free();
    }
  }

  private async ackCommitBestEffort(
    id: string,
    commitMessageId?: number,
    secondMessageId?: number,
  ): Promise<void> {
    try {
      await markAcked(id, commitMessageId, secondMessageId);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ContactsClient: commit-outbox ack failed", err);
    }
  }

  /**
   * ADR-0040 (G-35) — restart / group-open sweep. Re-publishes every un-acked
   * pending-commit outbox entry for `group` IDEMPOTENTLY, re-arming the
   * committer's own-echo filter (`markOwnPublished`) for the re-published commit.
   *
   * Correctness: the entry + N+1 snapshot were written in ONE atomic
   * transaction, so an un-acked entry ALWAYS means the device is durably at N+1 —
   * re-publish is unconditionally safe (no torn-write / epoch reconciliation).
   *   - A crash "after step-2 persist, before publish" -> the commit never
   *     reached the DS; the re-publish is the FIRST publish -> peers catch up.
   *   - A crash "after publish, before ack" -> the re-publish is a DUPLICATE;
   *     peers already at N+1 drop it as a wrong-epoch (past) commit, and the
   *     committer's own poll drops it via the re-armed echo filter (else it would
   *     hit `CannotDecryptOwnMessage`).
   * A re-publish failure (offline DS) leaves the entry un-acked for the next
   * sweep. MUST run BEFORE the first `pollMessages` so the re-armed messageId is
   * in place when the poll re-fetches the commit.
   */
  /**
   * ADR-0045 S4a/S4b — run one committer sweep for `group`, if this device is
   * the acting committer.
   *
   * The committer set is the ADR-0022 owner plus admins, ordered by
   * `ownCommitterRank()`; which of them ACTS is decided by `actingCommitter`,
   * the caller-supplied fold over the observed liveness beacons. Rank 0 — the
   * owner — always acts, because no rank is lower; an admin acts when nobody
   * strictly better placed has beaconed inside the freshness window, which
   * includes the case where it has heard nothing at all. So an admin-only
   * group no longer waits for an absent owner.
   *
   * Short-circuits synchronously — before touching the network — when this
   * device is not a candidate, or when nothing is pending and no verdict is
   * owed. `actingCommitter` is consulted only once there IS something to
   * sweep, so a quiet group never pays for the fold. That is what makes this
   * safe to call at the tail of every poll.
   *
   * Runs under the same per-group MLS lock the commit and poll regions take.
   * The whole region (stage -> publish -> verdict -> merge/abandon) is inside
   * it, network round-trips included, because the verdict is a read-modify-write
   * over both the group epoch and the journal cursor; a poll interleaving in
   * the middle would not merely slow it down, it would make the verdict wrong.
   *
   * @param cursor  the SAME journal cursor `pollMessages` drives. The verdict
   *   applies the rows below its own commit, so it must advance the cursor over
   *   them or the poll would re-feed consumed rows to OpenMLS.
   * @param onSweptEvent  called, in journal order, with the timeline event of
   *   every row the verdict applied. REQUIRED rather than optional: the cursor
   *   moves past those rows, so a caller that has nowhere to put them is a
   *   caller that loses them, and that must be a visible decision at the call
   *   site rather than a defaulted no-op.
   */
  async sweepIfCommitter(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    cursor: SweepCursorRef,
    markOwnPublished: (messageId: number) => void,
    actingCommitter: (ownRank: number) => Promise<ActingCommitter>,
    onSweptEvent: (event: IncomingChatEvent) => void,
  ): Promise<SweepOutcome> {
    // A group with no role table has no committer to be, and the core refuses
    // a sweep on it outright. Short-circuit before resolving an identity or
    // loading wasm, so the poll tail costs nothing at all on a legacy group.
    if (!usesProposalPath(group)) {
      return { kind: "not-committer", rank: undefined };
    }
    const myIdentity = await this.resolveIdentity(identity);
    const wasm = await loadWasm();
    return withGroupMlsLock(base64Encode(group.groupId), async () =>
      runCommitterSweep(
        sweepPortsFor(
          wasm,
          myIdentity,
          group,
          cursor,
          markOwnPublished,
          this.ds,
          this.onPersistWithCommit,
          () => this.persistBestEffort(),
          () => this.persistDurableOrThrow(),
          actingCommitter,
          onSweptEvent,
        ),
      ),
    );
  }

  /**
   * @param onSweptEvent  ADR-0045 — as on {@link sweepIfCommitter}: the timeline
   *   events of every row an OWED verdict applied. Optional only because the
   *   whole owed-verdict branch is skipped when no `cursor` is supplied; when a
   *   cursor IS supplied, rows are consumed and dropping their events loses
   *   messages, so a caller with a timeline must pass this.
   */
  async sweepPendingCommits(
    group: WasmGroup,
    markOwnPublished: (messageId: number) => void,
    identity?: IdentitySecrets | null,
    cursor?: SweepCursorRef,
    onSweptEvent?: (event: IncomingChatEvent) => void,
  ): Promise<void> {
    // ADR-0045 obligation 1 — a staged sweep is DURABLE and freezes this
    // group's own membership operations across restarts, so a verdict owed
    // from a previous run must be finished before anything else. It is wired
    // into this existing restart seam rather than into a second one, because
    // two restart paths over the same durable state is how they drift.
    //
    // It runs FIRST: the legacy re-publish loop below would otherwise
    // re-publish the sweep's own commit as if it were a direct-path entry and
    // — worse — re-publish its Welcome, which ADR-0045 forbids before the
    // verdict.
    if (cursor !== undefined) {
      const owedPorts = async (): Promise<SweepPorts> =>
        sweepPortsFor(
          await loadWasm(),
          await this.resolveIdentity(identity ?? null),
          group,
          cursor,
          markOwnPublished,
          this.ds,
          this.onPersistWithCommit,
          () => this.persistBestEffort(),
          () => this.persistDurableOrThrow(),
          // ADR-0045 S4b — never consulted on either owed path: an OWED
          // verdict is finished by whoever staged it, not by whoever is
          // currently acting, and `resolveOwedVerdict` therefore never asks. A
          // `defer` is the fail-closed value should that ever change: it would
          // stop a sweep from STARTING, which is recoverable, rather than let
          // one start on no evidence, which is the hazard.
          () => Promise.resolve({ kind: "defer", toRank: undefined }),
          onSweptEvent ?? (() => {}),
        );

      try {
        const ports = await owedPorts();
        await withGroupMlsLock(base64Encode(group.groupId), async () =>
          resolveOwedVerdict(ports),
        );
      } catch (err) {
        // The journal was unreadable, so the verdict stays owed and the sweep
        // stays staged for the next attempt (which runs at the tail of every
        // poll). Membership on this group is frozen until then — a temporary
        // freeze that self-heals, which is strictly better than merging or
        // abandoning on a verdict nobody read.
        // eslint-disable-next-line no-console
        console.warn("ContactsClient: owed sweep verdict unresolved", err);
      }

      // A sweep whose verdict SETTLED but whose Welcome never reached the wire
      // owes only the fan-out. It is invisible to every other recovery path —
      // the merge cleared `hasStagedSweep()` and emptied the pending set, and
      // the legacy loop below skips sweep entries — so without this the added
      // member stays in every peer's roster, addressed by every later message,
      // permanently unable to construct group state.
      //
      // It runs AFTER the owed verdict on purpose: a still-staged sweep owes a
      // VERDICT, and only once that has merged is a Welcome allowed on the wire
      // at all (ADR-0045 obligation 2). `resolveOwedWelcomes` refuses while
      // anything is staged, so the order here is belt to that braces.
      try {
        const ports = await owedPorts();
        await withGroupMlsLock(base64Encode(group.groupId), async () =>
          resolveOwedWelcomes(ports),
        );
      } catch (err) {
        // The DS is still unreachable. The durable marker survives, so the next
        // sweep — group open or poll tail — retries the fan-out.
        // eslint-disable-next-line no-console
        console.warn("ContactsClient: owed sweep Welcome unresolved", err);
      }
    }

    const groupIdBytes = group.groupId;
    const groupIdB64 = base64Encode(groupIdBytes);
    let entries: PendingCommitEntry[];
    try {
      entries = await loadUnacked(groupIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ContactsClient: commit-outbox sweep load failed", err);
      return;
    }
    for (const entry of entries) {
      // ADR-0045 — a staged-sweep entry is NOT re-publishable on this path.
      // Two reasons, and either alone is decisive: its device is durably at N,
      // not N+1, so the unconditional-re-publish argument this loop rests on
      // does not hold for it; and its Welcome must not reach the wire until the
      // verdict says the commit won. `resolveOwedVerdict` above owns it.
      if (entry.sweepVerdictPending === true) continue;
      try {
        const commitBytes = base64Decode(entry.commitB64);
        const commitPub = await this.ds.publishGroup(groupIdBytes, commitBytes);
        // Echo re-arm: the device is durably at N+1, so feeding its own
        // re-fetched commit into processIncoming would raise
        // CannotDecryptOwnMessage. Suppress it on the very next poll.
        markOwnPublished(commitPub.id);

        // ADR-0045 S7 — a direct-path second artifact is ALWAYS a mailbox
        // Welcome. The rendezvous arm this loop used to carry belonged to the
        // external-join approve, which no longer commits; and since ADR-0053
        // no Welcome goes to a group journal at all (contract point 8). The
        // commit re-publish below is proven under whatever access key the
        // group holds now: its epoch was registered before the entry was
        // written, so that key is one the server knows.
        let secondMessageId: number | undefined;
        if (
          entry.secondArtifactB64 !== undefined &&
          entry.secondArtifactMailboxB64 !== undefined
        ) {
          // Welcome -> the new member's mailbox (NOT this device's group
          // journal), so no echo re-arm — the committer never re-fetches it.
          const secondBytes = base64Decode(entry.secondArtifactB64);
          const pseudonym = base64Decode(entry.secondArtifactMailboxB64);
          const pub2 = await this.ds.publishMailbox(pseudonym, secondBytes);
          secondMessageId = pub2.id;
        }
        await this.ackCommitBestEffort(entry.id, commitPub.id, secondMessageId);
      } catch (err) {
        // Offline / DS down -> leave un-acked; the next sweep retries. Safe.
        // eslint-disable-next-line no-console
        console.warn(
          "ContactsClient: commit-outbox re-publish failed, will retry",
          entry.id,
          err,
        );
      }
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
    // G-30 #2 (ADR-0019): snapshot the pin BEFORE gate 1 runs. Gate 1
    // re-pins to the new key on a verified rotation (overwriting the pin),
    // but gate 2's rotation-chain check needs the OLD pinned key + the
    // OLD chain-version floor to anchor the chain. Capturing it here keeps
    // gate 1 untouched.
    const oldPin = readPin(base64Encode(accountId));

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
    // G-30 #2 (ADR-0019): pass the OLD pin + the bundle's supersession
    // proof so the log check can route a key ROTATION through the
    // chain verifier (anchoring the transition to the operator-signed
    // log) instead of the standalone single-key inclusion check.
    const accountIdB64 = base64Encode(accountId);
    const servedPubkey = base64Decode(bundle.signature_pubkey);
    await this.checkTransparencyLog(
      accountId,
      accountIdB64,
      servedPubkey,
      oldPin,
      bundle.supersession_proof,
    );
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
   * G-30 #2 (ADR-0019): when the served key is a ROTATION away from the
   * pinned one AND the bundle carried a supersession proof, the check
   * routes through `verifyRotationChain` (the chain verifier — root sig +
   * no-rollback + Merkle inclusion + the supersession proof from the OLD
   * pinned key + a strictly-newer logged version + freshness) instead of
   * the standalone single-key `verifyIdentityInLog`. The POSTURE is
   * unchanged: only `"rollback"` hard-fails; a chain contradiction
   * surfaces as `"mismatch"` (NOT a block); a not-logged / unreachable /
   * off rotation falls back to the standalone gate-1 result (no
   * over-block). On a `"confirmed"` chain the proven log version is
   * persisted as the next rotation's downgrade floor.
   *
   * @param accountId    the peer's raw 16-byte account_id.
   * @param accountIdB64 its base64 form (the pin-store key).
   * @param servedPubkey the 32-byte Ed25519 key discovery served.
   * @param oldPin       the pin AS IT WAS BEFORE gate 1 re-pinned (G-30
   *                     #2) — `undefined` on a first-ever observation.
   * @param supersessionProofB64 the bundle's `supersession_proof`
   *                     (base64), when present — the chain's old-key
   *                     authorisation.
   * @throws `LogRollbackDetected` ONLY on a `"rollback"` verdict.
   */
  private async checkTransparencyLog(
    accountId: Uint8Array,
    accountIdB64: string,
    servedPubkey: Uint8Array,
    oldPin?: AccountPubkeyPin,
    supersessionProofB64?: string,
  ): Promise<void> {
    // G-30 #2 — a ROTATION is: a pin existed, its key DIFFERS from the
    // served key, and the bundle carried a supersession proof. Only then
    // can the chain verifier anchor the transition. Anything else (first
    // observation / unchanged key / no proof) keeps the existing
    // single-key inclusion check unchanged.
    const isRotation =
      oldPin !== undefined &&
      oldPin.pinnedSignaturePubkeyB64 !== base64Encode(servedPubkey) &&
      supersessionProofB64 !== undefined &&
      supersessionProofB64.length > 0;

    let status: LogVerifyStatus;
    let isReset: boolean | null;
    let version: number | null;
    try {
      if (isRotation) {
        // `oldPin` is defined in this branch (the `isRotation` guard).
        const result = await verifyRotationChain(accountId, servedPubkey, {
          supersessionProofB64: supersessionProofB64 as string,
          pinnedOldPubkey: base64Decode(oldPin.pinnedSignaturePubkeyB64),
          pinnedLogVersion: oldPin.pinnedLogVersion ?? 0,
        });
        ({ status, isReset, version } = result);
      } else {
        const r = await verifyIdentityInLog(accountId, servedPubkey);
        status = r.status;
        isReset = r.isReset;
        version = null;
      }
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

    // G-30 #2 — persist the chain-confirmed log version ONLY on a
    // `"confirmed"` rotation chain (where `version !== null`). A transient
    // `unreachable` / `mismatch` / `absent` verdict must NOT clear a
    // previously-confirmed floor, so we never pass `null` here.
    if (version !== null) {
      recordLogVersion(accountIdB64, version);
    }

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
    this.reportUsernameResolved(recipientSignaturePub, lookup.normalisedUsername);

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
    const senderIdentity = await this.resolveAccountIdentity(identity);
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
    // ADR-0048 §2 — the sender's profile rides the sealed plaintext.
    const profileEnvelope = await this.profileEnvelopeForRequest();
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
        profileEnvelope,
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
    const myIdentity = await this.resolveAccountIdentity(identity);

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
   * ADR-0050 — poll this DEVICE's mailbox: the pseudonym of the device leaf
   * key, where Welcomes sealed to this device's KeyPackages arrive. Only
   * Welcome envelopes are surfaced; anything else in a device mailbox is
   * dropped (the cursor still advances).
   */
  async pollDeviceMailbox(
    identity: IdentitySecrets | null,
    after: number,
    waitSecs = 0,
    signal?: AbortSignal,
  ): Promise<PollResult> {
    const wasm = await loadWasm();
    const device = await this.resolveIdentity(identity);
    const pseudonym = wasm.IdentityPseudonym.fromIdentity(
      device.signaturePublicKey,
    ).asBytes();
    let raw;
    try {
      raw = await this.ds.fetchMailbox(pseudonym, {
        after,
        waitSecs,
        ...(signal !== undefined ? { signal } : {}),
      });
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    const events: IncomingMailboxEvent[] = [];
    let cursor = after;
    for (const message of raw.messages) {
      cursor = message.id;
      const env = parseEnvelope(message.payload);
      if (env.kind === KIND_CONTACT_ACK) {
        // A ContactAck routed by the requester's device key (a sender that
        // signed its request with the device identity) lands here; it is a
        // verified accept all the same, so it is dispatched like the account
        // mailbox does rather than silently skipped.
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
      if (env.kind !== KIND_WELCOME) continue;
      events.push({
        kind: "welcome",
        welcome: {
          messageId: message.id,
          welcomeBytes: env.body,
          createdAt: message.createdAt,
        },
      });
    }
    return { events, cursor, hasMore: raw.hasMore };
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
   * @param signal     Aborts the fetch (a parked long poll on sign-out).
   *
   * No cursor re-read and no serialization here: this is a plain
   * fetch-and-decode. Any caller that can overlap with another poll of
   * the same mailbox must go through `contacts/mailbox-poller.ts`, which
   * drops a batch whose cursor moved and serializes the dispatch.
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
    signal?: AbortSignal,
  ): Promise<PollResult> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveAccountIdentity(identity);
    const myPseudonym = wasm.IdentityPseudonym.fromIdentity(
      myIdentity.signaturePublicKey,
    ).asBytes();
    const myX25519Secret = myIdentity.x25519SecretKey;

    let raw;
    try {
      raw = await this.ds.fetchMailbox(myPseudonym, {
        after,
        waitSecs,
        ...(signal !== undefined ? { signal } : {}),
      });
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
            fromAccountMailbox: true,
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
      let profile: ReceivedProfile | null = null;
      try {
        const opened = parsed.openEnvelope(myX25519Secret);
        intro = new TextDecoder("utf-8", { fatal: false }).decode(opened.introMessage);
        // ADR-0048 — a profile out of bounds arrives as `undefined` with the
        // intro intact; the request itself is still a valid request.
        const p = opened.profile;
        if (p !== undefined) {
          profile = {
            displayName: p.displayName ?? null,
            username: p.username ?? null,
            updatedAtMs: p.updatedAtMs,
            // Copied before `free()`: the getter's view dies with the handle.
            accountId: p.accountId === undefined ? null : p.accountId.slice(),
          };
          p.free();
        }
        opened.free();
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
          profile,
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
   *   3. `group.addMembers(allDeviceKeyPackages)` produces ONE
   *      `(commit, welcome)` pair for every device of the recipient
   *      plus our own other devices (ADR-0050). Per MLS, `commit`
   *      would normally be broadcast to all existing members — but
   *      our group is one-of-one before this call, so there's
   *      no one to broadcast to. The commit is dropped.
   *   4. POST the same welcome bytes to each device's mailbox
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

    // 1. Fetch every device KeyPackage of the recipient (ADR-0050): a
    //    person is added as all of their devices, each its own leaf.
    let recipientDevices;
    try {
      recipientDevices = await this.discovery.fetchKeyPackages(recipientAccountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (recipientDevices === null || recipientDevices.length === 0) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }
    void recipientSignaturePubkey;

    // 2. Resolve our DEVICE identity (ADR-0050 — the leaf this device holds
    //    in every group; same instance every Group on this device shares, so
    //    mutations to its provider survive across calls and get snapshotted
    //    to IDB after this method returns).
    const myIdentity = await this.resolveIdentity(identity);
    const group = new wasm.Group(myIdentity);

    // 3. Add every device of the recipient AND our own OTHER devices in ONE
    //    commit (ADR-0050). We are the only member, so the commit fans out to
    //    nobody and is dropped; the ONE Welcome carries every added leaf's
    //    secrets and goes, byte-identical, to each leaf's own mailbox. One
    //    commit per device would strand every device but the last at a
    //    Welcome epoch we then advanced past with nobody to tell.
    const ownDevices = await this.ownOtherDevices(identity, myIdentity);
    const devices = [...recipientDevices, ...ownDevices];
    let added;
    try {
      added = group.addMembers(
        myIdentity,
        devices.map((d) => d.keyPackage),
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    void added.commit;
    // ADR-0053 contract point 2 — claim the journal at creation, at the epoch
    // the one commit above moved it to (nobody ever holds epoch 0: the commit
    // fans out to nobody). Unsigned: this is the first registration of an
    // unclaimed journal. Best effort — a failure leaves the journal unclaimed,
    // which behaves as before, and the first open claims it.
    {
      const founding = takeSigner(() => group.dsAccessSigner(myIdentity));
      if (founding !== null) {
        try {
          await this.ds.claimGroupWith(group.groupId, founding, null);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("ds: claim at group creation failed; claimed on first open", err);
        } finally {
          founding.free();
        }
      }
    }
    let firstWelcomeMessageId = 0;
    for (const dev of devices) {
      let publish;
      try {
        publish = await this.sendWelcome(
          wasm.keyPackageSignatureKey(dev.keyPackage),
          added.welcome,
        );
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      if (firstWelcomeMessageId === 0) firstWelcomeMessageId = publish.messageId;
    }
    const publish = { messageId: firstWelcomeMessageId };

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
    this.reportUsernameResolved(recipientSignaturePubkey, lookup.normalisedUsername);

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
   * register this id with `ChatClient.markOwnPublishedCommit(groupIdB64, id)`
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
  ): Promise<MembershipOutcome> {
    // 1. Fetch every device KeyPackage of the new member (ADR-0050).
    let devices;
    try {
      devices = await this.discovery.fetchKeyPackages(recipientAccountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (devices === null || devices.length === 0) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }
    void recipientSignaturePubkey;

    // 2. Resolve the canonical Identity — same instance every
    //    Group on this device uses (1.16e6c2). The `addMember`
    //    call below mutates this Identity's provider storage:
    //    appends the new member's leaf, increments the epoch,
    //    queues the Welcome state for the recipient.
    const myIdentity = await this.resolveIdentity(identity);

    // SERIALIZED per group (`withGroupMlsLock`) from here to the ack. The
    // create-commit at step 3 eager-merges N -> N+1, so it MUST be inside: a
    // concurrent commit region that created its own commit against the same N
    // would publish on an epoch peers never reach. The KeyPackage fetch above
    // stays outside — it touches no MLS state and would only lengthen the hold.
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      // ADR-0045 — a group carrying an ADR-0022 role table routes membership
      // through proposals. The direct path stays for a group with no role
      // table, which has no committer and whose peers' receipt gates would drop
      // a proposal outright.
      //
      // The core enforces this from the other side too: `add_member` REFUSES
      // while anything is pending, because every OpenMLS entry point the direct
      // paths are built on sweeps the whole pending set alongside the one change
      // it was asked for — an admin asking to add one member would add two, and
      // route the single Welcome to only one of them.
      // ADR-0050: one proposal / one commit per device of the person. The
      // outcome reported is the last device's; every earlier one either
      // landed or threw.
      let outcome: MembershipOutcome | null = null;
      for (const dev of devices) {
        outcome = usesProposalPath(group)
          ? await this.proposeAddMemberLocked(myIdentity, group, dev.keyPackage)
          : await this.addMemberToGroupLocked(myIdentity, group, dev.keyPackage);
      }
      // `devices` is non-empty (checked above), so `outcome` is set.
      return outcome as MembershipOutcome;
    });
  }

  /**
   * ADR-0045 — publish an Add **proposal** instead of committing one.
   *
   * The proposal is retained in this device's own pending set as well as
   * published, so a committer running here sweeps it like any other. It is
   * published even when this device is the committer and will sweep it a
   * moment later, and that is not redundant: OpenMLS resolves a commit's
   * by-reference proposals through the proposal store on **every receiving
   * member**, not only on the committer at sweep time, so a peer that never saw
   * the proposal refuses the commit carrying it with
   * `InvalidCommit(MissingProposal)`.
   *
   * The epoch does not advance and there is no Welcome yet — the Welcome is
   * produced by whichever device sweeps — so there is nothing to roll back and
   * no ADR-0040 outbox entry to write. That is the whole point of the option:
   * a proposal that loses a race costs nothing, where a commit that loses one
   * forks its author off the group.
   */
  private async proposeAddMemberLocked(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    keyPackageBytes: Uint8Array,
  ): Promise<MembershipOutcome> {
    let proposalBytes: Uint8Array;
    try {
      proposalBytes = group.proposeAddMember(myIdentity, keyPackageBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    // The proposal is now in this device's own pending store, which is provider
    // state — persist so a reload does not lose a request the peers hold.
    await this.persistBestEffort();
    let publish;
    try {
      publish = await this.ds.publishGroup(group.groupId, proposalBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    return { kind: "proposed", proposalMessageId: publish.id };
  }

  /** The serialized body of {@link addMemberToGroup} — see that method for the
   *  step numbering. Runs under `withGroupMlsLock`; never call it directly. */
  private async addMemberToGroupLocked(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    keyPackageBytes: Uint8Array,
  ): Promise<MembershipOutcome> {
    // 3. Drive the MLS add. Returns `{ commit, welcome }` — the
    //    commit is the wire bytes existing members process to
    //    advance their tree; the welcome is what the new member
    //    decodes via `joinFromWelcome`.
    // ADR-0053 — the key of the epoch this commit leaves, taken BEFORE the
    // create call eager-merges N -> N+1.
    const signerN = takeSigner(() => group.dsAccessSigner(myIdentity));
    let added;
    try {
      added = group.addMember(myIdentity, keyPackageBytes);
    } catch (err) {
      signerN?.free();
      throw ContactsError.wrap(err);
    }
    const commitBytes = added.commit;
    const welcomeBytes = added.welcome;

    // 4+5. ADR-0040 (G-35) + F1 — from just after the eager-merge above through
    //    the successful atomic persist is ONE fail-closed region. The core
    //    `addMember` eager-merged epoch N -> N+1 in the in-memory provider, so ANY
    //    throw before the publish — the Welcome-artifact computation
    //    (`IdentityPseudonym.fromIdentity` / `wrapEnvelope`, which CAN throw) OR
    //    the persist itself — must reset the in-memory group back to the durable N
    //    snapshot before aborting; otherwise a later same-session commit builds on
    //    the un-published N+1 and strands the WHOLE group ("device-ahead"). Compute
    //    the second artifact's EXACT publishable bytes + its mailbox destination
    //    UP-FRONT (so the outbox entry holds the very bytes the restart sweep
    //    re-publishes — no drift), then persist the entry (commit + Welcome bytes,
    //    both artifacts) AND the N+1 snapshot in ONE atomic write, FAIL-CLOSED. A
    //    throw ABORTS: no publish, group reset to N — on the next launch the device
    //    is at N with no entry, clean, as if the op never happened. Publish stays
    //    OUTSIDE this region: a publish failure must NOT reset (the durable entry
    //    is re-published idempotently by `sweepPendingCommits`).
    let groupIdBytes: Uint8Array;
    let welcomePseudonym: Uint8Array;
    let welcomeEnvelope: Uint8Array;
    let entryId: string;
    try {
      groupIdBytes = group.groupId;
      const wasm = await loadWasm();
      // ADR-0050: the Welcome is sealed to THIS leaf's KeyPackage, so it is
      // addressed to the leaf's own mailbox, never the account's.
      welcomePseudonym = wasm.IdentityPseudonym.fromIdentity(
        wasm.keyPackageSignatureKey(keyPackageBytes),
      ).asBytes();
      welcomeEnvelope = wrapEnvelope(KIND_WELCOME, welcomeBytes);
      // ADR-0053 contract point 3 — register N+1 before anything durable says
      // the commit exists. A refusal lands in the catch below: reset, no publish.
      await this.registerNextEpochOrThrow(group, myIdentity, signerN);
      entryId = newOutboxEntryId();
      await this.persistCommitOrThrow({
        id: entryId,
        groupId: base64Encode(groupIdBytes),
        targetEpoch: group.epoch.toString(),
        commitB64: base64Encode(commitBytes),
        secondArtifactB64: base64Encode(welcomeEnvelope),
        secondArtifactKind: "welcome",
        secondArtifactMailboxB64: base64Encode(welcomePseudonym),
      });
    } catch (err) {
      await this.resetGroupAfterPersistAbort(group);
      throw ContactsError.wrap(err);
    } finally {
      signerN?.free();
    }

    // 6. Publish artifact 1 — the commit to the DS group journal. Without it the
    //    existing members' local state never moves past the pre-add epoch.
    let commitPublish;
    try {
      commitPublish = await this.ds.publishGroup(groupIdBytes, commitBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 7. Publish artifact 2 — the Welcome to the new member's stranger inbox
    //    (the SAME bytes stored in the outbox entry).
    let welcomePublish;
    try {
      welcomePublish = await this.ds.publishMailbox(
        welcomePseudonym,
        welcomeEnvelope,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    // 8. Both artifacts durable on the wire -> ack (best-effort; a crash here
    //    just triggers a harmless idempotent re-publish on the next sweep).
    await this.ackCommitBestEffort(entryId, commitPublish.id, welcomePublish.id);

    return {
      kind: "committed",
      commitMessageId: commitPublish.id,
      welcomeMessageId: welcomePublish.id,
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
  ): Promise<MembershipOutcome> {
    const myIdentity = await this.resolveIdentity(identity);

    // SERIALIZED per group — the create-commit below eager-merges N -> N+1, so
    // it must run against the epoch the predecessor region actually published.
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      // ADR-0050 — removing a member removes the PERSON: every leaf of the
      // account the named leaf belongs to. Leaving (naming one's own leaf)
      // therefore leaves from every device at once.
      const leaves = leavesOfSameAccount(group, leafIndex);
      // ADR-0045 — proposal path for a role-table group; the direct commit
      // survives only where there is no role table and therefore no committer.
      let outcome: MembershipOutcome | null = null;
      for (const leaf of leaves) {
        outcome = usesProposalPath(group)
          ? await this.proposeRemoveMemberLocked(myIdentity, group, leaf)
          : await this.removeMemberFromGroupLocked(myIdentity, group, leaf);
      }
      return outcome as MembershipOutcome;
    });
  }

  /**
   * ADR-0050 section 2.8 / 2.10 — admit ONE device (by its published
   * KeyPackage) to a group this device is in: the pairing / confirmation
   * fan-out on the old device, and a contact's re-admission of a person's new
   * device. Self-service on the core side: a member may add a leaf of its own
   * account or of an account already in the group, no role needed.
   *
   * Resolves `null` when a leaf with that signature key is already in the
   * roster — nothing to do, not an error. Otherwise the ADR-0045 split:
   * a role-table group gets a proposal (swept by the caller; see
   * `BackgroundReceiveApi.admitDevice`), a table-less group a direct commit
   * with its Welcome sent to the device's own mailbox.
   */
  async admitDeviceToGroup(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    keyPackage: Uint8Array,
  ): Promise<MembershipOutcome | null> {
    const wasm = await loadWasm();
    const deviceKey = wasm.keyPackageSignatureKey(keyPackage);
    if (leafIndexOfSignatureKey(group, deviceKey) !== null) return null;
    const myIdentity = await this.resolveIdentity(identity);
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      // Re-checked under the lock: a poll may have applied the very commit
      // that admitted this device while we waited.
      if (leafIndexOfSignatureKey(group, deviceKey) !== null) return null;
      return usesProposalPath(group)
        ? this.proposeAddMemberLocked(myIdentity, group, keyPackage)
        : this.addMemberToGroupLocked(myIdentity, group, keyPackage);
    });
  }

  /**
   * ADR-0050 section 2.10 — retire ONE device's leaf from a group: the
   * removal fan-out (Devices screen, forget-this-device). Unlike
   * {@link removeMemberFromGroup}, which removes the PERSON, this names a
   * single leaf by its signature key and leaves the account's other devices
   * in place. Self-service on the core side for the account's own leaves.
   *
   * Resolves `null` when no leaf holds that key in this group.
   */
  async retireDeviceFromGroup(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    deviceKey: Uint8Array,
  ): Promise<MembershipOutcome | null> {
    if (leafIndexOfSignatureKey(group, deviceKey) === null) return null;
    const myIdentity = await this.resolveIdentity(identity);
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      const leaf = leafIndexOfSignatureKey(group, deviceKey);
      if (leaf === null) return null;
      return usesProposalPath(group)
        ? this.proposeRemoveMemberLocked(myIdentity, group, leaf)
        : this.removeMemberFromGroupLocked(myIdentity, group, leaf);
    });
  }

  /**
   * ADR-0045 — publish a Remove **proposal**.
   *
   * Naming this device's OWN leaf is how a member leaves, and needs no role;
   * any other leaf needs owner or admin, and the owner's leaf is refused for
   * everyone. The core runs that rule locally before anything is published, so
   * an unauthorised request never reaches the wire and the caller gets a
   * synchronous, specific error — but the local check is a courtesy: the
   * enforcing copies are every peer's receipt gate and commit gate, which do
   * not trust us.
   *
   * The member STAYS in the roster until a sweep lands, and the UI must keep
   * showing them. Dropping the row now would assert they are gone while they
   * are still in the group and still receiving every message.
   */
  private async proposeRemoveMemberLocked(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    leafIndex: number,
  ): Promise<MembershipOutcome> {
    let proposalBytes: Uint8Array;
    try {
      proposalBytes = group.proposeRemoveMember(myIdentity, leafIndex);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    await this.persistBestEffort();
    let publish;
    try {
      publish = await this.ds.publishGroup(group.groupId, proposalBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    return { kind: "proposed", proposalMessageId: publish.id };
  }

  /** The serialized body of {@link removeMemberFromGroup}. Runs under
   *  `withGroupMlsLock`; never call it directly. */
  private async removeMemberFromGroupLocked(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    leafIndex: number,
  ): Promise<MembershipOutcome> {
    // ADR-0053 — the key of the epoch this commit leaves, taken BEFORE the
    // create call eager-merges N -> N+1.
    const signerN = takeSigner(() => group.dsAccessSigner(myIdentity));
    let commitBytes: Uint8Array;
    try {
      commitBytes = group.removeMember(myIdentity, leafIndex);
    } catch (err) {
      signerN?.free();
      throw ContactsError.wrap(err);
    }
    // ADR-0040 (G-35) + F1 — from just after the eager-merge above through the
    // successful atomic persist is ONE fail-closed region: the core `removeMember`
    // eager-merged epoch N -> N+1, and although a remove has no throwing
    // pre-persist artifact, the reset-guarded try starts right after the
    // create-commit (uniform with `addMemberToGroup` / `approveExternalJoin`) so
    // the invariant — ANY throw before the publish resets the in-memory group back
    // to the durable N snapshot — holds identically on all four commit paths and
    // cannot be reintroduced by a future pre-persist addition. Persist the outbox
    // entry (commit only; remove has no Welcome) + the N+1 snapshot atomically,
    // FAIL-CLOSED. A throw ABORTS: no publish, group reset to N. Publish stays
    // OUTSIDE this region (a publish failure must NOT reset — the durable entry is
    // re-published by `sweepPendingCommits`).
    let groupIdBytes: Uint8Array;
    let entryId: string;
    try {
      groupIdBytes = group.groupId;
      // ADR-0053 contract point 3 — register N+1 before the outbox write; a
      // refusal lands in the catch below: reset, no publish.
      await this.registerNextEpochOrThrow(group, myIdentity, signerN);
      entryId = newOutboxEntryId();
      await this.persistCommitOrThrow({
        id: entryId,
        groupId: base64Encode(groupIdBytes),
        targetEpoch: group.epoch.toString(),
        commitB64: base64Encode(commitBytes),
      });
    } catch (err) {
      await this.resetGroupAfterPersistAbort(group);
      throw ContactsError.wrap(err);
    } finally {
      signerN?.free();
    }

    let commitPublish;
    try {
      commitPublish = await this.ds.publishGroup(groupIdBytes, commitBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    await this.ackCommitBestEffort(entryId, commitPublish.id);

    return {
      kind: "committed",
      commitMessageId: commitPublish.id,
      welcomeMessageId: undefined,
    };
  }

  /** ADR-0022 — promote / demote a member as an admin (owner only). Reads the
   *  group's current role table, adds or removes `memberSigPubkey` from the
   *  admin set, and produces + publishes the `setGroupRoles`
   *  GroupContextExtensions commit. Peers accept it only because the committer
   *  is the owner (the gate enforces that), so the caller must already be the
   *  owner — the UI shows the toggle only then. The caller registers the
   *  returned `commitMessageId` with `ChatClient.markOwnPublishedCommit`. */
  async setMemberAdmin(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    memberSigPubkey: Uint8Array,
    makeAdmin: boolean,
  ): Promise<MembershipOutcome> {
    const myIdentity = await this.resolveIdentity(identity);

    // SERIALIZED per group. Unlike the other three, this region deliberately
    // begins BEFORE the role-table read below rather than merely before the
    // create, because the read is what would go stale.
    //
    // Honest scope: that placement fixes nothing TODAY. The read, the admin-set
    // recomputation and `setGroupRoles` currently run with no `await` between
    // them, so the read-modify-write already completes in one microtask and a
    // concurrent toggle cannot observe a half-updated table. The boundary is
    // placed here so that stays true — insert one `await` between the read and
    // the create (an async role lookup, a confirmation prompt) and without this
    // the second toggle would recompute from the pre-existing table and publish
    // one that silently erases the first's change, which is the read-modify-write
    // shape ADR-0044 pins as C-62 sub-invariant 5. Costs nothing, so it is not
    // worth leaving to a future reader to rediscover.
    return withGroupMlsLock(base64Encode(group.groupId), async () =>
      this.setMemberAdminLocked(myIdentity, group, memberSigPubkey, makeAdmin),
    );
  }

  /** Read the group's current role table and return the admin set with
   *  `memberSigPubkey` added or removed. Shared by the direct and the proposal
   *  path so the two can never compute a different table. */
  private nextAdminSet(
    group: WasmGroup,
    memberSigPubkey: Uint8Array,
    makeAdmin: boolean,
  ): { owner: Uint8Array; adminsFlat: Uint8Array } {
    const owner = group.roleTableOwner();
    if (owner === undefined) {
      throw ContactsError.wrap(new Error("group has no role table"));
    }
    const flat = group.roleTableAdmins();
    const eq = (a: Uint8Array, b: Uint8Array): boolean =>
      a.length === b.length && a.every((x, i) => x === b[i]);
    const current: Uint8Array[] = [];
    for (let i = 0; i + 32 <= flat.length; i += 32) {
      current.push(flat.slice(i, i + 32));
    }
    let next = current.filter((a) => !eq(a, memberSigPubkey));
    if (makeAdmin) next = [...next, memberSigPubkey];
    const adminsFlat = new Uint8Array(next.length * 32);
    next.forEach((a, i) => adminsFlat.set(a, i * 32));
    return { owner, adminsFlat };
  }

  /** The serialized body of {@link setMemberAdmin} — including the role-table
   *  read. Runs under `withGroupMlsLock`; never call it directly. */
  private async setMemberAdminLocked(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    memberSigPubkey: Uint8Array,
    makeAdmin: boolean,
  ): Promise<MembershipOutcome> {
    const { owner, adminsFlat } = this.nextAdminSet(
      group,
      memberSigPubkey,
      makeAdmin,
    );

    // ADR-0045 — a role change in a role-table group is a PROPOSAL. Note the
    // asymmetry with add / remove is only apparent: reaching this method at all
    // means the group HAS a role table (`nextAdminSet` refuses without one), so
    // the direct arm below is unreachable in practice and is kept only because
    // the local role read is what proves it.
    if (usesProposalPath(group)) {
      // A SECOND role change while one is pending is refused, and this is a
      // real consequence of the proposal path rather than caution.
      //
      // A role change is a read-modify-write over the role table, and on the
      // direct path the per-group lock was enough: the first toggle's commit
      // updated the table, so the second read it and composed. A PROPOSAL
      // changes nothing until it is swept, so two toggles taken a moment apart
      // both read the SAME committed table and each proposes a table carrying
      // only its own change. RFC 9420 permits at most one
      // `GroupContextExtensions` proposal per commit, so the sweep keeps one
      // and silently discards the other — the first promotion would vanish with
      // no error anywhere. Serializing cannot fix that, because there is
      // nothing for the second read to observe.
      //
      // Recomputing from "the committed table as amended by our own pending
      // proposal" is not available either: `ProposalKind::RoleChange` carries
      // no payload (the core's `classify` discards the extensions), so a client
      // cannot read back what it proposed.
      //
      // So the honest answer is to refuse, and say so. The window is one sweep
      // — the owner is rank 0 and sweeps immediately — so this is momentary in
      // the ordinary case and a real, visible wait only when the committer is
      // absent, which is precisely when a silent loss would be worst.
      if (hasPendingRoleChange(group)) {
        throw ContactsError.wrap(new KeyedError(ROLE_CHANGE_ALREADY_PENDING_KEY));
      }
      let proposalBytes: Uint8Array;
      try {
        proposalBytes = group.proposeGroupRoles(myIdentity, owner, adminsFlat);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      await this.persistBestEffort();
      let publish;
      try {
        publish = await this.ds.publishGroup(group.groupId, proposalBytes);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      return { kind: "proposed", proposalMessageId: publish.id };
    }

    // ADR-0053 — the key of the epoch this commit leaves, taken BEFORE the
    // create call eager-merges N -> N+1.
    const signerN = takeSigner(() => group.dsAccessSigner(myIdentity));
    let commitBytes: Uint8Array;
    try {
      commitBytes = group.setGroupRoles(myIdentity, owner, adminsFlat);
    } catch (err) {
      signerN?.free();
      throw ContactsError.wrap(err);
    }
    // ADR-0040 (G-35) + F1 — from just after the eager-merge above through the
    // successful atomic persist is ONE fail-closed region: the core `setGroupRoles`
    // eager-merged epoch N -> N+1, and although a role change has no throwing
    // pre-persist artifact, the reset-guarded try starts right after the
    // create-commit (uniform with `addMemberToGroup` / `approveExternalJoin`) so
    // the invariant — ANY throw before the publish resets the in-memory group back
    // to the durable N snapshot — holds identically on all four commit paths and
    // cannot be reintroduced by a future pre-persist addition. Persist the outbox
    // entry (commit only; a role change has no Welcome) + the N+1 snapshot
    // atomically, FAIL-CLOSED. A throw ABORTS: no publish, group reset to N.
    // Publish stays OUTSIDE this region (a publish failure must NOT reset).
    let groupIdBytes: Uint8Array;
    let entryId: string;
    try {
      groupIdBytes = group.groupId;
      // ADR-0053 contract point 3 — register N+1 before the outbox write; a
      // refusal lands in the catch below: reset, no publish.
      await this.registerNextEpochOrThrow(group, myIdentity, signerN);
      entryId = newOutboxEntryId();
      await this.persistCommitOrThrow({
        id: entryId,
        groupId: base64Encode(groupIdBytes),
        targetEpoch: group.epoch.toString(),
        commitB64: base64Encode(commitBytes),
      });
    } catch (err) {
      await this.resetGroupAfterPersistAbort(group);
      throw ContactsError.wrap(err);
    } finally {
      signerN?.free();
    }

    let commitPublish;
    try {
      commitPublish = await this.ds.publishGroup(groupIdBytes, commitBytes);
    } catch (err) {
      throw ContactsError.wrap(err);
    }

    await this.ackCommitBestEffort(entryId, commitPublish.id);

    return {
      kind: "committed",
      commitMessageId: commitPublish.id,
      welcomeMessageId: undefined,
    };
  }

  /**
   * ADR-0045 UX §6.1 — hand the group's owner slot to another member.
   *
   * **Always the proposal arm, and there is no second one here.** The wasm
   * surface carries `transferOwnership` too, and this method deliberately never
   * reaches it, because for this flow the direct arm is *structurally
   * unreachable*: the UI gates the control on `isOwner`, which is read off the
   * role table (`Group.ownRole` → `group_roles()`), and `usesProposalPath` is
   * the identical `group_roles()` read (`Group.roleTableOwner`). A group with no
   * role table has no owner, so `isOwner && !usesProposalPath` cannot hold. That
   * makes transfer the one membership action needing a single arm rather than
   * two, unlike add / remove / role change, each of which carries a direct
   * fallback for the table-less case. A table-less group reaching here anyway is
   * refused by the core, which is the right place for it.
   *
   * Everything else mirrors {@link setMemberAdmin}'s proposal arm exactly,
   * because a transfer **is** a role change — deliberately not a new commit
   * type, just the owner-only role-table rewrite with the owner slot moved. So
   * it inherits that arm's discipline verbatim: the per-group MLS lock, the
   * C-65 pre-check, no outbox entry and no reset region (a proposal advances no
   * epoch, so there is nothing to roll back), and a `proposed` outcome the
   * caller sweeps immediately.
   *
   * `newOwnerSigPubkey` is the successor's 32-byte Ed25519 signature public key
   * — the stable handle `Group.members()` reports and the role table keys on —
   * never a leaf index: OpenMLS hands a vacated leaf to the next Add.
   *
   * `outgoing` decides what becomes of THIS device's own role and is required.
   * The core refuses to default it and both FFI surfaces carry that structurally
   * (wasm's `OutgoingOwner` discriminants start at `1`, so a missing argument
   * throws rather than silently meaning `Admin`); this signature keeps the
   * property by taking a required union rather than an optional argument.
   */
  async transferGroupOwnership(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    newOwnerSigPubkey: Uint8Array,
    outgoing: "admin" | "member",
  ): Promise<MembershipOutcome> {
    const myIdentity = await this.resolveIdentity(identity);
    const wasm = await loadWasm();
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      // C-65, and it collides in BOTH directions: a pending role change refuses
      // a transfer, and a pending transfer refuses a role change. It falls out
      // of `propose_ownership_transfer` delegating to `propose_group_roles`
      // rather than being restated, which is the point. The UI disables the
      // control with this string as its reason once the blocking request is past
      // the quiet window (UX §6.1.8's deliberate asymmetry with the admin
      // toggles, which keep refuse-on-tap); this is the check for the window in
      // which the control is still live.
      if (hasPendingRoleChange(group)) {
        throw ContactsError.wrap(new KeyedError(ROLE_CHANGE_ALREADY_PENDING_KEY));
      }
      let proposalBytes: Uint8Array;
      try {
        proposalBytes = group.proposeOwnershipTransfer(
          myIdentity,
          newOwnerSigPubkey,
          outgoing === "admin"
            ? wasm.OutgoingOwner.Admin
            : wasm.OutgoingOwner.Member,
        );
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      await this.persistBestEffort();
      let publish;
      try {
        publish = await this.ds.publishGroup(group.groupId, proposalBytes);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      return { kind: "proposed", proposalMessageId: publish.id };
    });
  }

  /**
   * ADR-0027 (G-3), on the ADR-0045 proposal path since **S7** — approve a
   * pending external-join request (owner / admin only).
   *
   * `proposalRef` is the joiner's leaf signature key surfaced by an
   * `externalJoinRequest` event or `group.pendingExternalJoins()`.
   *
   * ## One artifact, and no epoch
   *
   * The core publishes an ordinary admin-authored `Add` **proposal** carrying
   * the joiner's retained KeyPackage — the admin's own MLS-authenticated
   * signature *is* the approval — so this posts exactly one thing to the group
   * journal and the epoch does not move. Everything the old direct-commit
   * shape needed is therefore gone: no Welcome to compute here, no
   * `WelcomeRendezvous` to post here, and no ADR-0040 outbox entry, because a
   * proposal makes nothing durable that a crash could strand.
   *
   * The joiner's Welcome — and its `WelcomeRendezvous` — is produced by
   * whichever device later sweeps this proposal into a commit. See
   * `chat/sweep.ts`: that sweep posts one rendezvous per added key, blind to
   * which of them came from a link.
   *
   * ## No pre-drain
   *
   * The old path had to sweep the pending set first, because the direct commit
   * it used was refused while anything was pending. That dead end is gone: an
   * approve is now a proposal like any other and composes with whatever else is
   * waiting.
   *
   * A refusal (wrong role, a full pending set) throws and leaves the request
   * pending in the core, so the Approve affordance stays and can be retried.
   */
  async approveExternalJoin(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    proposalRef: Uint8Array,
  ): Promise<{ kind: "proposed"; proposalMessageId: number }> {
    const myIdentity = await this.resolveIdentity(identity);

    // SERIALIZED per group, like every other path that reads group state and
    // writes a snapshot — a proposal grows the pending set, so two approves
    // taken at once are still a read-modify-write on the same MLS store.
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      let proposalBytes: Uint8Array;
      try {
        proposalBytes = group.approveExternalJoin(myIdentity, proposalRef);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      await this.persistBestEffort();
      let publish;
      try {
        publish = await this.ds.publishGroup(group.groupId, proposalBytes);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      return { kind: "proposed" as const, proposalMessageId: publish.id };
    });
  }

  /** ADR-0027 (G-3) — reject a pending external-join request. Drops the
   *  retained proposal so it no longer surfaces in
   *  `group.pendingExternalJoins()`. Produces no commit and no journal post —
   *  the joiner simply never receives a Welcome and its proposal lapses when the
   *  group epoch next advances. The UI shows the action to owners / admins
   *  only; there is no peer-verifiable gate because there is no network
   *  artefact to gate.
   *
   *  The retained set is no longer in-memory only: the core persists it inside
   *  the MLS storage snapshot (which is why the wasm call takes the identity,
   *  like `approveExternalJoin`), so the `persistBestEffort()` after the drop
   *  writes the rejection to the durable snapshot and it survives a restart —
   *  a rejected request does not come back as pending when the owner's app
   *  reopens. */
  async rejectExternalJoin(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    proposalRef: Uint8Array,
  ): Promise<void> {
    const myIdentity = await this.resolveIdentity(identity);

    // SERIALIZED per group like `approveExternalJoin`: the drop rewrites the
    // retained set in the MLS store and the snapshot that follows captures it.
    await withGroupMlsLock(base64Encode(group.groupId), async () => {
      try {
        group.rejectExternalJoin(myIdentity, proposalRef);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      await this.persistBestEffort();
    });
  }

  /** ADR-0028 OQ2 (G-3) — read the join-attribution envelopes from a group's
   *  journal so the admin UI can show *which member's* `/g/` link each pending
   *  external-join request arrived on. Fetches the journal, parses every entry
   *  via `GroupJoinAttribution.parse` (most are MLS messages and throw — those
   *  are skipped), re-verifies the embedded capability (Ed25519, inside
   *  `parse`), and keeps only attributions whose `groupId` matches this group
   *  (defence against a replayed cross-group capability). The caller matches a
   *  returned `joinerSignatureKey` to a pending request and checks
   *  `issuerSignaturePubkey ∈ roster` against the live `group.members()`. The
   *  attribution is advisory — the role-gated approval gate is the boundary. */
  async fetchJoinAttributions(groupId: Uint8Array): Promise<
    Array<{
      joinerSignatureKey: Uint8Array;
      issuerSignaturePubkey: Uint8Array;
      issuerFingerprint: string;
    }>
  > {
    const wasm = await loadWasm();
    // ADR-0053 — a joiner posts its attribution as a KNOCK, never to the
    // journal (which it cannot read or write), so the attributions live in the
    // group's knock queue. Paged: knocks are pruned after seven days, but a
    // popular link can still outgrow one page.
    const raw: { messages: Array<{ id: number; payload: Uint8Array }> } = { messages: [] };
    try {
      await scanJournal<never>(
        (after, limit) => this.ds.fetchKnocks(groupId, { after, limit, waitSecs: 0 }),
        (m) => {
          raw.messages.push(m);
          return undefined;
        },
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    const out: Array<{
      joinerSignatureKey: Uint8Array;
      issuerSignaturePubkey: Uint8Array;
      issuerFingerprint: string;
    }> = [];
    for (const m of raw.messages) {
      let attr;
      try {
        attr = wasm.GroupJoinAttribution.parse(m.payload);
      } catch {
        continue; // not an attribution envelope (an MLS message / other) — skip
      }
      // Only trust an attribution whose capability targets THIS group.
      if (base64Encode(attr.groupId) !== base64Encode(groupId)) continue;
      out.push({
        joinerSignatureKey: attr.joinerSignatureKey,
        issuerSignaturePubkey: attr.issuerSignaturePubkey,
        issuerFingerprint: attr.issuerFingerprint,
      });
    }
    return out;
  }

  /** ADR-0028 (G-3) — mint a group invite-link (the owner/admin side). Signs a
   *  `GroupInviteCapability` over this group's `group_id` with the local
   *  identity's Ed25519 key and returns the full `<origin>/g/<base64url>` link
   *  the owner shares out-of-band. A non-member opens it, reads the group's
   *  current epoch from the journal, and posts an external-join proposal the
   *  admin approves (ADR-0027). The link conveys only the `group_id` — it
   *  authorizes nobody; the admin approval gate is the security boundary.
   *
   *  ADR-0028 v2 (docs/ux/group-invite-page.md): the capability also carries a
   *  **sealed preview** — the group's name, the inviter's display name and the
   *  member count at mint time — encrypted under a fresh link key that rides
   *  the URL **fragment** (`#<key>`), which no browser sends and no ledger
   *  stores. Both the pointer and the inline form carry the same fragment. A
   *  name the display-name rule refuses is left out rather than failing the
   *  mint: the page then shows no name, never a guess.
   *
   *  `expiryMs` is UNIX millis (`0` = never expires). The UI gates the
   *  affordance on `ownRole` (owner / admin), matching add / remove. */
  async createGroupInviteLink(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    expiryMs: bigint,
    origin: string,
    preview: GroupInviteMintPreview,
    maxUses: number = 0xff_ff_ff_ff,
  ): Promise<string> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveAccountIdentity(identity);
    const memberCount = Math.min(0xff_ff, Math.max(0, Math.floor(preview.memberCount)));
    const mintWith = (names: {
      groupName?: string | undefined;
      inviterName?: string | undefined;
    }) =>
      wasm.composeGroupInviteCapabilityWithPreview(
        myIdentity.ed25519Seed,
        group.groupId,
        expiryMs,
        names.groupName,
        names.inviterName,
        memberCount,
      );
    let wire: Uint8Array;
    let linkKey: Uint8Array;
    try {
      let mint: WasmGroupInviteMint;
      try {
        mint = mintWith({ groupName: preview.groupName, inviterName: preview.inviterName });
      } catch (err) {
        // The names come from user-typed fields (a group label, a profile
        // name); one the rule refuses must not take the link with it.
        // eslint-disable-next-line no-console
        console.warn("group-invite: preview names refused, minting without names", err);
        mint = mintWith({});
      }
      wire = mint.wire;
      linkKey = mint.linkKey;
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    // Production default (ADR-0028 OQ1): upload to the discovery ledger — the
    // server-side anti-abuse surface (per-joiner consume dedup + soft max_uses)
    // — and return the short pointer URL. `max_uses` is a soft throttle, not
    // an access control: the admin approval gate stays the security boundary.
    //
    // The inline form is the fallback for a TRANSPORT failure only (a
    // censored / offline network), where the link must still work without the
    // round-trip. A server answer (a 401 from a client with no session token,
    // a 429, a 5xx) surfaces instead: falling back there silently hands the
    // user the huge inline URL for a defect that should be fixed. The log
    // names the status and never the capability.
    const widths = inviteLinkWidths(wasm);
    let url: string;
    try {
      const blobId = await this.discovery.uploadGroupInvite(wire, maxUses);
      url = composePointerGroupInviteUrl(blobId, widths, origin);
    } catch (err) {
      if (!(err instanceof DiscoveryNetworkError)) {
        const status = (err as { status?: unknown }).status;
        // eslint-disable-next-line no-console
        console.warn(
          `group-invite: ledger upload failed (${
            typeof status === "number" ? `HTTP ${status}` : "local error"
          })`,
        );
        throw ContactsError.wrap(err);
      }
      // eslint-disable-next-line no-console
      console.warn("group-invite: ledger unreachable, using the inline link");
      url = composeInlineGroupInviteUrl(wire, origin);
    }
    return appendGroupInviteLinkSecret(url, linkKey, widths);
  }

  /** ADR-0028 revoke — the issuer revokes a previously-created group invite
   *  link. Only the server-backed pointer form (a 6-byte ledger id) is
   *  revocable; an inline link has no ledger row. Parses the `blob_id` out of
   *  the `/g/<slug>` link, signs the revocation with the issuer's identity key,
   *  and DELETEs the ledger row. Returns `false` (nothing to do) for an inline /
   *  malformed link. */
  async revokeGroupInviteLink(
    identity: IdentitySecrets | null,
    link: string,
  ): Promise<boolean> {
    // The slug ends where the fragment (the ADR-0028 link secret) begins.
    const widths = inviteLinkWidths(await loadWasm());
    const split = splitGroupInviteLink(link, widths);
    if (split === null) return false;
    let parsed;
    try {
      parsed = parseGroupInviteSlug(split.slug, widths);
    } catch {
      return false; // malformed, or a pre-amendment link
    }
    if (parsed.kind !== "pointer") return false; // inline link — no ledger row
    const blobId = parsed.blobId;
    const myIdentity = await this.resolveAccountIdentity(identity);
    try {
      await this.discovery.revokeGroupInvite(blobId, myIdentity.ed25519Seed);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    return true;
  }

  /** ADR-0028 (G-3) — resolve a `/g/<slug>` slug to the capability wire bytes,
   *  fetching from the discovery ledger when the slug is an 8-character
   *  ledger id (the production form) or passing the bytes straight through
   *  when it is an inline capability (the upload-failed fallback). Returns the
   *  `blobId` (non-null only for the pointer form) so the caller can drive
   *  `consumeGroupInvite` before requesting to join. A 429 from the ledger
   *  surfaces as a wrapped `DiscoveryRateLimitedError` (see
   *  `isDiscoveryRateLimited`), never as "not found". */
  async resolveGroupInviteSlug(
    slug: string,
  ): Promise<{ capabilityWire: Uint8Array; blobId: Uint8Array | null }> {
    let parsed;
    try {
      parsed = parseGroupInviteSlug(slug, inviteLinkWidths(await loadWasm()));
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (parsed.kind === "inline") {
      return { capabilityWire: parsed.capabilityBytes, blobId: null };
    }
    let fetched: Uint8Array | null;
    try {
      fetched = await this.discovery.fetchGroupInvite(parsed.blobId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (fetched === null) {
      throw ContactsError.wrap(new Error("group invite not found or expired"));
    }
    return { capabilityWire: fetched, blobId: parsed.blobId };
  }

  /** ADR-0028 (G-3) — joiner step 1: request to join a group from a `/g/` link
   *  capability (the wire bytes the slug base64url-decodes to). Decodes +
   *  verifies the capability (fail-closed on a forged / malformed / expired
   *  link, before any post), reads the group's current epoch from the journal
   *  head's public MLS framing (`epochAfterMessage` over the paged journal, no group secret), crafts an
   *  external-join proposal (ADR-0027) for that epoch, and posts it to the
   *  group journal. Returns the `groupId` + the joiner's signature key — the
   *  caller drives `pollForJoinWelcome` on a loop until an admin approves and
   *  the rendezvous Welcome arrives. */
  async requestToJoinFromLink(
    identity: IdentitySecrets | null,
    capabilityWire: Uint8Array,
    nowMs: number,
    opts?: {
      blobId?: Uint8Array | null | undefined;
      joinerAccountId?: Uint8Array | undefined;
    },
  ): Promise<{ groupId: Uint8Array; joinerSignatureKey: Uint8Array }> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveIdentity(identity);
    let cap;
    try {
      cap = wasm.GroupInviteCapability.decodeAndVerify(capabilityWire);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (!cap.isValidNow(BigInt(nowMs))) {
      throw ContactsError.wrap(new Error("group invite link has expired"));
    }
    // ADR-0028 OQ1 — when the link was a server-backed pointer, register one
    // consume against the ledger's soft `max_uses` ceiling before posting the
    // request (anti-abuse defence-in-depth; the admin approval gate remains the
    // real boundary). A repeat from this joiner is idempotent. Inline links
    // (no `blobId`) skip this — there is no ledger row to consume.
    if (opts?.blobId != null && opts.joinerAccountId != null) {
      let consume;
      try {
        consume = await this.discovery.consumeGroupInvite(
          opts.blobId,
          capabilityWire,
          opts.joinerAccountId,
        );
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      if (consume.kind === "not-found") {
        throw ContactsError.wrap(new Error("group invite not found or expired"));
      }
      if (consume.kind === "exhausted") {
        throw ContactsError.wrap(new Error("group invite has reached its use limit"));
      }
    }
    const groupId = cap.groupId;
    // ADR-0053 contract point 6 — the knock. A claimed journal answers only
    // its members, so the joiner never reads it: a probe knock (one anonymous
    // token, ADR-0054 S3)
    // returns the group's latest registered epoch, the proposal is built for
    // that epoch, and a second knock carries the attribution and the proposal
    // into the group's knock queue, which the owner and admins read.
    try {
      await knockToJoin({
        knock: (payloads) => this.ds.knock(groupId, payloads),
        buildProposal: (epoch) => myIdentity.createExternalJoinProposal(groupId, epoch),
        // ADR-0028 OQ2 — the self-signed join attribution tells the approving
        // admin which member's link this request arrived on. Advisory: a
        // failure to build it must not block the join, the admin approval gate
        // (ADR-0027) is the boundary.
        buildAttribution: () => {
          try {
            return wasm.composeGroupJoinAttribution(myIdentity.ed25519Seed, capabilityWire);
          } catch {
            return null;
          }
        },
      });
    } catch (err) {
      // `GroupNotClaimedError` carries readable copy of its own: nobody in
      // the group has opened it since the update, so no epoch is registered.
      throw ContactsError.wrap(err);
    }
    // The proposal's KeyPackage private key lives in the provider and is the
    // only thing that can open the Welcome the committer will send, minutes or
    // hours from now. Persist it now: a reload before then used to lose it,
    // and the Welcome then failed with `NoMatchingKeyPackage`, unrecoverably.
    await this.persistBestEffort();
    return { groupId, joinerSignatureKey: myIdentity.signaturePublicKey };
  }

  /** Convenience wrapper around [`addMemberToGroup`] that
   *  resolves `@username` first. Mirror of
   *  [`startGroupWithUsername`] for the existing-group path. */
  async addMemberToGroupByUsername(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    username: string,
    /**
     * ADR-0045 UX §7 — called once the typed handle has resolved to a concrete
     * account, immediately before the add is attempted.
     *
     * It exists because the caller cannot otherwise learn *who* a failed add
     * was for: the resolution happens in here and a refusal propagates as a
     * throw, so by the time the caller catches it the identity of the
     * prospective member is gone. An automatic re-send has to be keyed on the
     * resolved `(account_id, signature key)` rather than on the handle —
     * re-resolving a username later could name a different account if the
     * handle has since been re-registered, and adding the wrong person is not
     * a retry.
     */
    onResolved?: (recipient: {
      accountId: Uint8Array;
      signaturePubkey: Uint8Array;
    }) => void,
  ): Promise<{
    recipientUsername: string;
    recipientAccountId: Uint8Array;
    /** The added member's 32-byte Ed25519 signature key — the stable handle the
     *  ADR-0045 pending-request record keys on (never a leaf index: OpenMLS
     *  reuses a vacated leaf for the next Add). */
    recipientSignaturePubkey: Uint8Array;
    /** ADR-0045 — `committed` only in a group with no role table; otherwise the
     *  add was published as a proposal for a committer to sweep. */
    outcome: MembershipOutcome;
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
    this.reportUsernameResolved(recipientSignaturePubkey, lookup.normalisedUsername);
    onResolved?.({
      accountId: lookup.account,
      signaturePubkey: recipientSignaturePubkey,
    });

    const outcome = await this.addMemberToGroup(
      identity,
      group,
      lookup.account,
      recipientSignaturePubkey,
    );
    return {
      recipientUsername: lookup.normalisedUsername,
      recipientAccountId: lookup.account,
      recipientSignaturePubkey,
      outcome,
    };
  }
}
