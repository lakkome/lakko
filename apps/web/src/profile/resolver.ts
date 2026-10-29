// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §5 / UX spec §3 — the ONE display resolver.
 *
 * Every surface that shows a person (contacts, request cards, chat list and
 * header, message authors, typing, reactions, read receipts, join requests,
 * the members panel, admin notices, call rosters and banners) asks this
 * function, so the precedence cannot drift between screens:
 *
 *   1. a name the viewer gave this contact (local rename);
 *   2. the person's profile display name;
 *   3. the person's VERIFIED `@username`;
 *   4. the key fingerprint, WITHOUT `@`.
 *
 * The "profile" of steps 2 and 3 is the person's received profile envelope
 * or, when none was received and the viewer gave no rename, what a pending
 * contact's invite link states (ADR-0079: the inviter's own name and
 * username, inside the capability their account key signed). A received
 * profile wins over the link because it is the person's current statement;
 * the link's username is verified exactly like a profile's claim
 * ({@link resolvePerson}).
 *
 * ## A username is shown only when verified
 *
 * An unverified claim is not rendered at all — not greyed out, not with a
 * question mark — because a displayed `@pavel` that is not Pavel is exactly
 * the impersonation ADR-0048 exists to prevent. So this function takes the
 * verification state for the CLAIMED username as an input and treats anything
 * but `verified` as absent.
 *
 * ## The fingerprint carries no `@`
 *
 * Before ADR-0048 every contact was labelled `@` + a hex prefix of its key,
 * which reads like a username and is not one. The fallback here is the same
 * bytes grouped `a1b2 c3d4`, so it can never be mistaken for a handle.
 *
 * Pure: no wasm, no storage, no Solid.
 */

/** Verification state of a `(signature key, username)` claim. */
export type VerificationState =
  /** Discovery resolved the username to this very key. */
  | "verified"
  /** Discovery resolved it to another key, or to nobody, or the name fails
   *  the username rule. Definitive: never shown for this key. */
  | "mismatch"
  /** A lookup is in flight. */
  | "pending"
  /** The lookup could not complete (network, proof-of-work). Retried later;
   *  shown the same as a mismatch meanwhile — i.e. not at all. */
  | "error";

/** What the resolver needs about one person. */
export interface DisplayInputs {
  /** The person's Ed25519 signature public key (32 bytes). Only used for the
   *  fingerprint fallback. */
  readonly signatureKey: Uint8Array;
  /** The viewer's local rename, if any. Blank means "no rename". */
  readonly rename?: string | undefined;
  /** The person's received profile, if any. */
  readonly profile?:
    | {
        readonly displayName: string | null;
        readonly username: string | null;
      }
    | undefined;
  /** Verification state of `profile.username` for this key. */
  readonly verification?: VerificationState | undefined;
}

/** Which rule produced the primary name. */
export type NameSource = "rename" | "profile" | "username" | "fingerprint";

/** The resolved presentation of one person. */
export interface ResolvedName {
  /** The name to show wherever there is room for one string (bubbles, call
   *  tiles, typing, reactions). */
  readonly name: string;
  readonly source: NameSource;
  /** `@alice` when the claimed username is verified, else `null`. */
  readonly username: string | null;
  /** The label for lists: `Name · @username` when a verified username exists
   *  and the name is not already the username; otherwise just `name`. */
  readonly listLabel: string;
  /** The person's own profile name when a local rename overrides it — for the
   *  contact detail's `Their name: X`. `null` otherwise. */
  readonly theirName: string | null;
  /** The key fingerprint, `a1b2 c3d4`. Always available for detail views. */
  readonly fingerprint: string;
  /** A username is claimed and its verification failed or mismatched — the
   *  quiet `Username not verified` line in the contact detail. A lookup still
   *  in flight is not a failure. */
  readonly usernameNotVerified: boolean;
}

/** Separator between a name and a verified username in a list row. */
export const NAME_USERNAME_SEPARATOR = " · ";

/**
 * The key fingerprint without `@`: the first four bytes as lower-case hex, in
 * two groups of four (`a1b2 c3d4`). The same bytes the old `@`-prefixed label
 * used, so a user comparing the two sees the same digits.
 */
