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
 *   3. Find the recipient's mailbox (ADR-0078): the mailbox key a
 *      grant, link or public record binds, verified against the
 *      recipient's pinned account key. The address is NOT a
 *      function of the identity key.
 *   4. Compose the wire bytes via
 *      `ContactRequest.compose(senderSigningSeed,
 *      recipientX25519Pub, fromPseudonym, validAfterMs,
 *      introMessage)`.
 *   5. Seal them as one mailbox row (`contacts/mailbox-row.ts`) and
 *      POST it to `lakko-svc-ds`'s `/ds/mailbox/{address_hex}`.
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
import { hexOfKey, ownDeviceAdmissible, ownDeviceRoster } from "../devices/own-device-roster";
import { ownDeviceKeyPackageUsable } from "../devices/own-conversation-key";
import type { ConversationKind } from "../chat/conversation-kind";
import type { Wasm } from "../wasm";
import { DiscoveryClient, DiscoveryNetworkError } from "../discovery/client";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { mintedLinkFor, rememberMintedLink } from "./minted-links";
import { newRevocationKey, revokeMintedLink } from "./own-links";
import {
  appendGroupInviteLinkSecret,
  composeInlineGroupInviteUrl,
  composePointerGroupInviteUrl,
  parseGroupInviteSlug,
  splitGroupInviteLink,
} from "./group-invite";
import {
  InviteLinkDamagedError,
  inviteLinkWidths,
  openSealedInvite,
  parseInviteLinkFragment,
} from "./invite-link";
import { DsClient } from "../ds/client";
import { withGroupMlsLock } from "../mls/group-lock";
import type { IdentitySecrets } from "../auth/AuthContext";
import type {
  InviteLinkMint as WasmInviteLinkMint,
  GroupInviteCapability as WasmGroupInviteCapability,
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import type { IdentityFetchResponse } from "../discovery/wire";
import type { ReceivedProfile } from "../profile/profile-envelope";
import type { AccountPubkeyPin } from "./local-store";
import { acceptableBoundKeys, devicesBoundTo, trustedBoundKeys } from "./add-member-key";
import { contactRequestGate, requestTargetAfterFetch } from "./request-target";
import { resetVerdict, type AttestCheck, type LogCheck } from "./reset-verdict";
import { notifyResetAccepted } from "./reset-events";
import {
  dropVerifiedBinding,
  memberHeldKey,
  readVerifiedBinding,
  recordVerifiedBinding,
} from "./verified-binding";
import { fetchDeviceAttestKey } from "../auth/device-attest-key";
import { provenSuccessorWith } from "./proven-successor";
import { pendingAfterArrival } from "./inbox-store";
import { spentFailureOutcome, type SpentOutcome } from "./rotation-spent";
import { addressRecheckAllowed, lastAddressRecheckAtMs } from "./address-recheck-throttle";
import {
  acceptReset,
  accountIdB64OfKey,
  accountKeysOf,
  loadSupersededKeys,
  readPin,
  setPin,
  recordLogStatus,
  recordLogReset,
  recordLogVersion,
} from "./local-store";
import { verifyIdentityInLog, verifyRotationChain } from "../transparency/verify";
import type { LogVerifyStatus } from "../transparency/verify";
import { openOwnRow, sealRowFor } from "./mailbox-row";
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
import { isBackoffRefusal, sweepBackoff } from "../chat/sweep-backoff";
import type { ActingCommitter } from "../chat/committer-beacon";
// ADR-0045 — the ONE dispatch ladder that turns a processed MLS message into a
// timeline event. The sweep's verdict apply consumes journal rows the poll will
// never fetch again, so it has to build events for them, and it has to build
// them the same way the poll does.
import { consumeRegistrarSeed, ChatClient, rosterOf, toEvent } from "../chat/client";
import { noteOwnerTransition, readRoleTableOwner } from "../ds/root-handover";
import { bestGrantForPerson } from "../reach/received-grants";
import { personKeysHex, resolveAccountKey } from "./address-resolver";
import { DELEGATION_TTL_MS, committerCandidates } from "../reach/delegation-rules";
import type { ReachGrant as WasmReachGrant } from "../../wasm/lakko_ffi_wasm";
import { reachSession } from "../reach/reach-service";
import {
  accountKeyOfKeyPackage,
  accountKeyOfLeaf,
  MailboxUnreachableError,
  personKeysOf,
  verifiedTargetOfGrant,
  writeToPerson,
  writeWelcome,
} from "../reach/mailbox-write";
import {
  forgetJoinReply,
  joinReplyBindingVerifies,
  joinReplyFor,
  rememberJoinReply,
} from "../reach/join-replies";
import { journalHex } from "../ds/access";
import { scanJournal } from "../chat/journal-head-epoch";
import {
  dsAccess,
  knockToJoin,
  registerBeforePublish,
  takeSigner,
  type ProofSigner,
} from "../ds/access";
import { resolveLeafSignatureKey, resolveMemberIdentity } from "../chat/author-identity";
import type { IncomingChatEvent } from "../chat/client";
import {
  JournalClaimFailedError,
  claimAtCreation,
  evicteeAccountKeys,
  releaseAuthority,
  revocationKeys,
  type OwnershipRosterMember,
} from "../ds/ownership";
import { registrarListFor } from "../ds/registrar-list";
import { wasmRegistrarDerivation } from "../ds/journal-authority";
// ADR-0045 UX §6 — the seventh refusal string, defined ONCE beside the rest of
// the §6.1 copy. Both C-65 directions reuse it verbatim (a pending role change
// refuses a transfer, a pending transfer refuses a role change), so a single
// definition is what keeps the table at seven strings rather than drifting into
// two near-identical ones.
import { ROLE_CHANGE_ALREADY_PENDING_KEY } from "../chat/ownership-transfer";
import { KeyedError } from "../l10n/keyed-error";
import { recordOwnRequest, type OwnRequest } from "../chat/membership-retry";
import { ownPersonKeyB64, personKeyOf } from "../chat/person-key";

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
   *  provider snapshot to disk. Must resolve only when the snapshot
   *  was written and throw otherwise (`chat/persist-hooks.ts`): the
   *  founding path and the proposal paths publish only after it
   *  resolved; the other calls log and swallow a throw. */
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
  /** ADR-0060 section 6 — the requester's `reply` grant (the 134-byte grant
   *  wire, base64), which the accept (the 1:1 Welcome) and the ContactAck are
   *  written with; `null` when the request carried none or did not decrypt.
   *  Secret: never logged, kept only until the request is answered. */
  replyGrantB64: string | null;
  /** ADR-0079 section 2 — which of this account's `/i/` links the request
   *  answers (the link id and its secret, as the sender opened them), or
   *  `null`. SECRET: compared with the link register, never logged. */
  linkAnswer: LinkAnswer | null;
  /** ADR-0079 section 3 — set by the mailbox when the link answer named no
   *  live link of this account: the card shows "Sent with an invite link
   *  that no longer works." Absent for every other request. */
  deadLink?: boolean;
  /** ISO-8601 timestamp the DS recorded on receive. */
  createdAt: string;
}

/** ADR-0079 section 2 — the `/i/` link a contact request answers. SECRET. */
export interface LinkAnswer {
  /** The link's 16-byte id (`InviteCapability.linkId`). */
  readonly linkId: Uint8Array;
  /** The link's 12-byte secret (the URL fragment). */
  readonly linkSecret: Uint8Array;
}

/** Options of the contact-request senders (ADR-0060 section 6). */
export interface ContactRequestOptions {
  /** The grant this write is made under: an invite link's `link` grant when
   *  the request answers an `/i/` or `/c/` link. Without one, the request is
   *  signed with any grant kept for the recipient (a group or channel they let
   *  find us), else sent unsigned (which only an Everyone mailbox stores).
   *  Not consumed: the caller frees it and never keeps a link grant. */
  readonly writeGrant?: WasmReachGrant | null;
  /** ADR-0078 - the recipient's 97-byte mailbox binding from their public
   *  record (a person in Everyone): where an unsigned request goes when no
   *  grant reaches them. Verified against the recipient's key before use. */
  readonly openBinding?: Uint8Array | null;
  /** ADR-0079 section 2 — the `/i/` link this request is sent from, sealed
   *  to the recipient so their device can accept it without asking. */
  readonly linkAnswer?: LinkAnswer | null;
}

/** A verified incoming Welcome (MLS group-join blob) decoded
 *  out of the mailbox (1.16e3). */
