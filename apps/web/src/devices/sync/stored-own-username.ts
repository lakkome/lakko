// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The own-username register (ADR-0076) as the STORED synced state holds it,
 * read without a running DeviceGroup runtime.
 *
 * A restore merges the backup's synced state into the stored one before any
 * DeviceGroup runs, and on a device restored with a passkey the session has
 * no name until that runtime runs - which needs the name to knock (ADR-0081
 * third amendment). Two readers need the register before then: the rejoin
 * knock (`DeviceGroupSyncContext`) and the landing's username step
 * (`auth/username-prompt.ts`).
 */
import type { DeviceCipher } from "../../auth/device-cipher";
import { loadWasm } from "../../wasm";
import { loadSyncState } from "./sync-state-idb";
import { openPersistedFold } from "./synced-projection";

/** The fold this needs: the register and its release. */
export interface OwnUsernameFold {
  ownUsername(): string | undefined;
  free(): void;
}

export interface StoredOwnUsernameDeps {
  /** The stored synced state's fold bytes, or `null` when none is stored. */
  readonly load: (accountIdB64: string, deviceKey: DeviceCipher) => Promise<Uint8Array | null>;
  /** Open the fold, or `null` when the bytes do not decode. */
  readonly open: (bytes: Uint8Array) => Promise<OwnUsernameFold | null>;
}

const DEFAULT_DEPS: StoredOwnUsernameDeps = {
  load: async (accountIdB64, deviceKey) => {
    const stored = await loadSyncState(accountIdB64, deviceKey);
    return stored === null || stored.synced.length === 0 ? null : stored.synced;
  },
  open: async (bytes) => openPersistedFold(await loadWasm(), bytes, "stored"),
};

/** The stored register's name, `""` when none is stored or it names none.
 *  Throws when the store cannot be read. */
export async function storedOwnUsername(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  deps: StoredOwnUsernameDeps = DEFAULT_DEPS,
): Promise<string> {
  const bytes = await deps.load(accountIdB64, deviceKey);
  if (bytes === null) return "";
  const fold = await deps.open(bytes);
  if (fold === null) return "";
  try {
    return (fold.ownUsername() ?? "").trim();
  } finally {
    fold.free();
  }
}