export function fingerprintLabel(signatureKey: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < 4; i++) {
    hex += (signatureKey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return `${hex.slice(0, 4)} ${hex.slice(4, 8)}`;
}

/** Resolve one person's presentation. See the module docs for the rules. */
export function resolveDisplay(inputs: DisplayInputs): ResolvedName {
  const fingerprint = fingerprintLabel(inputs.signatureKey);
  const rename = (inputs.rename ?? "").trim();
  const profileName = inputs.profile?.displayName ?? null;
  const claimed = inputs.profile?.username ?? null;
  const verified = claimed !== null && claimed.length > 0 && inputs.verification === "verified";
  const username = verified ? `@${claimed}` : null;
  // Only a refusal earns the line. A check that could not reach the
  // directory (a 429, the network) says nothing about the person: it reads
  // as a check still running and is retried (2026-10-24, docs/ux/profile-names.md
  // section 6).
  const usernameNotVerified =
    claimed !== null && claimed.length > 0 && inputs.verification === "mismatch";

  let name: string;
  let source: NameSource;
  if (rename.length > 0) {
    name = rename;
    source = "rename";
  } else if (profileName !== null && profileName.length > 0) {
    name = profileName;
    source = "profile";
  } else if (username !== null) {
    name = username;
    source = "username";
  } else {
    name = fingerprint;
    source = "fingerprint";
  }

  const listLabel =
    username !== null && source !== "username"
      ? `${name}${NAME_USERNAME_SEPARATOR}${username}`
      : name;
  const theirName =
    source === "rename" && profileName !== null && profileName.length > 0 && profileName !== rename
      ? profileName
      : null;

  return { name, source, username, listLabel, theirName, fingerprint, usernameNotVerified };
}


/** What a pending contact's invite link states about the inviter
 *  (ADR-0079). Empty strings mean "not stated". */
export interface LinkStatement {
  readonly displayName: string;
  readonly username: string;
}

/** The profile fields the resolver works from. */
export interface ProfileInput {
  readonly displayName: string | null;
  readonly username: string | null;
}

/** Everything {@link resolvePerson} needs about one person. */
export interface PersonInputs {
  readonly signatureKey: Uint8Array;
  /** The viewer's local rename, if any. Blank means "no rename". */
  readonly rename?: string | undefined;
  /** The person's received profile, if any. */
  readonly received?: ProfileInput | undefined;
  /** What a pending contact's invite link states, when this key is the
   *  pending contact's account key. */
  readonly linkStatement?: LinkStatement | undefined;
  /** The verification state of `username` claimed for this key. */
  readonly verificationOf: (username: string) => VerificationState | undefined;
}

/**
 * The profile the resolver uses for one person: the received profile when
 * there is one; otherwise, when the viewer gave no rename, the pending
 * contact's link statement; otherwise none. Pure.
 */
export function profileInputOf(
  rename: string | undefined,
  received: ProfileInput | undefined,
  linkStatement: LinkStatement | undefined,
): ProfileInput | undefined {
  if (received !== undefined) return received;
  if ((rename ?? "").trim().length > 0 || linkStatement === undefined) return undefined;
  const displayName = linkStatement.displayName.trim();
  const username = linkStatement.username.trim();
  if (displayName.length === 0 && username.length === 0) return undefined;
  return {
    displayName: displayName.length > 0 ? displayName : null,
    username: username.length > 0 ? username : null,
  };
}

/**
 * Resolve one person from everything the device holds about them: the
 * rename, the received profile, a pending contact's link statement, and the
 * verification state of whichever username claim is used. The ONE place the
 * precedence among these sources is decided; `ProfileContext` calls it.
 */
export function resolvePerson(inputs: PersonInputs): ResolvedName {
  const profile = profileInputOf(inputs.rename, inputs.received, inputs.linkStatement);
  const claimed = profile?.username ?? null;
  return resolveDisplay({
    signatureKey: inputs.signatureKey,
    rename: inputs.rename,
    profile,
    verification: claimed !== null && claimed.length > 0 ? inputs.verificationOf(claimed) : undefined,
  });
}
