// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a backup keeps of the person themselves: their own profile (name,
 * username, photo references) and the "who can find me" setting, each as the
 * opaque envelope another device of the account would receive. Until these
 * travelled, a restore landed a person with no name and back on the default
 * setting (D31, D37 of the 2026-11-01 walk).
 *
 * Composing reads the stores, not the session objects: the baseline uploaded
 * right after a restore runs before the profile and reach runtimes have
 * bound, and a blob without these fields would replace one that has them.
 *
 * Restoring is fail closed. An empty or undecodable field changes nothing.
 * The name and photo go to the own-profile store only when it holds nothing
 * newer. The setting goes through the intake of a DeviceGroup Visibility
 * envelope, so the mailbox and the Everyone publication run as for any other
 * change; it is also written to its row first, so the baseline that follows a
 * restore carries it before the setting has loaded.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { avatarFromWasm, type AvatarData } from "../profile/avatar-model";
import { adoptOwnProfile, loadOwnProfile } from "../profile/own-profile";
import { composeOwnProfileForDevices, type AccountIdentity } from "../profile/own-profile-sync";
import { ownCardShareable } from "../profile/card-share";
import { visibilitySetting } from "../reach/visibility-store";
import { loadReachRow, saveReachRow } from "../reach/reach-store";
import { loadWasm, type Wasm } from "../wasm";
import { registerAccountMemory } from "../auth/account-memory";

/** Answers the account's username while a session of that account stands. */
export type BackupUsernameSource = (accountIdB64: string) => string | null;

let usernameSource: BackupUsernameSource | null = null;

/** Register where a backup finds the username; returns the unregister. */
export function registerBackupUsernameSource(source: BackupUsernameSource): () => void {
  usernameSource = source;
  return () => {
    if (usernameSource === source) usernameSource = null;
  };
}

/**
 * The own profile envelope a backup carries: the stored name and photo under
 * the stored stamp, with the username when the session knows it. `null` for
 * an account that never saved a profile.
 */
export function ownProfileForBackup(
  wasm: Wasm,
  /** The account identity (ADR-0050: the person, not a device); it signs
   *  the envelope (ADR-0101 section 8). */
  accountIdentity: AccountIdentity,
  accountIdB64: string,
  accountIdBytes: Uint8Array,
): Uint8Array | null {
  try {
    return composeOwnProfileForDevices(
      wasm,
      accountIdentity,
      loadOwnProfile(accountIdB64),
      accountIdBytes,
      usernameSource?.(accountIdB64) ?? null,
      ownCardShareable(visibilitySetting()),
    );
  } catch (err) {
    // A record that does not compose must not cost the whole backup.
    // eslint-disable-next-line no-console
    console.warn("backup: own profile not carried", err instanceof Error ? err.name : "");
    return null;
  }
}

/** The "who can find me" envelope a backup carries, as it is stored, or
 *  `null` with no setting on this device. */
export async function visibilityForBackup(accountIdB64: string): Promise<Uint8Array | null> {
  try {
    const row = await loadReachRow(accountIdB64, "visibility");
    return row === null ? null : base64Decode(row);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("backup: setting not carried", err instanceof Error ? err.name : "");
    return null;
  }
}

/** What the restored profile says. */
export interface RestoredOwnProfile {
  readonly displayName: string | null;
  /** The username the profile names, `null` for none. A claim only: the
   *  caller checks it with auth before the device takes it
   *  (`auth/username-prompt.ts`). */
  readonly username: string | null;
  readonly updatedAtMs: bigint;
  readonly avatar: AvatarData | null;
}

/** Decode the backup's own-profile field. `null` for an empty field, one that
 *  does not decode, or one naming another account. Pure over the core. */
export function decodeRestoredOwnProfile(
  wasm: Pick<Wasm, "decodeProfileEnvelope">,
  bytes: Uint8Array,
  accountIdB64: string,
): RestoredOwnProfile | null {
  if (bytes.length === 0) return null;
  let profile: ReturnType<Wasm["decodeProfileEnvelope"]>;
  try {
    profile = wasm.decodeProfileEnvelope(bytes);
  } catch {
    return null;
  }
  try {
    const accountId = profile.accountId;
    if (accountId !== undefined && base64Encode(accountId) !== accountIdB64) return null;
    return {
      displayName: profile.displayName ?? null,
      username: nonEmpty(profile.username),
      updatedAtMs: profile.updatedAtMs,
      avatar: avatarFromWasm(profile.avatar),
    };
  } catch {
    return null;
  } finally {
    profile.free();
  }
}

function nonEmpty(value: string | undefined): string | null {
  const trimmed = (value ?? "").trim();
  return trimmed.length > 0 ? trimmed : null;
}

/** The username the last restore of an account's backup named, in this
 *  page. Kept apart from the own-profile store, which holds no username: a
 *  passkey sign-in lands with none, and this is where the device first looks
 *  for it (`docs/ux/identity-recovery.md` section 8b). */
const restoredUsernames = new Map<string, string>();

registerAccountMemory("restored-own-username", (ids) => {
  for (const id of ids) restoredUsernames.delete(id);
});

/** The username the backup restored in this page named for `accountIdB64`,
 *  or `null`. */
export function restoredOwnUsername(accountIdB64: string): string | null {
  return restoredUsernames.get(accountIdB64) ?? null;
}

/** Keep the backup's own profile when the store holds nothing newer. Answers
 *  whether it was kept. Never throws. */
export async function adoptRestoredOwnProfile(accountIdB64: string, bytes: Uint8Array): Promise<boolean> {
  if (bytes.length === 0) return false;
  const restored = decodeRestoredOwnProfile(await loadWasm(), bytes, accountIdB64);
  if (restored === null) return false;
  // Whatever the store does with the name and photo, the username the
  // backup names is kept for the landing to check.
  if (restored.username !== null) restoredUsernames.set(accountIdB64, restored.username);
  return (
    adoptOwnProfile(accountIdB64, restored.displayName, restored.updatedAtMs, undefined, restored.avatar) !== null
  );
}

/** The stamp of a Visibility envelope, or `null` when it does not decode. */
export function visibilityStampOf(
  wasm: Pick<Wasm, "decodeVisibilityEnvelope">,
  bytes: Uint8Array,
): bigint | null {
  try {
    const decoded = wasm.decodeVisibilityEnvelope(bytes);
    try {
      return decoded.updatedAtMs;
    } finally {
      decoded.free();
    }
  } catch {
    return null;
  }
}

/** Put the backup's setting in its row when that holds nothing newer. Answers
 *  whether the field was a setting at all: `false` for an empty or undecodable
 *  field, which changes nothing. */
export async function storeRestoredVisibility(accountIdB64: string, bytes: Uint8Array): Promise<boolean> {
  if (bytes.length === 0) return false;
  const wasm = await loadWasm();
  const incoming = visibilityStampOf(wasm, bytes);
  if (incoming === null) return false;
  const row = await loadReachRow(accountIdB64, "visibility");
  const held = row === null ? null : visibilityStampOf(wasm, base64Decode(row));
  if (held === null || wasm.visibilityIsNewer(incoming, held)) {
    await saveReachRow(accountIdB64, "visibility", base64Encode(bytes));
  }
  return true;
}
