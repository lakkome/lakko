// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 trigger 4 — the profile REQUEST, a newcomer's "introduce
 * yourselves" (`crates/lakko-contacts/src/profile_request.rs`).
 *
 * ## Why it exists
 *
 * A profile is pushed into a group when a device joins, renames, or notices
 * the group grew. The last trigger is what a newcomer depends on for everyone
 * else's name, and it fires on the OTHER devices — each of which has to notice
 * the growth. A device that was away when the join landed never re-sends, a
 * closed web tab cannot, so the newcomer sits with `Member` in place of a name
 * (three of five members on a web account that had joined last, 2026-09-19).
 * The request is the newcomer's pull: it asks once, and every member answers
 * with its ordinary profile envelope through the throttled growth path.
 *
 * ## What lives here, and why it is a plain module
 *
 * Three things the receive loop, the chat view and the provider share:
 *
 *   * {@link sniffProfileRequest} — recognising the envelope ahead of the
 *     chat parser, whose lossy-UTF-8 fallback would otherwise render it as a
 *     bubble reading `lakko/profile-request/v1` plus a control byte;
 *   * {@link decideProfileRequest} — whether THIS device should ask now,
 *     composed from the roster, the name resolver and the persisted
 *     last-asked stamp, with the core's rule injected so the decision is
 *     testable without wasm;
 *   * the small persisted store of when this device last asked each group.
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in a `node` environment with
 * no Solid transform, so a rule that lived in a `.tsx` file could not be
 * tested at all. No wasm is imported: the decoder and the rule come in as
 * structural ports.
 */

import type { NameSource } from "./resolver";

// ---------------------------------------------------------------------------
// Recognising the envelope
// ---------------------------------------------------------------------------

/** The two wasm exports the sniff needs, structurally. */
export interface ProfileRequestDecoder {
  /** Throws for anything that is not a well-formed request. Returns nothing
   *  on success — the envelope is field-less. */
  decodeProfileRequest(bytes: Uint8Array): void;
  /** The exact on-wire length, read from the FFI rather than hand-copied. */
  profileRequestEnvelopeLen(): number;
}

/** First byte every `lakko/…` service-envelope domain starts with. A cheap
 *  skip for chat envelopes (JSON, `{`) and call signaling; never a verdict. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/**
 * Is this application plaintext a profile request?
 *
 * Like the committer beacon, the request carries no fields, so every request
 * on the wire is byte-identical and `length === profileRequestEnvelopeLen()`
 * plus the domain prefix already pins all but the version byte. The decode
 * behind the peek is what makes the arm fail-closed on an unknown schema
 * version: a future v2 request this build does not understand falls through
 * to the chat parser like any other unrecognised plaintext rather than being
 * silently swallowed by a length-only match.
 *
 * The length comes from the FFI, never from a local `26` — a stale literal
 * fails in the dangerous direction (a too-narrow peek stops suppressing and
 * the request renders as a bubble again). Never throws.
 */
export function sniffProfileRequest(decoder: ProfileRequestDecoder, bytes: Uint8Array): boolean {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return false;
  let expected: number;
  try {
    expected = decoder.profileRequestEnvelopeLen();
  } catch {
    return false;
  }
  if (bytes.length !== expected) return false;
  try {
    decoder.decodeProfileRequest(bytes);
    return true;
  } catch {
    // Not a request (wrong domain / unknown version) — let the chat parser
    // have it.
    return false;
  }
}

// ---------------------------------------------------------------------------
// Whether to ask
// ---------------------------------------------------------------------------

/** The core's `shouldRequestProfiles`, as a port. */
export type ProfileRequestRule = (
  joinedJustNow: boolean,
  unnamedMembers: number,
  lastRequestedAtMs: bigint | undefined,
  nowMs: bigint,
) => boolean;

