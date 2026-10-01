// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0056 security follow-up: on the join-by-name restore (another device,
 * no password), the account the fresh device lands in comes from the grant
 * redeem, and the prompt was routed there by the auth login handle of the
 * typed name. A squatted login handle can route that prompt to another
 * account, whose device confirms, and the fresh device would land in that
 * account and save the TYPED name as its own.
 *
 * Discovery is a registry independent of auth (its own service, its own
 * database, its own OPRF key), so the fresh device asks it too: with the
 * freshly redeemed session, it looks the typed name up through the ordinary
 * token-spending discovery lookup and requires the resolved account to be
 * the redeemed one. Only then does anything land on this device.
 *
 * Fail closed: a mismatch, a name discovery does not know, and a lookup that
 * could not run (network, token quota, tokens switched off) all refuse, zero
 * the transferred seed and drop the session before anything is persisted.
 *
 * Pure orchestration over injected steps, so the ordering is pinned by a
 * test (`device-restore-name-check.test.ts`).
 */
import { KeyedError } from "../l10n/keyed-error";

/** The name resolves to another account, or to none. */
export const TYPED_NAME_OTHER_ACCOUNT =
  "The name you typed belongs to a different account. Nothing was changed on this device.";

/** The lookup could not run; the app's generic network failure copy. */
export const TYPED_NAME_CHECK_UNAVAILABLE =
  "Could not reach Lakko. Check your connection and try again.";

/** The typed name does not resolve to the redeemed account. Keyed
 *  (ADR-0061 section 6): the screen renders `device_link.error.name_other_account`
 *  in the current language; {@link TYPED_NAME_OTHER_ACCOUNT} is its English. */
export class TypedNameOtherAccount extends KeyedError {
  override name = "TypedNameOtherAccount";
  constructor() {
    super("device_link.error.name_other_account");
  }
}

/** The typed name could not be checked; nothing landed. Keyed as the app's
 *  generic network failure (`common.error.offline`), whose English is
 *  {@link TYPED_NAME_CHECK_UNAVAILABLE}. */
export class TypedNameCheckUnavailable extends KeyedError {
  override name = "TypedNameCheckUnavailable";
  constructor(cause: unknown) {
    super("common.error.offline");
    this.cause = cause;
  }
}

/**
 * The `lookup` step over the discovery client: the account the typed name
 * resolves to, `null` when discovery does not know it. This is the one
 * discovery lookup a sign-in or recovery path makes, and it runs only after
 * the redeemed session is attached (see `runNameCheckedDeviceRestore`).
 */
export function discoveryAccountLookup(discovery: {
  lookup(username: string): Promise<{ readonly account: Uint8Array | null }>;
}): (username: string) => Promise<Uint8Array | null> {
  return async (username) => (await discovery.lookup(username)).account;
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) {
    diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  }
  return diff === 0;
}

/**
 * Run the gates of a join-by-name device restore, then `land` it.
 *
 * Order: `verifySeed` (the transferred seed reproduces the published
 * identity of the redeemed account; it zeroes the seed on its own refusals)
 * -> `attachSession` (the lookup draws its anonymous token with the redeemed
 * session) -> `lookup(username)` -> compare with `accountIdBytes` -> `land`.
 * Any refusal after the session is attached zeroes `phraseSeed`, calls
 * `dropSession` and throws before `land` runs.
 */
export async function runNameCheckedDeviceRestore(args: {
  /** The name the person typed on this device. */
  readonly username: string;
  /** The account the grant redeem answered for. */
  readonly accountIdBytes: Uint8Array;
  /** The transferred AccountIdentity seed; zeroed on every refusal. */
  readonly phraseSeed: Uint8Array;
  /** The redeemed session token. */
  readonly sessionToken: string;
  readonly verifySeed: () => Promise<void>;
  readonly attachSession: (sessionToken: string) => void;
  readonly dropSession: () => void;
  /** The discovery lookup: the account the name resolves to, `null` when
   *  discovery does not know the name. */
  readonly lookup: (username: string) => Promise<Uint8Array | null>;
  /** Persist and land the session; runs only after every gate passed. */
  readonly land: () => Promise<void>;
}): Promise<void> {
  await args.verifySeed();

  args.attachSession(args.sessionToken);
  let resolved: Uint8Array | null;
  try {
    resolved = await args.lookup(args.username);
  } catch (err) {
    args.phraseSeed.fill(0);
    args.dropSession();
    // eslint-disable-next-line no-console
    console.warn(
      "device restore: typed-name lookup failed; refusing",
      err instanceof Error ? err.name : "unknown",
    );
    throw new TypedNameCheckUnavailable(err);
  }
  if (resolved === null || !sameBytes(resolved, args.accountIdBytes)) {
    args.phraseSeed.fill(0);
    args.dropSession();
    // eslint-disable-next-line no-console
    console.warn(
      resolved === null
        ? "device restore: typed name unknown to discovery; refusing"
        : "device restore: typed name resolves to another account; refusing",
    );
    throw new TypedNameOtherAccount();
  }
  await args.land();
}