export interface IncomingWelcome {
  messageId: number;
  /** Raw MLS Welcome bytes — feed straight into
   *  `Group.joinFromWelcome(identity, ...)`. */
  welcomeBytes: Uint8Array;
  createdAt: string;
  /** ADR-0053 — the Welcome arrived in the ACCOUNT mailbox (ADR-0078: the
   *  only mailbox there is). Since ADR-0050 a conversation Welcome is addressed to a
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
 *  on the ack has already been verified against `accepterPubkey`,
 *  which is what pairs it with the original request (and closes the
 *  `reply` key that request handed out). */
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
  /** The original request's `from_pseudonym` field, echoed back. Since
   *  ADR-0078 it is 32 random bytes chosen per request and nothing
   *  matches on it; kept because the signed ack wire carries it. */
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
  | {
      kind: "contact-request";
      request: IncomingContactRequest;
      /** The DS row the request was decoded from, exactly as received. The
       *  mailbox keeps it (not the decoded request) until Accept or Decline,
       *  so a request survives a reload the cursor has already skipped past
       *  (`contacts/inbox-store.ts`). */
      row: MailboxRow;
    }
  | { kind: "welcome"; welcome: IncomingWelcome }
  | { kind: "contact-ack"; ack: IncomingContactAck };

/** One account-mailbox row as the DS served it. `payload` is the envelope
 *  (kind byte + wire bytes): a signed ContactRequest whose intro and profile
 *  are sealed to this account's X25519 key. */
export interface MailboxRow {
  readonly messageId: number;
  readonly payload: Uint8Array;
  readonly createdAt: string;
}

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

/** ADR-0050 — base64 of the person key (account key, else the legacy leaf
 *  key) of the member at `leafIndex`, or `null` on a torn read / no such leaf. */
function personKeyAtLeaf(group: WasmGroup, leafIndex: number): string | null {
  try {
    const roster = group.members() as unknown as Array<{
      leafIndex: number;
      signaturePublicKey: Uint8Array;
      accountKey?: Uint8Array;
    }>;
    const hit = roster.find((m) => m.leafIndex === leafIndex);
    return hit === undefined ? null : base64Encode(personKeyOf(hit));
  } catch {
    return null;
  }
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
/** A group created with its first members and its DS journal claimed, whose
 *  Welcome has NOT been sent ({@link ContactsClient.prepareGroupWith}). */
export interface PreparedGroup {
  readonly group: WasmGroup;
  /** The Welcome of the founding commit, byte-identical for every target. */
  readonly welcome: Uint8Array;
  /** One per added person: the account key their mailbox is named after. */
  readonly targets: readonly { readonly accountKey: Uint8Array; readonly isRecipient: boolean }[];
}

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
   * Which refusal copy the action shows (`contacts.identity_changed.*`):
   *
   *   * `false` — the key log, read to the end, shows the new key WITHOUT
   *     a reset: `unexplained`.
   *   * `null`  — everything else: `unconfirmed`.
   *
   * Never `true`: a change that IS an attested reset is accepted silently
   * (ADR-0073 R.2) and never reaches a refusal. The type keeps `boolean`
   * for the persisted log marker's shape.
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
 * ADR-0073 R.2 "no verdict" — a contact's published key differs from the
 * one this device holds, no proof came with it, and whether the change is
 * an attested reset could not be established YET (the attest key or the key
 * log could not be reached). Nothing was changed and nothing is recorded;
 * the next trigger asks again. An action that needed the new key fails like
 * any action that could not reach the server; it is not a refusal.
 */
export class IdentityCheckInconclusive extends Error {
  override name = "IdentityCheckInconclusive";
  constructor(
    /** base64 of the contact's account_id. */
    public readonly accountIdB64: string,
  ) {
    super("the contact's published identity could not be checked yet");
  }
}

/** The outcome of {@link ContactsClient.checkContactIdentity}. */
export type ContactIdentityCheck =
  /** The published key is what this device trusts: the pin, a key just
   *  TOFU-pinned, a proven successor, or an attested reset just accepted
   *  (ADR-0073 R.2). */
  | "current"
  /** The account publishes no identity bundle. */
  | "absent";

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

/** ADR-0078 — the `from_pseudonym` of an outgoing contact request: 32 fresh
 *  random bytes per request, never a function of the sender's identity key.
 *  The receiver echoes it in its ack and nothing on either side routes or
 *  matches on it. */
export function randomFromPseudonym(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(32));
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
 * ADR-0045 S4b + ADR-0050 section 2.5a item 6 — the acting-committer question
 * as a caller answers it: this device's current rank and its own leaf
 * signature key (the identity the group runs on), folded over the observed
 * beacons.
 */
export type ActingCommitterPort = (
  ownRank: number,
  ownSignatureKey: Uint8Array,
) => Promise<ActingCommitter>;

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
  actingCommitter: ActingCommitterPort,
  onSweptEvent: (event: IncomingChatEvent) => void,
): SweepPorts {
  const groupIdBytes = group.groupId;
  const groupIdB64 = base64Encode(groupIdBytes);
  // ADR-0060 — the accounts one sweep's Welcome fan-out already reached.
  const welcomedAccounts = new Map<string, number>();
  /** The leaves the staged sweep removes (set by `stagePendingProposals`). */
  let stagedRemovedLeaves = new Set<number>();
  return {
    // ADR-0050 section 2.5a item 6 — the fold is keyed with the signature key
    // of the SAME identity `ownCommitterRank` is asked with below, so the
    // rank and the key always describe one device.
    actingCommitter: (ownRank) => actingCommitter(ownRank, identity.signaturePublicKey),
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
        stagedRemovedLeaves = new Set<number>();
        for (const raw of staged.swept()) {
          const record = raw as {
            kind: string;
            addSignatureKey?: Uint8Array;
            removeTargetLeaf?: number;
            senderLeaf?: number;
          };
          if (record.kind === "add" && record.addSignatureKey !== undefined) {
            added.push(record.addSignatureKey);
          }
          // ADR-0057 — the leaves this commit removes, for the owner's
          // revocation list: a `remove` names its target, a `leave` (RFC 9420
          // SelfRemove) removes its sender.
          if (record.kind === "remove" && record.removeTargetLeaf !== undefined) {
            stagedRemovedLeaves.add(record.removeTargetLeaf);
          }
          if (record.kind === "leave" && record.senderLeaf !== undefined) {
            stagedRemovedLeaves.add(record.senderLeaf);
          }
        }
        return {
          commit: staged.commit,
          welcome: staged.welcome,
          addedSignatureKeys: added,
        };
      },
      mergeStagedSweep: () => {
        // ADR-0057 root handover - the sweep that carries this account's own
        // ownership transfer moves the role table here.
        const ownerBefore = readRoleTableOwner(group);
        group.mergeStagedSweep(identity);
        noteOwnerTransition(groupIdBytes, ownerBefore, readRoleTableOwner(group));
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
        const ownerBefore = readRoleTableOwner(group);
        const processed = group.processIncomingVerifyingRoles(identity, row.payload);
        // ADR-0057 root handover - a remote commit the verdict applies.
        noteOwnerTransition(groupIdBytes, ownerBefore, readRoleTableOwner(group));
        try {
          // The roster is read HERE, one line after the apply, because that
          // is the epoch this row decrypted at — a leaf resolved any later
          // can name whoever was added into a vacated slot since.
          const event = await toEvent(
            row.id,
            row.createdAt,
            processed,
            (leaf) => resolveMemberIdentity(rosterOf(group), leaf),
            (leaf) => resolveLeafSignatureKey(rosterOf(group), leaf),
          );
          // ADR-0057 — consumed at receipt, like the poll does, never shown.
          if (event.kind === "registrarSeed") {
            await consumeRegistrarSeed(group, event);
          } else if (event.kind === "serviceEnvelope") {
            // An unrecognised `lakko/…` service envelope: dropped, never shown.
          } else {
            onSweptEvent(event);
          }
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
      // ADR-0060 section 6 — the sweep records each added LEAF's signature key
      // (`welcomeMailbox` below); the Welcome is written, once per account,
      // to the account mailbox that leaf's binding names, read off the roster
      // after the merge. A leaf of an account already written to in this
      // sweep is skipped (its devices share one mailbox and one Welcome).
      // ADR-0078 — `bytes` is the RAW Welcome (`wrapWelcome` is the
      // identity): it is sealed here, per attempt, to the mailbox key the
      // grant used for that attempt binds.
      publishMailbox: async (leafKey, bytes) => {
        const accountKey = accountKeyOfLeaf(rosterOf(group), leafKey);
        // ADR-0074 section 3.5a rule 1 - the leaf's binding may name a proven
        // predecessor: the Welcome goes to the person's CURRENT mailbox, and
        // two leaves of one person bound under different keys share it.
        const accountHex = journalHex(resolveAccountKey(accountKey));
        const done = welcomedAccounts.get(accountHex);
        if (done !== undefined) return done;
        const explicit = joinReplyFor(leafKey);
        // ADR-0050 section 2.5a item 7 — never unsigned: every grant held for
        // the account is tried in order (next on 401); with none left nothing
        // is written, `null` tells the sweep, and the Welcome stays owed
        // (`markWelcomesOwed`, not acked) for the next poll or a grant's
        // arrival.
        let id: number;
        try {
          // ADR-0074 section 3.5a rule 6 - a refusal re-checks the address
          // once; the mailbox is the one resolved for that attempt.
          const wasm = await loadWasm();
          id = await writeWelcome(
            accountKey,
            { scope: groupIdBytes, explicit },
            async (target, auth) =>
              (await ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome", bytes), auth)).id,
          );
        } catch (err) {
          if (!(err instanceof MailboxUnreachableError)) throw err;
          // eslint-disable-next-line no-console
          console.info("ADR-0050: a Welcome is kept owed; no grant reaches the added account's mailbox");
          return null;
        }
        welcomedAccounts.set(accountHex, id);
        if (explicit !== null) forgetJoinReply(leafKey);
        return id;
      },
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
      findOwedWelcomes: async (skip) => {
        const owed = (await loadUnacked(groupIdB64)).find(
          (e) => e.sweepWelcomesOwed === true && skip?.has(e.id) !== true,
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
    // ADR-0060 — the destination recorded is the added LEAF's key; the
    // account mailbox it maps to is resolved at publish (see `publishMailbox`).
    welcomeMailbox: (signatureKey) => signatureKey,
    // ADR-0078 — the outbox keeps the RAW Welcome: a row is sealed to the
    // mailbox key the write resolves, which is only known at publish.
    wrapWelcome: (welcome) => welcome,
    // ADR-0053 contract point 3 + ADR-0057 obligation 5 — the staged epoch's
    // key, registered with this device's root or registrar key before the
    // sweep is published. The owner revokes the registrar keys of the
    // accounts this sweep evicts in the same statement; an admin cannot
    // derive them and sends none (the owner's next reconcile removes them).
    registerStagedEpoch: async () => {
      const staged = takeSigner(() => {
        const next = group.dsStagedAccessSigner(identity);
        if (next === undefined) throw new Error("no staged sweep");
        return next;
      });
      const authority = dsAccess.journalAuthority(groupIdBytes);
      // Read BEFORE the network: the backoff is keyed on the membership the
      // refusal was about.
      const fingerprint = membershipFingerprint(group);
      try {
        const evictees = evicteeAccountKeys(
          rosterOf(group) as unknown as OwnershipRosterMember[],
          stagedRemovedLeaves,
        );
        // ADR-0057 (amended 2026-10-01) item 1 - the owner derives the
        // evictees' registrar keys; an admin revokes the ones it collected
        // from the owner's envelopes, lower ranks only.
        const revoke = revocationKeys(
          authority,
          evictees,
          wasmRegistrarDerivation(wasm),
          registrarListFor(groupIdB64),
        );
        releaseAuthority(authority);
        // The DS client signs with the same authority it resolves itself.
        await registerBeforePublish(ds, groupIdBytes, staged, revoke);
        sweepBackoff.reset(groupIdB64);
      } catch (err) {
        // A refusal that will not change by the next poll: back off the next
        // sweep start (`chat/sweep-backoff.ts`). The sweep itself abandons as
        // before; nothing is published.
        if (isBackoffRefusal(err)) {
          const waitMs = sweepBackoff.noteRefused(groupIdB64, Date.now(), fingerprint);
          // eslint-disable-next-line no-console
          console.warn(`ds: staged epoch registration refused; next sweep in ${Math.round(waitMs / 1000)} s`);
        }
        throw err;
      } finally {
        releaseAuthority(authority);
        staged?.free();
      }
    },
    mayStartSweep: () =>
      sweepBackoff.mayStart(groupIdB64, Date.now(), membershipFingerprint(group)),
    mayRegister: () => dsAccess.mayRegister(groupIdBytes),
    // After a lost verdict nothing is re-registered (ADR-0057): the winner
    // registered its own epoch before publishing, and every registration is
    // an APPEND now (the second security pass), so this device's registration
    // could not have displaced the winner's - it met 409 or landed first and
    // the winner's met 409. A Replace is never sent from a commit path; a
    // journal a lower rank pushed ahead is the repair's (`ds/journal-repair`).
    beginApply: () => async () => {},
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

/**
 * The group's membership and role table as one string, for the sweep backoff:
 * a change to either ends the wait (`chat/sweep-backoff.ts`). A torn read
 * yields a value that differs from any real one, which ends a wait rather
 * than extending it.
 */
function membershipFingerprint(group: WasmGroup): string {
  try {
    const keys = (group.members() as Array<{ signaturePublicKey: Uint8Array }>)
      .map((m) => base64Encode(m.signaturePublicKey))
      .sort();
    const owner = group.roleTableOwner();
    const admins = group.roleTableAdmins();
    return [
      keys.join(","),
      owner === undefined ? "-" : base64Encode(owner),
      base64Encode(admins),
    ].join("|");
  } catch {
    return `unreadable:${Math.random()}`;
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
    // its ContactAck to a mailbox nothing reads (ADR-0078: device mailboxes
    // are gone).
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
      // ADR-0074 section 2.3 — only the devices the account's DeviceGroup
      // holds: a removed device that publishes itself again is not put into
      // a new conversation. With no roster loaded none is added here.
      const roster = ownDeviceRoster(base64Encode(secrets.accountId));
      // ADR-0074 (a device paired after a key rotation) — and only from a
      // KeyPackage bound to the account key this device's leaves are bound
      // to; a device that published before the pairing state bundle reached
      // it is added by the re-admission arm once it has published again.
      const wasm = await loadWasm();
      const conversationKeyB64 = ownPersonKeyB64(myIdentity);
      return list.filter(
        (d) =>
          !bytesEqualCc(d.deviceId, ownKey) &&
          ownDeviceAdmissible(roster, hexOfKey(d.deviceId)) &&
          ownDeviceKeyPackageUsable(wasm, d.keyPackage, conversationKeyB64),
      );
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
   * The FAIL-CLOSED persist a proposal path runs before its publish, and the
   * founding path before its Welcome leaves: {@link persistDurableOrThrow}
   * with the failure wrapped like every other refusal of these paths. A
   * proposal or a Welcome on the wire whose MLS state this device did not
   * record is the case the rule "a refused persist means nothing was
   * published" excludes.
   */
  private async persistProposalOrThrow(): Promise<void> {
    try {
      await this.persistDurableOrThrow();
    } catch (err) {
      throw ContactsError.wrap(err);
    }
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
    // ADR-0057 — the new epoch is registered with this device's root or
    // registrar key; the key of the epoch it left proves nothing any more.
    void signerN;
    const signerN1 = takeSigner(() => group.dsAccessSigner(myIdentity));
    try {
      await registerBeforePublish(this.ds, group.groupId, signerN1);
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
   * The committer set is the ADR-0022 owner plus admins — and, since ADR-0050
   * section 2.5a, a plain member while its pending store holds a device
   * admission it could sweep (its sweep then commits only device admissions
   * and its own account's self-service proposals) — ordered by
   * `ownCommitterRank()`; which of them ACTS is decided by `actingCommitter`,
   * the caller-supplied fold over the observed liveness beacons. Every device
   * of a ranked account holds the account's rank (ADR-0050 section 2.5a item
   * 6), so the fold also takes this device's own signature key: a device
   * defers to a fresh beacon of a lower rank, or of the same rank with a lower
   * key (a sibling device of the same account), and acts otherwise, including
   * when it has heard nothing at all. The owner never waits for an admin, and
   * an admin-only group never waits for an absent owner.
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
    actingCommitter: ActingCommitterPort,
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
          entry.secondArtifactAccountKeyB64 !== undefined
        ) {
          // Welcome -> the new member's mailbox (NOT this device's group
          // journal), so no echo re-arm — the committer never re-fetches it.
          // ADR-0078: the entry holds the RAW Welcome and the account key; the
          // target is resolved now (a verified binding of a grant held for
          // that account) and the row sealed to it. Signed with the key that
          // reaches the account; rule 6: re-checked once on a refusal.
          const secondBytes = base64Decode(entry.secondArtifactB64);
          const wasm = await loadWasm();
          const pub2 = await writeToPerson(
            base64Decode(entry.secondArtifactAccountKeyB64),
            { scope: groupIdBytes, allowMember: true },
            (target, auth) =>
              this.ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome", secondBytes), auth),
          );
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
    /** ADR-0073 R.2: the key this device HOLDS for the account when it
     *  never pinned it (a roster's account key for that account id). */
    heldKeyB64?: string,
  ): Promise<void> {
    // G-30 #2 (ADR-0019): snapshot the pin BEFORE gate 1 runs. Gate 1
    // re-pins to the new key on a verified rotation (overwriting the pin),
    // but gate 2's rotation-chain check needs the OLD pinned key + the
    // OLD chain-version floor to anchor the chain. Capturing it here keeps
    // gate 1 untouched.
    const oldPin = readPin(base64Encode(accountId));

    // Gate 1 (primary, fail-closed). A throw here short-circuits gate 2.
    try {
      await this.verifySupersessionGate(accountId, bundle, heldKeyB64);
    } catch (err) {
      // ADR-0073 R.2 — a CHANGED key with NO supersession proof is asked
      // whether it is an attested reset. Accepted: the re-pin is written and
      // the call proceeds as for any trusted key, with nothing shown.
      // Refused: the gate's own refusal, with the copy the verdict names.
      // No verdict: nothing changes. A change that CAME with a proof which
      // failed is never a reset (a proven rotation and a reset are two
      // paths) and stays the gate's refusal.
      if (!(err instanceof SupersessionRefused)) throw err;
      if (err.reason !== "proof-absent") {
        err.loggedReset = await this.loggedResetForRefusal(accountId, bundle);
        throw err;
      }
      // The held key is the pin, else the verified binding the gate judged
      // the change from. A roster key is never the held key by itself.
      const heldBinding = oldPin === undefined ? readVerifiedBinding(base64Encode(accountId)) : undefined;
      const verdict = await this.judgeReset(
        accountId,
        bundle,
        oldPin?.pinnedSignaturePubkeyB64 ?? heldBinding?.keyB64,
        oldPin?.acceptedKeyGeneration ?? heldBinding?.keyGeneration ?? 0,
      );
      if (verdict.kind === "refused") {
        err.loggedReset = verdict.why === "unexplained" ? false : null;
        throw err;
      }
      if (verdict.kind === "no-verdict") {
        throw new IdentityCheckInconclusive(base64Encode(accountId));
      }
      // Accepted and re-pinned: gate 2 below already ran inside the verdict.
      return;
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
   * ADR-0073 R.2 — fetch an account's published identity and run the
   * fail-closed check. One of the rule's triggers: the scheduled per-chat
   * device check, opening a one-to-one, a send whose last check is old.
   *
   * @param heldKeyB64 the roster's account key of a group member this device
   *   never pinned. Ignored when a pin exists. It is NOT taken as the held
   *   key on the caller's word (the account id may be a member's own claim):
   *   `verified-binding.ts` decides. The first time the directory serves
   *   exactly this key a verified binding is recorded; a change is judged
   *   only from such a binding; anything else is
   *   `IdentityCheckInconclusive` and nothing is written.
   * @throws `SupersessionRefused` for a change that is neither proven nor an
   *   attested reset; `IdentityCheckInconclusive` when that could not be
   *   established yet; a rollback and a network failure propagate too.
   */
  async checkContactIdentity(
    accountId: Uint8Array,
    heldKeyB64?: string,
  ): Promise<ContactIdentityCheck> {
    let bundle: IdentityFetchResponse | null;
    try {
      bundle = await this.discovery.identityFetch(accountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (bundle === null) return "absent";
    await this.verifyBundleAccountKey(accountId, bundle, heldKeyB64);
    return "current";
  }

  /**
   * A contact request sent to keys the caller already holds (an invite
   * link's capability, a search result's cross-checked bundle), where no
   * identity fetch runs in the send itself. Call it BEFORE anything is
   * spent or sent.
   *
   * - no pin for the account (a stranger): passes;
   * - a pinned account under a key that is not the pin: the published
   *   identity is fetched and run through the fail-closed gate (an attested
   *   reset is accepted there, silently; an unproven change is the gate's
   *   own `SupersessionRefused`); if the gate passes and the key is STILL
   *   neither the pin nor a proven predecessor of it (e.g. an old link
   *   naming a superseded key), the send is refused as a key mismatch. Old
   *   keys do not come back.
   */
  async refuseContactRequestToUnverifiedKeys(
    accountId: Uint8Array,
    recipientSignaturePubkey: Uint8Array,
  ): Promise<void> {
    const accountIdB64 = base64Encode(accountId);
    const recipientKeyB64 = base64Encode(recipientSignaturePubkey);
    if (contactRequestGate(readPin(accountIdB64), recipientKeyB64) === "send") return;
    await this.checkContactIdentity(accountId);
    // ADR-0074 section 3.5a rule 1 - a key that is a PROVEN predecessor of
    // the pin is the same person: the send re-fetches and seals to the
    // current keys (`currentRequestTarget`). Any other key is refused.
    const pinnedNow = readPin(accountIdB64);
    const provenPredecessor =
      pinnedNow !== undefined &&
      base64Encode(resolveAccountKey(recipientSignaturePubkey)) === pinnedNow.pinnedSignaturePubkeyB64;
    if (!provenPredecessor && contactRequestGate(pinnedNow, recipientKeyB64) !== "send") {
      throw new SupersessionRefused(
        "key-mismatch",
        accountIdB64,
        "the key this request would be sealed for is not the account's trusted key",
      );
    }
  }

  /**
   * ADR-0074 section 3.5a rule 1 - the keys a contact request to a bundle in
   * hand actually goes out to.
   *
   * A bundle carried by a link or a search result may name a key its owner
   * has since rotated away BY PROOF. The request is then sealed to the
   * CURRENT signature and X25519 keys, which only the published bundle
   * carries, and sent to the current mailbox. The fetch goes through the
   * fail-closed gate; a failed fetch REFUSES the send, and the old key is
   * never the fallback. A key that is neither the published one nor a
   * proven predecessor of it is refused as a key mismatch.
   *
   * No fetch runs when the key in hand is already what this device trusts
   * (no pin and no proven successor known, or the pin itself).
   */
  private async currentRequestTarget(
    accountId: Uint8Array,
    recipientSignaturePubkey: Uint8Array,
    recipientX25519Pubkey: Uint8Array,
  ): Promise<{ signaturePubkey: Uint8Array; x25519Pubkey: Uint8Array }> {
    const accountIdB64 = base64Encode(accountId);
    const suppliedKeyB64 = base64Encode(recipientSignaturePubkey);
    const supplied = { signaturePubkey: recipientSignaturePubkey, x25519Pubkey: recipientX25519Pubkey };
    const verdict = contactRequestGate(readPin(accountIdB64), suppliedKeyB64);
    const resolvesToItself =
      base64Encode(resolveAccountKey(recipientSignaturePubkey)) === suppliedKeyB64;
    if (verdict === "send" && resolvesToItself) return supplied;
    let bundle: IdentityFetchResponse | null;
    try {
      bundle = await this.discovery.identityFetch(accountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (bundle === null) throw ContactsError.noIdentityBundle(accountId);
    await this.verifyBundleAccountKey(accountId, bundle);
    const published = base64Decode(bundle.signature_pubkey);
    const target = requestTargetAfterFetch({
      suppliedKeyB64,
      publishedKeyB64: bundle.signature_pubkey,
      publishedIsProvenSuccessor: await provenSuccessorWith(
        recipientSignaturePubkey,
        published,
        bundle,
      ),
    });
    if (target === "as-supplied") return supplied;
    if (target === "current") {
      return { signaturePubkey: published, x25519Pubkey: base64Decode(bundle.x25519_pubkey) };
    }
    throw new SupersessionRefused(
      "key-mismatch",
      accountIdB64,
      "the key this request would be sealed for is not the account's trusted key",
    );
  }

  /**
   * ADR-0074 section 3.5a rule 6 - re-check the address of the person
   * `accountKey` names after a refused write: fetch their identity bundle
   * and run it through the fail-closed gate, which records a proven
   * predecessor (so the next resolution addresses the current mailbox).
   *
   * Only for a person a TRUSTED local record names (a pin, an accepted
   * contact); a claimed account id is not followed. Never throws: a failed
   * fetch or a refusal of the gate leaves the address as it was.
   */
  async recheckAddress(accountKey: Uint8Array): Promise<void> {
    const accountIdB64 = accountIdB64OfKey(base64Encode(accountKey));
    if (accountIdB64 === null) return;
    // At most one re-check per person in five minutes, on every client: a
    // run of refused writes to one person costs one directory fetch.
    const nowMs = Date.now();
    if (!addressRecheckAllowed(lastAddressRecheckAtMs.get(accountIdB64) ?? null, nowMs)) return;
    lastAddressRecheckAtMs.set(accountIdB64, nowMs);
    try {
      const accountId = base64Decode(accountIdB64);
      const bundle = await this.discovery.identityFetch(accountId);
      if (bundle === null) return;
      await this.verifyBundleAccountKey(accountId, bundle);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.info(
        "ContactsClient: address re-check changed nothing",
        err instanceof Error ? err.name : "",
      );
    }
  }

  /**
   * ADR-0074 section 3.5a rule 5 - send an unanswered contact request AGAIN
   * after this account's keys were rotated: a fresh request from the new
   * key, with a new `reply` grant, sealed to the recipient's published keys
   * (fetched through the fail-closed gate). It carries the profile and the
   * intro this device kept for the request, when it kept one.
   *
   * Never throws: answers how it ended (`rotation-spent.ts`). Only a server
   * that could not be reached is `unreachable`.
   */
  async resendContactRequestAfterRotation(
    identity: IdentitySecrets | null,
    recipientAccountId: Uint8Array,
    /** The intro the first request carried, as this device kept it
     *  (`sent-request-intros.ts`); empty when it kept none. */
    introMessage: Uint8Array = new Uint8Array(0),
  ): Promise<SpentOutcome> {
    try {
      const bundle = await this.discovery.identityFetch(recipientAccountId);
      if (bundle === null) return "skipped";
      await this.verifyBundleAccountKey(recipientAccountId, bundle);
      await this.composeAndPublishContactRequest(
        identity,
        base64Decode(bundle.signature_pubkey),
        base64Decode(bundle.x25519_pubkey),
        introMessage,
      );
      return "done";
    } catch (err) {
      return spentFailureOutcome(err);
    }
  }

  /**
   * ADR-0074 section 3.5a rule 5 - one pending contact request per person.
   *
   * A person whose account keys were rotated re-sends their unanswered
   * request from the new key. Given a request just received and the pending
   * list, answer which pending requests it replaces and whether it should
   * itself be dropped ({@link pendingAfterArrival}). Only requests naming
   * the SAME account id are compared, and the account id alone decides
   * nothing (it is the sender's claim): the new key must be the PROVEN
   * successor of the old one, by the pin store's chain or by the proof the
   * account's published bundle carries (`proven-successor.ts`).
   *
   * Never throws; a failed fetch replaces nothing (two rows, as before).
   */
  async pendingRequestsSupersededBy(
    request: IncomingContactRequest,
    pending: readonly IncomingContactRequest[],
  ): Promise<{ readonly replaces: number[]; readonly dropIncoming: boolean }> {
    const none = { replaces: [], dropIncoming: false };
    const accountId = request.fromAccountId;
    if (accountId === null) return none;
    const incomingKeyB64 = base64Encode(request.fromPubkey);
    const accountHex = journalHex(accountId);
    const candidates = pending.filter(
      (p) =>
        p.fromAccountId !== null &&
        journalHex(p.fromAccountId) === accountHex &&
        base64Encode(p.fromPubkey) !== incomingKeyB64,
    );
    if (candidates.length === 0) return none;
    try {
      let bundle: IdentityFetchResponse | null = null;
      try {
        bundle = await this.discovery.identityFetch(accountId);
      } catch {
        bundle = null;
      }
      const successors = new Set<string>();
      for (const p of candidates) {
        const otherKeyB64 = base64Encode(p.fromPubkey);
        if (await provenSuccessorWith(p.fromPubkey, request.fromPubkey, bundle)) {
          successors.add(`${otherKeyB64}>${incomingKeyB64}`);
        } else if (await provenSuccessorWith(request.fromPubkey, p.fromPubkey, bundle)) {
          successors.add(`${incomingKeyB64}>${otherKeyB64}`);
        }
      }
      return pendingAfterArrival(
        candidates.map((p) => ({ messageId: p.messageId, keyB64: base64Encode(p.fromPubkey) })),
        incomingKeyB64,
        (oldKey, newKey) => successors.has(`${oldKey}>${newKey}`),
      );
    } catch {
      return none;
    }
  }

  /**
   * ADR-0073 R.2 — gather the rule's inputs for a proof-less key change and
   * act on the verdict (`reset-verdict.ts`). On `accepted` the re-pin is
   * written and the listeners that move the shared conversations are told;
   * nothing is shown anywhere. Never throws.
   */
  private async judgeReset(
    accountId: Uint8Array,
    bundle: IdentityFetchResponse,
    heldKeyB64: string | undefined,
    /** The generation last accepted for the account: the pin's, else the
     *  verified binding's baseline. */
    acceptedGeneration: number,
  ): Promise<ReturnType<typeof resetVerdict>> {
    const accountIdB64 = base64Encode(accountId);
    if (heldKeyB64 === undefined) return { kind: "no-verdict" };
    let servedPubkey: Uint8Array;
    try {
      servedPubkey = base64Decode(bundle.signature_pubkey);
    } catch {
      return { kind: "refused", why: "unconfirmed" };
    }
    const attest = await this.checkResetAttestation(accountId, servedPubkey, bundle);
    let log: LogCheck;
    try {
      const r = await verifyIdentityInLog(accountId, servedPubkey);
      log = { status: r.status, isReset: r.isReset };
    } catch {
      log = { status: "unreachable", isReset: null };
    }
    const verdict = resetVerdict({
      heldKeyB64,
      acceptedGeneration,
      fetchedKeyB64: bundle.signature_pubkey,
      bundleGeneration: bundle.key_generation,
      attest,
      log,
    });
    if (verdict.kind !== "accepted") return verdict;
    const written = acceptReset({
      accountIdB64,
      heldKeyB64,
      newKeyB64: bundle.signature_pubkey,
      newX25519B64: bundle.x25519_pubkey,
      keyGeneration: verdict.keyGeneration,
      signedAtMs: verdict.signedAtMs,
      nowMs: Date.now(),
    });
    if (written === "stale" || written === "no-account") {
      return { kind: "refused", why: "unconfirmed" };
    }
    // The account is pinned now: the pin is the verified binding.
    dropVerifiedBinding(accountIdB64);
    recordLogStatus(accountIdB64, log.status);
    recordLogReset(accountIdB64, log.isReset);
    if (written === "accepted") {
      notifyResetAccepted({
        accountIdB64,
        previousKeyB64: heldKeyB64,
        newKeyB64: bundle.signature_pubkey,
        keyGeneration: verdict.keyGeneration,
        signedAtMs: verdict.signedAtMs,
      });
    }
    return verdict;
  }

  /** R.2 step 1: the bundle's attestation under the trusted attest key (the
   *  build-time pin when set, else the cached JWKS key). */
  private async checkResetAttestation(
    accountId: Uint8Array,
    servedPubkey: Uint8Array,
    bundle: IdentityFetchResponse,
  ): Promise<AttestCheck> {
    const raw = bundle.key_reset_attest;
    if (raw === undefined || raw.length === 0) return { kind: "absent" };
    let attestation: Uint8Array;
    try {
      attestation = base64Decode(raw);
    } catch {
      return { kind: "invalid" };
    }
    let attestKey: Uint8Array;
    try {
      attestKey = await fetchDeviceAttestKey();
    } catch {
      return { kind: "no-key" };
    }
    try {
      const wasm = await loadWasm();
      const verified = wasm.verifyKeyResetAttestation(attestation, attestKey, accountId, servedPubkey);
      const keyGeneration = Number(verified.keyGeneration);
      const signedAtMs = Number(verified.signedAtMillis);
      try {
        verified.free();
      } catch {
        // Freeing is best-effort.
      }
      return { kind: "verified", keyGeneration, signedAtMs };
    } catch {
      return { kind: "invalid" };
    }
  }

  /**
   * The copy marker for a refusal that is NOT a reset question (a proof
   * that failed, a proof for another key): `false` only when the key log,
   * read through the VERIFIED path, shows the served key without a reset.
   * Never throws.
   */
  private async loggedResetForRefusal(
    accountId: Uint8Array,
    bundle: IdentityFetchResponse,
  ): Promise<boolean | null> {
    try {
      const r = await verifyIdentityInLog(accountId, base64Decode(bundle.signature_pubkey));
      return r.status === "confirmed" && r.isReset === false ? false : null;
    } catch {
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
    /** ADR-0073 R.2: with no pin, the key a roster names for this account
     *  id is what the device HOLDS: a fetched key that differs from it is a
     *  change, not a first observation. */
    heldKeyB64?: string,
  ): Promise<void> {
    const accountIdB64 = base64Encode(accountId);
    const fetchedPubkeyB64 = bundle.signature_pubkey;

    const pinned = readPin(accountIdB64);
    // With no pin, what this device HOLDS for the account is a VERIFIED
    // BINDING (`verified-binding.ts`), never the roster key on the strength
    // of a claimed account id: a member's own profile can name any id.
    const binding = pinned === undefined ? readVerifiedBinding(accountIdB64) : undefined;
    const bindingAsPin = (b: {
      readonly keyB64: string;
      readonly keyGeneration: number;
    }): Pick<AccountPubkeyPin, "pinnedSignaturePubkeyB64" | "rotationMarker"> & Partial<AccountPubkeyPin> => ({
      pinnedSignaturePubkeyB64: b.keyB64,
      rotationMarker: 0,
      acceptedKeyGeneration: b.keyGeneration,
    });
    let existing:
      | (Pick<AccountPubkeyPin, "pinnedSignaturePubkeyB64" | "rotationMarker"> & Partial<AccountPubkeyPin>)
      | undefined;
    if (pinned !== undefined) {
      existing = pinned;
    } else if (heldKeyB64 !== undefined) {
      // The member path: a roster key under an account id this device never
      // pinned. Nothing is pinned here.
      const held = memberHeldKey({
        pinned: false,
        binding,
        rosterKeyB64: heldKeyB64,
        fetchedKeyB64: fetchedPubkeyB64,
      });
      switch (held.kind) {
        case "record":
          // The directory serves exactly the roster key: this device has now
          // seen the binding for itself. A first observation, never a reset.
          recordVerifiedBinding(accountIdB64, {
            keyB64: fetchedPubkeyB64,
            keyGeneration: Number.isSafeInteger(bundle.key_generation) ? bundle.key_generation : 0,
            verifiedAtMs: Date.now(),
          });
          return;
        case "confirmed":
          return;
        case "held":
          existing = bindingAsPin({ keyB64: held.heldKeyB64, keyGeneration: held.acceptedGeneration });
          break;
        case "pin":
        case "none":
          // Nothing is established for this roster key under this account
          // id (a claimed id, or a first sight of an account that already
          // moved on): no verdict, nothing done, nothing shown.
          throw new IdentityCheckInconclusive(accountIdB64);
      }
    } else if (binding !== undefined) {
      // A caller with no roster key (a contact request, a search result):
      // the binding this device verified earlier stands in for the pin, so
      // a key that changed since is a CHANGE, not a first observation.
      existing = bindingAsPin(binding);
    }

    // Case 1 — first-ever observation. TOFU-pin and proceed. The bundle's
    // generation is recorded as the accepted-generation baseline (R.2).
    if (existing === undefined) {
      setPin(accountIdB64, fetchedPubkeyB64, 0, undefined, { keyGeneration: bundle.key_generation });
      return;
    }

    // Case 2 — unchanged key. Nothing to verify. A pin from before the
    // generation was recorded takes its baseline now; a verified binding
    // met on the contact path becomes the pin, keeping its baseline.
    if (existing.pinnedSignaturePubkeyB64 === fetchedPubkeyB64) {
      if (pinned === undefined) {
        setPin(accountIdB64, fetchedPubkeyB64, 0, undefined, {
          keyGeneration: existing.acceptedKeyGeneration ?? bundle.key_generation,
        });
        dropVerifiedBinding(accountIdB64);
      } else if (existing.acceptedKeyGeneration === undefined) {
        setPin(accountIdB64, fetchedPubkeyB64, existing.rotationMarker, undefined, {
          keyGeneration: bundle.key_generation,
        });
      }
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
    // ADR-0074 section 3.5: the proof verified under the pinned key for
    // exactly this key, so the replaced key is a PROVEN predecessor: the
    // leaves bound to it stay the same person. Nothing is flagged, closed or
    // evicted, and nothing is shown. An attestation beside the proof (the
    // proven-rotation arm) does not change this: the proof is what tells a
    // rotation from a reset.
    if (pinned === undefined) {
      // The change was judged from a verified binding: the binding's key
      // becomes the pin first, so the proven re-pin below records it as the
      // proven predecessor (the member's leaves stay the same person).
      setPin(accountIdB64, existing.pinnedSignaturePubkeyB64, 0, undefined, {
        ...(existing.acceptedKeyGeneration !== undefined
          ? { keyGeneration: existing.acceptedKeyGeneration }
          : {}),
      });
      dropVerifiedBinding(accountIdB64);
    }
    const result = setPin(accountIdB64, fetchedPubkeyB64, proofExpiresAt, undefined, {
      proven: true,
    });
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
    options?: ContactRequestOptions,
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
    // ADR-0073 R.2: a proof-less change that is an attested reset is
    // accepted inside the gate, silently.
    await this.verifyBundleAccountKey(recipientAccountId, bundle);
    const recipientSignaturePub = base64Decode(bundle.signature_pubkey);
    const recipientX25519Pub = base64Decode(bundle.x25519_pubkey);
    this.reportUsernameResolved(recipientSignaturePub, lookup.normalisedUsername);

    // ADR-0078 - a person in Everyone: their public record's mailbox binding
    // is where an unsigned request goes (verified against the bundle key
    // before the write; a binding signed by another key reaches nothing).
    const publish = await this.composeAndPublishContactRequest(
      identity,
      recipientSignaturePub,
      recipientX25519Pub,
      introMessage,
      { ...options, openBinding: options?.openBinding ?? lookup.mailboxBinding ?? null },
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
    options?: ContactRequestOptions,
  ): Promise<SendResult> {
    // ADR-0073 R.2: no identity fetch runs on this path, so a key that is
    // not the stored pin sends the published identity through the gate
    // first (a stranger has no pin and passes).
    await this.refuseContactRequestToUnverifiedKeys(recipientAccountId, recipientSignaturePubkey);
    // ADR-0074 section 3.5a rule 1 - a key that is a proven predecessor is
    // replaced by the account's current keys, fetched through the gate.
    const target = await this.currentRequestTarget(
      recipientAccountId,
      recipientSignaturePubkey,
      recipientX25519Pubkey,
    );
    const publish = await this.composeAndPublishContactRequest(
      identity,
      target.signaturePubkey,
      target.x25519Pubkey,
      introMessage,
      options,
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
    options?: ContactRequestOptions,
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

    // ADR-0078: the recipient's mailbox is the one a binding (a grant's,
    // or the public record's for a person in Everyone) names, verified
    // against the recipient key in hand; no address is derived from it.
    //
    // ADR-0074 section 3.5a rule 1 - resolved first. A request is sealed to
    // the X25519 key the caller supplied, which belongs to the key it
    // supplied: when that key is a PROVEN predecessor of an account this
    // device has pinned at a newer key, the bundle (or the link) is stale
    // and a request sealed to it would be readable by the holder of the old
    // seed. Refused; the caller fetches the current bundle.
    if (
      journalHex(resolveAccountKey(recipientSignaturePubkey)) !== journalHex(recipientSignaturePubkey)
    ) {
      throw ContactsError.wrap(
        new Error("contact request: the recipient key was superseded by a proven key rotation"),
      );
    }
    // Sender's `from_pseudonym` field: 32 fresh random bytes per request
    // (ADR-0078). It used to be the sender's identity pseudonym, a public
    // function of the identity key; nothing routes or matches on it any
    // more (the ack goes to the mailbox the reply grant's binding names,
    // and an ack is paired by `accepterPubkey`, not by `ackToPseudonym`),
    // so a value derived from the key would only be a linkable handle.
    const senderPseudonym = randomFromPseudonym();

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
    // ADR-0060 section 6 — our `reply` grant rides the sealed request, so the
    // accept (the 1:1 Welcome) and the ContactAck can land in our closed
    // mailbox. Registered (7-day expiry) before the request is sent: a request
    // whose answer could not arrive must not go out. No session (a signed-out
    // edge) sends none, and the answer then lands only if we are in Everyone.
    const session = reachSession();
    let reply: { scopeHex: string; grant: WasmReachGrant } | null = null;
    if (session !== null) {
      try {
        reply = await session.newReply("contact-request", journalHex(recipientSignaturePubkey));
      } catch (err) {
        throw ContactsError.wrap(err);
      }
    }
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
        reply?.grant ?? undefined,
        // ADR-0079 section 2 — two empty arrays mean "no link answer".
        options?.linkAnswer?.linkId ?? new Uint8Array(0),
        options?.linkAnswer?.linkSecret ?? new Uint8Array(0),
        introMessage,
      );
    } catch (err) {
      throw ContactsError.wrap(err);
    } finally {
      // `ContactRequest.compose` takes the grant by value: after a compose the
      // handle is moved and freeing it again throws (which, from a `finally`,
      // would replace the result). Free it quietly.
      try {
        reply?.grant.free();
      } catch {
        /* moved into the request */
      }
    }

    // ADR-0078: sealed as a ContactRequest row (the kind rides inside the
    // seal) to the mailbox key the grant used for the attempt binds. ADR-0060:
    // signed with the grant that lets us reach the recipient (a link's, or a
    // kept one); unsigned (one `mailbox-write` token) only to the mailbox the
    // public record of a person in Everyone binds.
    // ADR-0074 section 3.5a rule 6 - a refused write re-checks the address
    // once. The request is SEALED to the key in hand, so every binding is
    // verified against exactly that key (`exactKey`): should it turn out to
    // be superseded, no binding of the successor's is accepted and the write
    // is not moved to another mailbox.
    let publish;
    try {
      publish = await writeToPerson(
        recipientSignaturePubkey,
        {
          explicit: options?.writeGrant ?? null,
          openBinding: options?.openBinding ?? null,
          exactKey: true,
        },
        (target, auth) =>
          this.ds.publishMailbox(target.address, sealRowFor(wasm, target, "contact-request", wireBytes), auth),
      );
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
   * request's fromPseudonym`. The publish target is the requester's
   * mailbox the request's `reply` grant binds (ADR-0078: verified by
   * the core against `fromPubkey` when the request was opened, and again
   * here before the write), sealed as a ContactAck row.
   *
   * **Threat-model note:** the ack carries only public material
   * (pubkeys + account_id + a self-attested timestamp). The
   * Ed25519 signature proves "this came from `accepterPubkey`".
   * `ackToPseudonym` is the request's random per-request value
   * (ADR-0078) and is not matched on receive.
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

    // ADR-0078: the requester's mailbox is the one their `reply` grant's
    // binding names, verified against `fromPubkey`.
    // ADR-0074 section 3.5a rule 1 - resolved: an ack never goes to the
    // mailbox of a key a proven rotation superseded.
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

    // ADR-0060 section 6 — written with the requester's `reply` grant.
    // Rule 6: a refusal re-checks the requester's address once.
    let publish;
    try {
      publish = await writeToPerson(
        originalRequest.fromPubkey,
        { explicit: originalRequest.replyGrantB64 },
        (target, auth) =>
          this.ds.publishMailbox(target.address, sealRowFor(wasm, target, "contact-ack", wireBytes), auth),
      );
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
   * Routing target: the recipient's account mailbox, as a grant held for
   * them binds it (ADR-0078) — the receiver polls one place, opens every
   * sealed row and dispatches by the kind inside it.
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
   *                                  upload bundle. Every
   *                                  grant's mailbox binding
   *                                  is verified against it.
   * @param welcomeBytes              `welcome` field of the
   *                                  `AddMember` result.
   */
  async sendWelcome(
    recipientAccountKey: Uint8Array,
    welcomeBytes: Uint8Array,
    options: {
      /** The group the Welcome is into: prefers that scope's kept grant and
       *  allows the person's `member` key for it. */
      readonly groupId?: Uint8Array;
      /** A grant held for exactly this write (the requester's `reply` grant
       *  when accepting their contact request), as a kept wire. */
      readonly explicitGrantB64?: string | null;
    } = {},
  ): Promise<{ messageId: number; createdAt: string }> {
    // ADR-0060 section 6 — one inbound mailbox per person: a Welcome goes to
    // the added leaf's ACCOUNT mailbox, signed with the key that lets us
    // reach that account.
    // ADR-0074 section 3.5a rule 1 - the CURRENT mailbox of that account
    // (the leaf may be bound to a proven predecessor; own account included).
    // Rule 6: a refused write re-checks the address once and is retried.
    const wasm = await loadWasm();
    let publish;
    try {
      publish = await writeToPerson(
        recipientAccountKey,
        {
          ...(options.groupId === undefined ? {} : { scope: options.groupId }),
          explicit: options.explicitGrantB64 ?? null,
          // `allowMember` marks a Welcome. ADR-0073 R.4's continuity key is a
          // recorded no-op under ADR-0078 (no carrier names a reset account's
          // new mailbox); see `planWriteToPerson`.
          allowMember: true,
        },
        (target, auth) =>
          this.ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome", welcomeBytes), auth),
      );
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
   *                   the account's MAILBOX key (ADR-0078,
   *                   `MailboxKey.fromAccountIdentity`), whose
   *                   address is read and which opens every sealed
   *                   row, and the X25519 secret needed to unseal
   *                   each contact request's inner seal.
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
   * a row that does not open under the mailbox key (ADR-0078:
   * garbage, a row for another mailbox, an unknown kind) is
   * dropped unlogged; the verifier rejects bad signatures,
   * decoding errors are logged via `console.warn` and skipped. We don't surface a
   * "garbage on the wire" error to the UI because every honest
   * client just wants to keep moving past corrupted entries.
   */
  async pollMailbox(
    identity: IdentitySecrets | null,
    after: number,
    waitSecs = 0,
    signal?: AbortSignal,
    options: {
      /** ADR-0074 section 3.5a rules 4 and 5 - a PREDECESSOR mailbox
       *  delivers Welcomes only: a request or an ack found there is neither
       *  opened nor acted on (the cursor still advances). */
      readonly welcomesOnly?: boolean;
    } = {},
  ): Promise<PollResult> {
    const wasm = await loadWasm();
    const myIdentity = await this.resolveAccountIdentity(identity);
    // ADR-0078 - the account's mailbox key (the predecessor's, for a
    // predecessor mailbox: `resolveAccountIdentity` gives that identity).
    const mailboxKey = wasm.MailboxKey.fromAccountIdentity(myIdentity);
    const myX25519Secret = myIdentity.x25519SecretKey;

    try {
      let raw;
      try {
        raw = await this.ds.fetchMailbox(mailboxKey.address(), {
          after,
          waitSecs,
          ...(signal !== undefined ? { signal } : {}),
        });
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      const dispatched = dispatchMailboxRows(wasm, mailboxKey, raw.messages, after, myX25519Secret, options);
      return { ...dispatched, hasMore: raw.hasMore };
    } finally {
      try {
        mailboxKey.free();
      } catch {
        /* already freed */
      }
    }
  }

  /**
   * Rebuild pending contact requests from account-mailbox rows kept on this
   * device (`contacts/inbox-store.ts`), through the same verify-and-open path
   * `pollMailbox` runs. One result per row, in order; `null` for a row that no
   * longer decodes as a verified ContactRequest (the caller drops it).
   *
   * ADR-0078: a kept row holds the OPENED request body (the signed
   * ContactRequest wire, its intro and profile still sealed to this
   * account's X25519 key), not the sealed mailbox row.
   */
  async decodePendingRequestRows(
    identity: IdentitySecrets | null,
    rows: readonly MailboxRow[],
  ): Promise<(IncomingContactRequest | null)[]> {
    if (rows.length === 0) return [];
    const wasm = await loadWasm();
    const myIdentity = await this.resolveAccountIdentity(identity);
    const x25519Secret = myIdentity.x25519SecretKey;
    return rows.map((row) =>
      decodeContactRequestBody(wasm, row.messageId, row.payload, row.createdAt, x25519Secret),
    );
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
  async prepareGroupWith(
    identity: IdentitySecrets | null,
    recipientAccountId: Uint8Array,
    kind: ConversationKind,
    /** The account key the caller expects the person to have (a contact
     *  row's, a contact request's, a looked-up bundle's). Without one (a
     *  successor build names the account only) the keys this device trusts
     *  for the account id decide. Either way only a device bound to an
     *  accepted key founds the conversation (`add-member-key.ts`). */
    expectedSignaturePubkey?: Uint8Array,
  ): Promise<PreparedGroup> {
    const wasm = await loadWasm();

    // 0. ADR-0073 R.2: starting a chat is an identity fetch that runs the
    //    fail-closed check. An attested reset is accepted here, silently;
    //    an unproven change is refused before a KeyPackage is fetched.
    await this.checkContactIdentity(recipientAccountId);

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
    // ADR-0073 R.2 - the same filter as every other add path: only a device
    // bound to the expected key or a proven predecessor of it (with no
    // expected key: to a key this device trusts for the account after the
    // gate above), and never to a key a reset retired, founds the
    // conversation.
    recipientDevices = await this.devicesOfAccount(
      recipientAccountId,
      recipientDevices,
      expectedSignaturePubkey === undefined ? null : base64Encode(expectedSignaturePubkey),
    );
    if (recipientDevices.length === 0) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }

    // 2. Resolve our DEVICE identity (ADR-0050 — the leaf this device holds
    //    in every group; same instance every Group on this device shares, so
    //    mutations to its provider survive across calls and get snapshotted
    //    to IDB after this method returns).
    const myIdentity = await this.resolveIdentity(identity);
    // ADR-0062 — the kind is fixed here, in the GroupContext, and never
    // changes: a contact's conversation is a one-to-one, the new-group flow
    // passes `group` even while it holds two people.
    const group = new wasm.Group(
      myIdentity,
      kind === "chat" ? wasm.GroupKind.OneToOne : wasm.GroupKind.Group,
    );

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
    // ADR-0057 obligation 1 — the owner claims the journal at creation with
    // its ROOT (derived from the account identity), at the epoch the one
    // commit above moved it to (nobody ever holds epoch 0: the commit fans out
    // to nobody), naming that epoch's access key. FAIL-CLOSED (the second
    // security pass, finding 3): the claim must answer 204 before any Welcome
    // leaves, else whoever claimed the id first would be its root for good. A
    // failure aborts the creation with nothing published; the local group is
    // dropped (its random id is never reused).
    try {
      const founding = takeSigner(() => group.dsAccessSigner(myIdentity));
      if (founding === null) throw new Error("ds: the founding epoch's key could not be derived");
      let account: WasmIdentity | null = null;
      try {
        account = await this.resolveAccountIdentity(identity);
        const root = wasm.dsJournalRootSigner(account, group.groupId);
        try {
          await claimAtCreation(this.ds, group.groupId, root, founding.epoch, founding.verifyingKey());
        } finally {
          root.free();
        }
      } finally {
        founding.free();
        if (account !== null && account !== this.accountIdentityProvider?.()) {
          try {
            account.free();
          } catch {
            /* already freed */
          }
        }
      }
      dsAccess.markRootHeld(group.groupId);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ds: claim at group creation failed; the group was not created", err);
      try {
        group.free();
      } catch {
        /* already freed */
      }
      throw ContactsError.wrap(err instanceof JournalClaimFailedError ? err.cause : err);
    }
    // One Welcome target per PERSON (a KeyPackage bound to a proven
    // predecessor names the same mailbox as one bound to the current key).
    const targets: { accountKey: Uint8Array; isRecipient: boolean }[] = [];
    const seen = new Set<string>();
    for (const dev of devices) {
      const accountKey = await accountKeyOfKeyPackage(dev.keyPackage);
      const accountHex = journalHex(resolveAccountKey(accountKey));
      if (seen.has(accountHex)) continue;
      seen.add(accountHex);
      // By the device list this call fetched, not by a caller's key: after
      // an accepted reset (ADR-0073 R.2) a caller may still hold the old one.
      targets.push({
        accountKey,
        isRecipient: recipientDevices.some((d) => d.keyPackage === dev.keyPackage),
      });
    }
    // The creation mutated the canonical provider's storage (the group's
    // epoch state, the leaf private key, the queued Welcome state). Snapshot
    // it so a reload can `Group::load` this group id. FAIL-CLOSED: the
    // Welcome is returned only once the founder's state is on disk. A
    // Welcome delivered for a group the founder did not record would put
    // the others in a group whose founder (after a reload) is not in it, and
    // a persist refused because the account's stores were sealed by Forget
    // device (or the session ended) would otherwise be read as success. The
    // claimed journal id is never reused.
    try {
      await this.persistProposalOrThrow();
    } catch (err) {
      try {
        group.free();
      } catch {
        /* already freed */
      }
      throw err;
    }
    return { group, welcome: added.welcome, targets };
  }

  /**
   * Deliver the Welcome of a group prepared by {@link prepareGroupWith}: one
   * write per added ACCOUNT, to its account mailbox (ADR-0060 section 6).
   * The recipient's is written with `replyGrantB64` when this answers their
   * contact request; a Welcome to the new identity of an accepted reset is
   * signed with the continuity key (ADR-0073 R.4). Idempotent on the wire: a
   * second delivery is a duplicate the recipient's join ignores.
   */
  async deliverPreparedWelcomes(
    groupId: Uint8Array,
    welcome: Uint8Array,
    targets: readonly { readonly accountKey: Uint8Array; readonly isRecipient: boolean }[],
    options: { readonly replyGrantB64?: string | null } = {},
  ): Promise<{ welcomeMessageId: number }> {
    let firstWelcomeMessageId = 0;
    for (const target of targets) {
      let publish;
      try {
        publish = await this.sendWelcome(target.accountKey, welcome, {
          groupId,
          explicitGrantB64: target.isRecipient ? (options.replyGrantB64 ?? null) : null,
        });
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      if (firstWelcomeMessageId === 0) firstWelcomeMessageId = publish.messageId;
    }
    return { welcomeMessageId: firstWelcomeMessageId };
  }

  /**
   * Create a group with one other person and deliver its Welcome:
   * {@link prepareGroupWith} then {@link deliverPreparedWelcomes}.
   */
  async startGroupWith(
    identity: IdentitySecrets | null,
    recipientAccountId: Uint8Array,
    recipientSignaturePubkey: Uint8Array,
    kind: ConversationKind,
    options: {
      /** ADR-0060 — the requester's `reply` grant when this accepts their
       *  contact request: the 1:1 Welcome is written with it. */
      readonly replyGrantB64?: string | null;
    } = {},
  ): Promise<{ group: WasmGroup; welcomeMessageId: number }> {
    const prepared = await this.prepareGroupWith(
      identity,
      recipientAccountId,
      kind,
      recipientSignaturePubkey,
    );
    const delivered = await this.deliverPreparedWelcomes(
      prepared.group.groupId,
      prepared.welcome,
      prepared.targets,
      options,
    );
    await this.persistBestEffort();
    return { group: prepared.group, welcomeMessageId: delivered.welcomeMessageId };
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
      "chat",
    );
    return {
      group: out.group,
      recipientUsername: lookup.normalisedUsername,
      recipientAccountId: lookup.account,
      welcomeMessageId: out.welcomeMessageId,
    };
  }

  /**
   * ADR-0073 R.2 / ADR-0074 section 3.5 - THE filter every path that adds an
   * account's devices to a group goes through (`add-member-key.ts`): the
   * user-facing add, a new chat and a successor build, the ADR-0050
   * re-admission arm and the membership re-request. Call it AFTER the
   * identity gate ran for the account, so a proven rotation is recorded.
   *
   * @param expectedKeyB64 the account key the caller expects the person to
   *   have, or `null` for a path that has none: then the keys this device
   *   trusts for the account id decide (the pin and its proven chain, else
   *   its verified binding).
   * @returns the devices whose signed binding names an accepted key, in
   *   order. A key an accepted reset retired is never accepted.
   */
  private async devicesOfAccount<D extends { readonly keyPackage: Uint8Array }>(
    accountId: Uint8Array,
    devices: readonly D[],
    expectedKeyB64: string | null,
  ): Promise<D[]> {
    const wasm = await loadWasm();
    const accountIdB64 = base64Encode(accountId);
    const keys = accountKeysOf(accountIdB64);
    const superseded = loadSupersededKeys();
    const accepted =
      expectedKeyB64 !== null
        ? acceptableBoundKeys(expectedKeyB64, keys, superseded)
        : trustedBoundKeys(keys, readVerifiedBinding(accountIdB64)?.keyB64, superseded);
    return devicesBoundTo(devices, accepted, (d) => {
      try {
        const bound = wasm.keyPackageAccountKey(d.keyPackage);
        return bound === undefined ? null : base64Encode(bound);
      } catch {
        return null;
      }
    });
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
    // 0. ADR-0073 R.2: adding someone is an identity fetch that runs the
    //    fail-closed check (the directory serves the CURRENT identity's
    //    KeyPackages). This is also what the automatic re-request of an
    //    older add rides.
    await this.checkContactIdentity(recipientAccountId);

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
    // ADR-0073 R.2 - the directory is not trusted to say whose devices these
    // are. Only a device whose signed binding names the EXPECTED account key
    // (or a key of its pinned proven chain; never a key a reset retired) is
    // added; anything else is refused here, for every caller
    // (`add-member-key.ts`).
    devices = await this.devicesOfAccount(
      recipientAccountId,
      devices,
      base64Encode(recipientSignaturePubkey),
    );
    if (devices.length === 0) {
      throw ContactsError.noIdentityBundle(recipientAccountId);
    }

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
    }).then(async (outcome) => {
      // ADR-0060 section 6 — a proposed Add is committed by whoever sweeps,
      // who may hold no key that reaches the added person. Hand every
      // committer candidate a single-use delegation made with the key WE hold
      // for them. Outside the MLS lock: it is an ordinary application message.
      if (outcome.kind === "proposed") {
        // ADR-0050 2.5a point 4 — the intent, so a sweep that drops this
        // request is followed by a re-request (`chat/membership-retry.ts`).
        this.noteOwnRequest(
          identity,
          group,
          {
            kind: "add",
            subjectKeyB64: base64Encode(recipientSignaturePubkey),
            subjectAccountIdB64: base64Encode(recipientAccountId),
          },
          devices.map((d) => base64Encode(d.deviceId)),
        );
        await this.publishCommitterDelegations(identity, group, recipientSignaturePubkey);
      }
      return outcome;
    });
  }

  /**
   * ADR-0050 section 2.5a point 4 — record one own membership request that went
   * out as a PROPOSAL, at the epoch it was proposed at.
   *
   * Every public membership method calls this on its `proposed` outcome, so the
   * user path, the queue-full retry, the device re-admission arm and the
   * superseded re-request itself all record through one place. A direct commit
   * records nothing: it either landed or threw. Never throws — a lost record
   * costs the automatic re-request, never the request.
   */
  private noteOwnRequest(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    request: OwnRequest,
    deviceKeysB64?: readonly string[],
  ): void {
    if (identity === null) return;
    try {
      recordOwnRequest(
        base64Encode(identity.accountId),
        base64Encode(group.groupId),
        request,
        group.epoch,
        deviceKeysB64,
      );
    } catch {
      /* best-effort */
    }
  }

  /**
   * ADR-0060 section 6 — post a committer-delegation envelope for an Add this
   * device proposed: one delegation per committer candidate (the owner, then
   * the admins by rank, at most `maxCommitterDelegations()`), each signed with
   * the grant this account keeps for the added person and naming the
   * candidate's account key. Nothing is posted when this account holds no
   * grant for them (the added person is then reachable only if they are in
   * Everyone, or a committer holds a key of its own). Never throws: a missing
   * delegation costs the added person their Welcome, not the proposal.
   */
  private async publishCommitterDelegations(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    addedAccountKey: Uint8Array,
  ): Promise<void> {
    const session = reachSession();
    if (session === null) return;
    // ADR-0074 section 3.5a rules 1 and 2 - the grant held for the PERSON
    // (under any key they have had), for their CURRENT mailbox.
    const held = bestGrantForPerson(
      session.receivedGrants(),
      personKeysHex(journalHex(addedAccountKey)),
      { scopeHex: journalHex(group.groupId) },
    );
    if (held === null) return;
    try {
      const wasm = await loadWasm();
      let ownerHex: string | null = null;
      const adminHexes: string[] = [];
      const owner = group.roleTableOwner();
      if (owner !== undefined && owner !== null) ownerHex = journalHex(owner);
      const flat = group.roleTableAdmins();
      for (let i = 0; i + 32 <= flat.length; i += 32) adminHexes.push(journalHex(flat.slice(i, i + 32)));
      const candidates = committerCandidates(
        ownerHex,
        adminHexes,
        // The role table names ACCOUNT keys; `committerRank` ranks a LEAF key
        // (ADR-0050: an account key holds no leaf on a bound device), so the
        // account is ranked through any of its leaves, all of which hold the
        // account's rank (section 2.5a item 6).
        (hex) => {
          const leaf = rosterOf(group).find(
            (m) => journalHex(m.accountKey ?? m.signaturePublicKey) === hex,
          );
          return leaf === undefined ? undefined : group.committerRank(leaf.signaturePublicKey);
        },
        wasm.maxCommitterDelegations(),
      );
      if (candidates.length === 0) return;
      const grant = wasm.decodeReachGrant(base64Decode(held.wireB64));
      // ADR-0078 - the added person's mailbox is the one the grant's binding
      // names, verified against the account key the Add adds (and its proven
      // chain); the envelope carries the binding so the committer can verify
      // it too. A binding that does not verify posts nothing.
      const target = await verifiedTargetOfGrant(grant, personKeysOf(addedAccountKey));
      const binding = grant.binding;
      const seed = grant.seed;
      grant.free();
      if (target === null) {
        seed.fill(0);
        // eslint-disable-next-line no-console
        console.warn("ADR-0078: committer delegations not posted; the kept grant's mailbox binding did not verify");
        return;
      }
      const mailbox = target.address;
      const expires = BigInt(Date.now() + DELEGATION_TTL_MS);
      const blobs: Uint8Array[] = [];
      try {
        for (const hex of candidates) {
          blobs.push(
            wasm.signDelegationBlob(
              seed,
              mailbox,
              // The committer as the role table names it, resolved: a
              // committer whose account rotated signs with its current key
              // (it also tries the key named here unresolved, for a proposer
              // that has not observed the rotation).
              resolveAccountKey(hexBytes(hex)),
              expires,
              crypto.getRandomValues(new Uint8Array(16)),
            ),
          );
        }
      } finally {
        seed.fill(0);
      }
      const env = new wasm.CommitterDelegationEnvelope(addedAccountKey, binding, blobs);
      const bytes = wasm.composeCommitterDelegationEnvelope(env);
      env.free();
      const chat = new ChatClient(undefined, {
        ...(this.identityProvider === undefined ? {} : { identityProvider: this.identityProvider }),
        ...(this.onPersist === undefined ? {} : { onPersist: this.onPersist }),
      });
      await chat.broadcastControl(identity, group, bytes);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ContactsClient: committer delegations not posted", err instanceof Error ? err.message : "");
    }
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
    // A refused persist publishes nothing (see `persistProposalOrThrow`).
    await this.persistProposalOrThrow();
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
    //    (`accountKeyOfKeyPackage`, which CAN throw) OR
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
    let welcomeAccountKey: Uint8Array;
    let entryId: string;
    try {
      groupIdBytes = group.groupId;
      // ADR-0060 section 6 (amending ADR-0050 section 2.7): the Welcome is
      // sealed to THIS leaf's KeyPackage but addressed to the ACCOUNT mailbox
      // the leaf's binding names — one inbound mailbox per person.
      welcomeAccountKey = await accountKeyOfKeyPackage(keyPackageBytes);
      // ADR-0078 - the outbox keeps the RAW Welcome and the account key; the
      // row is sealed at publish to the mailbox key a grant held for that
      // account binds (verified then), which is also what a replay does.
      // ADR-0053 contract point 3 — register N+1 before anything durable says
      // the commit exists. A refusal lands in the catch below: reset, no publish.
      await this.registerNextEpochOrThrow(group, myIdentity, signerN);
      entryId = newOutboxEntryId();
      await this.persistCommitOrThrow({
        id: entryId,
        groupId: base64Encode(groupIdBytes),
        targetEpoch: group.epoch.toString(),
        commitB64: base64Encode(commitBytes),
        secondArtifactB64: base64Encode(welcomeBytes),
        secondArtifactKind: "welcome",
        secondArtifactAccountKeyB64: base64Encode(welcomeAccountKey),
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
    //    Rule 6 of ADR-0074 section 3.5a: a refusal re-checks the address
    //    once; the outbox entry above re-derives the mailbox on a replay.
    let welcomePublish;
    try {
      const wasm = await loadWasm();
      welcomePublish = await writeToPerson(
        welcomeAccountKey,
        { scope: groupIdBytes, allowMember: true },
        (target, auth) =>
          this.ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome", welcomeBytes), auth),
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
      // ADR-0050 2.5a point 4 — the subject by KEY, read before anything moves
      // the roster: the person's account key (or the legacy leaf key), never
      // the leaf index, which OpenMLS reuses for the next member added.
      const subjectKeyB64 = personKeyAtLeaf(group, leafIndex);
      // ADR-0045 — proposal path for a role-table group; the direct commit
      // survives only where there is no role table and therefore no committer.
      let outcome: MembershipOutcome | null = null;
      for (const leaf of leaves) {
        outcome = usesProposalPath(group)
          ? await this.proposeRemoveMemberLocked(myIdentity, group, leaf)
          : await this.removeMemberFromGroupLocked(myIdentity, group, leaf);
      }
      if (outcome?.kind === "proposed" && subjectKeyB64 !== null) {
        let own: string | null = null;
        try {
          own = ownPersonKeyB64(myIdentity);
        } catch {
          own = null;
        }
        this.noteOwnRequest(identity, group, {
          kind: subjectKeyB64 === own ? "leave" : "remove",
          subjectKeyB64,
        });
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
    /** The account the device belongs to, when the caller knows it. Recorded
     *  with the intent (ADR-0050 2.5a point 4) so a superseded admission can
     *  fetch a fresh KeyPackage; without it nothing is recorded and the
     *  scheduled re-admission arm is the only repair. */
    accountId?: Uint8Array,
  ): Promise<MembershipOutcome | null> {
    const wasm = await loadWasm();
    const deviceKey = wasm.keyPackageSignatureKey(keyPackage);
    if (leafIndexOfSignatureKey(group, deviceKey) !== null) return null;
    // ADR-0074 section 2.3 — a device of THIS account is admitted only when
    // the account's DeviceGroup holds it. Every caller that admits an own
    // device names the account, so the test is made here once.
    if (
      identity !== null &&
      accountId !== undefined &&
      bytesEqualCc(accountId, identity.accountId) &&
      !ownDeviceAdmissible(ownDeviceRoster(base64Encode(identity.accountId)), hexOfKey(deviceKey))
    ) {
      // eslint-disable-next-line no-console
      console.warn("own device not in the account's device list; not admitted to the chat");
      return null;
    }
    const myIdentity = await this.resolveIdentity(identity);
    // ADR-0074 (a device paired after a key rotation) — a device of THIS
    // account whose KeyPackage is bound to another account key than the one
    // this device's own leaves are bound to (it minted its binding before
    // the pairing state bundle reached it) is not admitted: to every gate of
    // the conversation it would be another account. It publishes again once
    // it re-minted, and the re-admission arm takes it then. Every own-device
    // add path comes through here.
    if (
      identity !== null &&
      accountId !== undefined &&
      bytesEqualCc(accountId, identity.accountId) &&
      !ownDeviceKeyPackageUsable(wasm, keyPackage, ownPersonKeyB64(myIdentity))
    ) {
      // eslint-disable-next-line no-console
      console.warn("own device bound under another of the account's keys; not admitted to the chat");
      return null;
    }
    // ADR-0073 R.2 - a device of ANOTHER account goes through the same
    // filter as every add path: its binding must name a key this device
    // trusts for that account id (never a key a reset retired). The
    // directory listing a device under an id proves nothing by itself.
    if (accountId !== undefined && (identity === null || !bytesEqualCc(accountId, identity.accountId))) {
      const kept = await this.devicesOfAccount(accountId, [{ keyPackage }], null);
      if (kept.length === 0) {
        // eslint-disable-next-line no-console
        console.warn("a listed device is not bound to a key trusted for its account; not admitted");
        return null;
      }
    }
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      // Re-checked under the lock: a poll may have applied the very commit
      // that admitted this device while we waited.
      if (leafIndexOfSignatureKey(group, deviceKey) !== null) return null;
      const outcome = usesProposalPath(group)
        ? await this.proposeAddMemberLocked(myIdentity, group, keyPackage)
        : await this.addMemberToGroupLocked(myIdentity, group, keyPackage);
      if (outcome.kind === "proposed" && accountId !== undefined) {
        this.noteOwnRequest(identity, group, {
          kind: "admit-device",
          subjectKeyB64: base64Encode(deviceKey),
          subjectAccountIdB64: base64Encode(accountId),
        });
      }
      return outcome;
    });
  }

  /**
   * ADR-0050 2.5a point 4 — re-derive a superseded device admission: fetch the
   * device's CURRENT KeyPackage from the directory and admit it through
   * {@link admitDeviceToGroup}. Never re-sends the bytes of the dropped
   * proposal.
   *
   * Throws a plain error (not a typed refusal, so the intent is kept for the
   * next merged commit) when the directory lists no KeyPackage for the device.
   */
  async readmitDeviceByKey(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    accountId: Uint8Array,
    deviceKey: Uint8Array,
  ): Promise<MembershipOutcome | null> {
    // ADR-0073 R.2 - the re-request runs the identity gate first, like every
    // add: a proven rotation is recorded (a verified binding becomes a pin
    // with its chain) before the device's binding is judged in
    // `admitDeviceToGroup`. Only for an account this device already trusts a
    // key for: a re-request never first-sights an account id.
    const own = identity !== null && bytesEqualCc(accountId, identity.accountId);
    if (!own) {
      const idB64 = base64Encode(accountId);
      if (readPin(idB64) === undefined && readVerifiedBinding(idB64) === undefined) {
        throw new Error("no trusted key for the device's account right now");
      }
      await this.checkContactIdentity(accountId);
    }
    let list;
    try {
      list = await this.discovery.fetchKeyPackages(accountId);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    const hit = list?.find((d) => bytesEqualCc(d.deviceId, deviceKey));
    if (hit === undefined) {
      throw new Error("the device has no published KeyPackage right now");
    }
    return this.admitDeviceToGroup(identity, group, hit.keyPackage, accountId);
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
      const outcome = usesProposalPath(group)
        ? await this.proposeRemoveMemberLocked(myIdentity, group, leaf)
        : await this.removeMemberFromGroupLocked(myIdentity, group, leaf);
      if (outcome.kind === "proposed") {
        this.noteOwnRequest(identity, group, {
          kind: "retire-device",
          subjectKeyB64: base64Encode(deviceKey),
        });
      }
      return outcome;
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
    // A refused persist publishes nothing (see `persistProposalOrThrow`).
    await this.persistProposalOrThrow();
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
    return withGroupMlsLock(base64Encode(group.groupId), async () => {
      const outcome = await this.setMemberAdminLocked(
        myIdentity,
        group,
        memberSigPubkey,
        makeAdmin,
      );
      // ADR-0050 2.5a point 4 — recorded as the DELTA, never the table: a
      // re-request recomputes the table from whatever is current then.
      if (outcome.kind === "proposed") {
        this.noteOwnRequest(identity, group, {
          kind: "set-admin",
          subjectKeyB64: base64Encode(memberSigPubkey),
          makeAdmin,
        });
      }
      return outcome;
    });
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
      // A refused persist publishes nothing: the proposal lives in this
      // device's pending store, and one on the wire that the device did not
      // record (its account wiped, the session gone, a disk failure) is a
      // request this device can no longer account for.
      await this.persistProposalOrThrow();
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
      // A refused persist publishes nothing: the proposal lives in this
      // device's pending store, and one on the wire that the device did not
      // record (its account wiped, the session gone, a disk failure) is a
      // request this device can no longer account for.
      await this.persistProposalOrThrow();
      let publish;
      try {
        publish = await this.ds.publishGroup(group.groupId, proposalBytes);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      this.noteOwnRequest(identity, group, {
        kind: "transfer-ownership",
        subjectKeyB64: base64Encode(newOwnerSigPubkey),
        outgoing,
      });
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
      // A refused persist publishes nothing: the proposal lives in this
      // device's pending store, and one on the wire that the device did not
      // record (its account wiped, the session gone, a disk failure) is a
      // request this device can no longer account for.
      await this.persistProposalOrThrow();
      let publish;
      try {
        publish = await this.ds.publishGroup(group.groupId, proposalBytes);
      } catch (err) {
        throw ContactsError.wrap(err);
      }
      this.noteOwnRequest(identity, group, {
        kind: "approve-join",
        subjectKeyB64: base64Encode(proposalRef),
      });
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
  async fetchJoinAttributions(
    groupId: Uint8Array,
    /** ADR-0078 - the account key a pending joiner's leaf is bound to
     *  (`Group.pendingExternalJoinAccountKey`). A reply grant is kept only
     *  when its mailbox binding verifies against it. */
    accountKeyOf?: (joinerSignatureKey: Uint8Array) => Uint8Array | undefined,
  ): Promise<
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
      if (base64Encode(attr.groupId) !== base64Encode(groupId)) {
        attr.free();
        continue;
      }
      // ADR-0060 section 6 (C-69) — the joiner's `reply` grant rides the
      // signed attribution; keep it so whichever committer sweeps the
      // approved `Add` can write the Welcome to the joiner's closed mailbox.
      const session = reachSession();
      if (session !== null) {
        const reply = attr.replyGrant;
        try {
          let accountKey: Uint8Array | undefined;
          try {
            accountKey = accountKeyOf?.(attr.joinerSignatureKey);
          } catch {
            accountKey = undefined;
          }
          // ADR-0078 - checked against the joiner's account key resolved from
          // the proposal's leaf; a grant naming another mailbox is not kept.
          if (joinReplyBindingVerifies(reply, accountKey)) {
            rememberJoinReply(attr.joinerSignatureKey, session.encodeGrant(reply));
          }
        } finally {
          reply.free();
        }
      }
      out.push({
        joinerSignatureKey: attr.joinerSignatureKey,
        issuerSignaturePubkey: attr.issuerSignaturePubkey,
        issuerFingerprint: attr.issuerFingerprint,
      });
      attr.free();
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
   *  member count at mint time. Since the 2026-09-28 security pass the WHOLE
   *  capability is sealed under a fresh 12-byte link secret that rides the
   *  URL **fragment** (`#<secret>`), which no browser sends and no ledger
   *  stores; the same secret opens the preview. Both the pointer and the
   *  inline form carry the same fragment. A name the display-name rule
   *  refuses is left out rather than failing the mint: the page then shows no
   *  name, never a guess.
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
    let mint: WasmInviteLinkMint;
    try {
      try {
        mint = mintWith({ groupName: preview.groupName, inviterName: preview.inviterName });
      } catch (err) {
        // The names come from user-typed fields (a group label, a profile
        // name); one the rule refuses must not take the link with it.
        // eslint-disable-next-line no-console
        console.warn("group-invite: preview names refused, minting without names", err);
        mint = mintWith({});
      }
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    const sealed = mint.sealed;
    const linkSecret = mint.linkSecret;
    mint.free();
    // ADR-0078 section 10: the ledger row carries a key minted for this link
    // alone; its seed is kept with the link's record and is the only thing
    // that revokes it.
    const revocation = await newRevocationKey();
    // Production default (ADR-0028 OQ1): upload the SEALED blob to the
    // discovery ledger (sealed-link amendment, 2026-09-28: the server cannot
    // open it; the clear fields carry what it needs) and return the short
    // pointer URL. `max_uses` is a soft throttle, not an access control: the
    // admin approval gate stays the security boundary.
    //
    // The inline form is the fallback for a TRANSPORT failure only (a
    // censored / offline network), where the link must still work without the
    // round-trip. A server answer (a 401 from a client with no session token,
    // a 429, a 5xx) surfaces instead: falling back there silently hands the
    // user the huge inline URL for a defect that should be fixed. The log
    // names the status and never the capability. Both forms carry the link
    // secret as the fragment; without it the link opens nothing.
    const widths = inviteLinkWidths(wasm);
    let url: string;
    try {
      const blobId = await this.discovery.uploadGroupInvite({
        sealed,
        revocationPubkey: revocation.publicKey,
        expiryMs,
        maxUses,
      });
      url = composePointerGroupInviteUrl(blobId, widths, origin);
      if (identity !== null) {
        await rememberMintedLink(base64Encode(identity.accountId), "group", base64UrlEncodeNoPad(blobId), {
          linkSecret,
          linkId: new Uint8Array(),
          revocationSeed: revocation.seed,
          facts: { label: "", expiryMs: Number(expiryMs), maxUses, bootstrapReferral: false },
        });
      }
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
      url = composeInlineGroupInviteUrl(sealed, origin);
    }
    return appendGroupInviteLinkSecret(url, linkSecret, widths);
  }

  /** ADR-0028 revoke — the issuer revokes a previously-created group invite
   *  link. Only the server-backed pointer form (a 6-byte ledger id) is
   *  revocable; an inline link has no ledger row. Parses the `blob_id` out of
   *  the `/g/<slug>` link, signs the revocation with the link's own revocation
   *  seed from this device's record of it (ADR-0078 section 10), and DELETEs
   *  the ledger row. Returns `false` (nothing to do) for an inline / malformed
   *  link, or one this device keeps no key for (it lives until it expires). */
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
    if (identity === null) return false;
    const accountIdB64 = base64Encode(identity.accountId);
    const slug = base64UrlEncodeNoPad(parsed.blobId);
    const kept = await mintedLinkFor(accountIdB64, "group", slug);
    if (kept === null || kept.revocationSeed === null) return false;
    try {
      const outcome = await revokeMintedLink(this.discovery, accountIdB64, {
        ...kept,
        kind: "group",
        blobIdSlug: slug,
        mintedAtMs: null,
      });
      return outcome === "done";
    } catch (err) {
      throw ContactsError.wrap(err);
    }
  }

  /** ADR-0028 (G-3) — resolve a `/g/<slug>` slug to the SEALED link blob,
   *  fetching from the discovery ledger when the slug is an 8-character
   *  ledger id (the production form) or passing the bytes straight through
   *  when it is the inline sealed blob (the upload-failed fallback). Returns
   *  the `blobId` (non-null only for the pointer form) so the caller can drive
   *  `consumeGroupInvite` before requesting to join. A 429 from the ledger
   *  surfaces as a wrapped `DiscoveryRateLimitedError` (see
   *  `isDiscoveryRateLimited`), never as "not found". */
  async resolveGroupInviteSlug(
    slug: string,
  ): Promise<{ sealed: Uint8Array; blobId: Uint8Array | null }> {
    let parsed;
    try {
      parsed = parseGroupInviteSlug(slug, inviteLinkWidths(await loadWasm()));
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    if (parsed.kind === "inline") {
      return { sealed: parsed.sealed, blobId: null };
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
    return { sealed: fetched, blobId: parsed.blobId };
  }

  /** Sealed-link amendment of ADR-0028 (2026-09-28): open a `/g/` link. A
   *  missing or malformed fragment is refused as damaged BEFORE any request;
   *  otherwise the sealed blob is resolved (ledger or inline) and opened with
   *  the fragment's secret, which also verifies the capability inside. Any
   *  open failure is `InviteLinkDamagedError`. The caller owns (and frees) the
   *  returned capability; `capabilityWire` is its PLAINTEXT wire, the value
   *  the joiner tag and the join attribution are computed over. */
  async openGroupInviteLink(
    slug: string,
    hash: string | null | undefined,
  ): Promise<{
    capability: WasmGroupInviteCapability;
    capabilityWire: Uint8Array;
    blobId: Uint8Array | null;
    linkSecret: Uint8Array;
  }> {
    const wasm = await loadWasm();
    const widths = inviteLinkWidths(wasm);
    // A malformed slug throws here, before any request.
    try {
      parseGroupInviteSlug(slug, widths);
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    const fragment = parseInviteLinkFragment(hash, widths);
    if (fragment.kind !== "secret") throw new InviteLinkDamagedError();
    const { sealed, blobId } = await this.resolveGroupInviteSlug(slug);
    const capability = openSealedInvite(
      (b, k) => wasm.GroupInviteCapability.openLink(b, k),
      sealed,
      fragment,
    );
    return { capability, capabilityWire: capability.wire, blobId, linkSecret: fragment.secret };
  }

  /** ADR-0028 (G-3) — joiner step 1: request to join a group from a `/g/` link
   *  capability (the plaintext wire the sealed link opened to, see
   *  `resolveGroupInviteSlug`). Decodes +
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
      cap.free();
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
    cap.free();
    // ADR-0060 section 6 (C-69) — the committer that sweeps our approved `Add`
    // writes the Welcome to this account's closed mailbox under our `reply`
    // grant, which rides the signed attribution. Registered (7-day expiry)
    // before the knock: a request whose answer could not land must not go
    // out. Closed once the group is joined (`closeRepliesTo(groupIdHex)`).
    const session = reachSession();
    if (session === null) {
      throw ContactsError.wrap(new KeyedError("common.error.starting_up"));
    }
    let replyGrant: import("../../wasm/lakko_ffi_wasm").ReachGrant;
    try {
      replyGrant = (await session.newReply("group-join", journalHex(groupId))).grant;
    } catch (err) {
      throw ContactsError.wrap(err);
    }
    let attribution: Uint8Array | null;
    try {
      // ADR-0028 OQ2 — the self-signed join attribution tells the approving
      // admin which member's link this request arrived on, and carries the
      // reply grant the Welcome needs.
      attribution = wasm.composeGroupJoinAttribution(myIdentity.ed25519Seed, capabilityWire, replyGrant);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("group-join: could not compose the join attribution", err instanceof Error ? err.name : "");
      attribution = null;
    } finally {
      replyGrant.free();
    }
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
        // The attribution is advisory for the approval (the admin approval
        // gate, ADR-0027, is the boundary) but it is also the only carrier of
        // our reply grant, so without it the Welcome lands only if this
        // account is in Everyone.
        buildAttribution: () => attribution,
        // The proposal's KeyPackage private key lives in the provider and is
        // the only thing that can open the Welcome the committer will send,
        // minutes or hours from now. FAIL-CLOSED and BEFORE the knock that
        // carries the proposal: a request this device could not record (the
        // stores sealed by Forget device, the session ended, an IndexedDB
        // failure) is never sent, so a refused persist means nothing was
        // published. After a reload an unrecorded key would fail the Welcome
        // with `NoMatchingKeyPackage`, unrecoverably.
        // (Unwrapped here: the catch below wraps every failure once.)
        persistBeforeKnock: () => this.persistDurableOrThrow(),
      });
    } catch (err) {
      // `GroupNotClaimedError` carries readable copy of its own: nobody in
      // the group has opened it since the update, so no epoch is registered.
      throw ContactsError.wrap(err);
    }
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

/**
 * ADR-0078 - open and dispatch the rows of one read of the own mailbox. Every
 * row is opened with `mailboxKey`; a row that does not open is DROPPED with no
 * log (it may be garbage, a row sealed for another mailbox, or an unknown
 * kind), and the cursor still advances past it. The opened kind decides the
 * decoder: a Welcome is surfaced as is, an ack and a request are verified as
 * before. A Welcome rendezvous (the co-publisher join) is read by the
 * co-publisher flow itself, never here. With `welcomesOnly` (a predecessor
 * mailbox, ADR-0074 section 3.5a rules 4 and 5) a request or an ack is
 * neither opened further nor acted on. Exported for the host tests.
 */
export function dispatchMailboxRows(
  wasm: Wasm,
  mailboxKey: import("../../wasm/lakko_ffi_wasm").MailboxKey,
  messages: readonly { readonly id: number; readonly payload: Uint8Array; readonly createdAt: string }[],
  after: number,
  myX25519Secret: Uint8Array,
  options: { readonly welcomesOnly?: boolean } = {},
): { events: IncomingMailboxEvent[]; cursor: number } {
  const events: IncomingMailboxEvent[] = [];
  let cursor = after;
  for (const message of messages) {
    cursor = message.id;
    const row = openOwnRow(wasm, mailboxKey, message.payload);
    if (row === null) continue;
    if (row.kind === "welcome") {
      events.push({
        kind: "welcome",
        welcome: {
          messageId: message.id,
          welcomeBytes: row.body,
          createdAt: message.createdAt,
          fromAccountMailbox: true,
        },
      });
      continue;
    }
    if (row.kind === "welcome-rendezvous") continue;
    if (options.welcomesOnly === true) continue;
    if (row.kind === "contact-ack") {
      // Phase 1.9b1: signature is verified inside the WASM
      // surface; bad-signature / malformed-wire entries drop
      // with a warn and the cursor still advances.
      let ack;
      try {
        ack = wasm.ContactAck.decodeAndVerify(row.body);
      } catch {
        // eslint-disable-next-line no-console
        console.warn("ContactAck verify failed");
        continue;
      }
      // ADR-0060 — they answered: the `reply` key our request handed them
      // has done its job and is removed from our mailbox.
      void reachSession()?.closeRepliesTo(journalHex(ack.accepterPubkey));
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
    // row.kind === "contact-request"
    const request = decodeContactRequestBody(wasm, message.id, row.body, message.createdAt, myX25519Secret);
    if (request === null) continue;
    events.push({
      kind: "contact-request",
      request,
      // ADR-0078: the row kept on this device is the OPENED request body
      // (`decodePendingRequestRows` decodes it directly).
      row: { messageId: message.id, payload: row.body, createdAt: message.createdAt },
    });
  }
  return { events, cursor };
}

/**
 * Verify and open one ContactRequest body (the envelope with its kind byte
 * already stripped). Shared by the live poll and by the reload path that
 * rebuilds pending requests from the rows `contacts/inbox-store.ts` kept, so
 * a restored request passes exactly the checks a freshly polled one did.
 *
 * Returns `null` when the signature or the wire fails verification. An
 * envelope that does not open for `x25519Secret` still yields a request
 * (the signature is verified) with no intro and no profile.
 */
function decodeContactRequestBody(
  wasm: Wasm,
  messageId: number,
  body: Uint8Array,
  createdAt: string,
  x25519Secret: Uint8Array,
): IncomingContactRequest | null {
  let parsed;
  try {
    parsed = wasm.ContactRequest.decodeAndVerify(body);
  } catch (err) {
    // Bad signature, malformed wire bytes, or some other verifier failure.
    // Dropped; the caller's cursor still advances past the row.
    // eslint-disable-next-line no-console
    console.warn("ContactRequest verify failed", messageId, err);
    return null;
  }

  let intro: string | null = null;
  let profile: ReceivedProfile | null = null;
  let replyGrantB64: string | null = null;
  let linkAnswer: LinkAnswer | null = null;
  try {
    const opened = parsed.openEnvelope(x25519Secret);
    intro = new TextDecoder("utf-8", { fatal: false }).decode(opened.introMessage);
    // ADR-0079 section 2 — the link this request answers, if any. Copied
    // before `free()`.
    const linkId = opened.linkAnswerId;
    const linkSecret = opened.linkAnswerSecret;
    if (linkId !== undefined && linkSecret !== undefined) {
      linkAnswer = { linkId: linkId.slice(), linkSecret: linkSecret.slice() };
    }
    // ADR-0060 section 6 — the requester's `reply` grant, the key our accept
    // and ack are written with. Only a `reply` class is kept.
    const reply = opened.replyGrant;
    if (reply !== undefined) {
      if (reply.class === wasm.ReachClass.Reply) {
        replyGrantB64 = base64Encode(wasm.encodeReachGrant(reply));
      }
      reply.free();
    }
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
    // Envelope didn't decrypt for our X25519 secret. Could be a malicious
    // request, could be a bug; either way the request is still surfaced (the
    // signature is verified) but with no intro body.
    // eslint-disable-next-line no-console
    console.warn("ContactRequest envelope unseal failed", messageId, err);
  }

  // `fromAccountId` is `Uint8Array | undefined` on the wasm-bindgen surface —
  // normalise to `null` for the discriminated TS API (the UI distinguishes
  // present-but-empty from absent).
  const fromAccountId = parsed.fromAccountId === undefined ? null : parsed.fromAccountId;
  return {
    messageId,
    fromPubkey: parsed.fromPubkey,
    fromAccountId,
    fromPseudonym: parsed.fromPseudonym,
    validAfterMs: parsed.validAfterMs,
    intro,
    profile,
    replyGrantB64,
    linkAnswer,
    createdAt,
  };
}

/** Lowercase hex to bytes (the form `journalHex` produces). */
function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