export interface ProfileRequestInput {
  /** The device has just joined the group — always ask. */
  readonly joinedJustNow: boolean;
  /** The distinct PEOPLE in the roster (base64 account keys, ADR-0050),
   *  the viewer included; the count below excludes them. */
  readonly memberKeysB64: readonly string[];
  /** Is this key one of the viewer's own? */
  readonly isOwnKey: (keyB64: string) => boolean;
  /** Where the display resolver got this person's name from. */
  readonly sourceOf: (keyB64: string) => NameSource;
  /** When this device last asked this group, or `undefined` if never. */
  readonly lastRequestedAtMs: number | undefined;
  readonly nowMs: number;
}

export interface ProfileRequestDecision {
  /** Publish the request now. */
  readonly ask: boolean;
  /** How many people other than the viewer this device cannot name. */
  readonly unnamed: number;
}

/**
 * How many people in `memberKeysB64` this device holds no name for.
 *
 * "No name" is the resolver's own answer — `source === "fingerprint"`, i.e.
 * neither a local rename, nor a received profile, nor a verified username.
 * The viewer is excluded: their own row is named from the own profile and a
 * request cannot change it. Duplicate keys count once.
 */
export function unnamedMemberCount(
  memberKeysB64: readonly string[],
  isOwnKey: (keyB64: string) => boolean,
  sourceOf: (keyB64: string) => NameSource,
): number {
  let unnamed = 0;
  const seen = new Set<string>();
  for (const key of memberKeysB64) {
    if (seen.has(key)) continue;
    seen.add(key);
    if (isOwnKey(key)) continue;
    if (sourceOf(key) === "fingerprint") unnamed += 1;
  }
  return unnamed;
}

/**
 * Should this device ask the group to introduce itself now?
 *
 * The rule itself is the core's (`shouldRequestProfiles`, so the three clients
 * cannot drift on it); this composes its inputs. A stamp that does not parse
 * as a finite number reads as "never asked" — the safe direction, since the
 * worst outcome is one extra 26-byte request per day.
 */
export function decideProfileRequest(
  input: ProfileRequestInput,
  rule: ProfileRequestRule,
): ProfileRequestDecision {
  const unnamed = unnamedMemberCount(input.memberKeysB64, input.isOwnKey, input.sourceOf);
  const last =
    input.lastRequestedAtMs !== undefined && Number.isFinite(input.lastRequestedAtMs)
      ? BigInt(Math.max(0, Math.trunc(input.lastRequestedAtMs)))
      : undefined;
  const now = BigInt(Math.max(0, Math.trunc(input.nowMs)));
  return { ask: rule(input.joinedJustNow, unnamed, last, now), unnamed };
}

// ---------------------------------------------------------------------------
// When this device last asked — persisted per (account, group)
// ---------------------------------------------------------------------------

/** The subset of `Storage` the store needs, so a test can hand in a map. */
export interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const STORE_PREFIX = "lakko.profile-requests.v1";

function storeKey(accountIdB64: string, groupIdB64: string): string {
  return `${STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

function defaultStorage(): KeyValueStorage | undefined {
  return (globalThis as { localStorage?: KeyValueStorage }).localStorage;
}

/**
 * When this device last asked `groupIdB64`, in ms, or `undefined` if never.
 * Never throws: a corrupt or absent row reads as "never asked".
 */
export function lastProfileRequestAt(
  accountIdB64: string,
  groupIdB64: string,
  storage: KeyValueStorage | undefined = defaultStorage(),
): number | undefined {
  try {
    const raw = storage?.getItem(storeKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return undefined;
    const parsed = Number(raw);
    return Number.isFinite(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

/** Record that this device asked `groupIdB64` at `atMs`. Never throws —
 *  losing the stamp costs at most one extra request per day. */
export function recordProfileRequest(
  accountIdB64: string,
  groupIdB64: string,
  atMs: number,
  storage: KeyValueStorage | undefined = defaultStorage(),
): void {
  if (!Number.isFinite(atMs)) return;
  try {
    storage?.setItem(storeKey(accountIdB64, groupIdB64), String(Math.trunc(atMs)));
  } catch {
    /* best-effort */
  }
}
