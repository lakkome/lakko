// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The moderator list as the panel uses it: its version (every statement
 * names it), a check under the pinned root key when this build pins one,
 * and whether the signed-in key's report key is the one the list holds.
 */

import { api } from "./api/client";
import type { Moderators } from "./api/types";
import { b64ToBytes, bytesEqual } from "./model/bytes";
import { requireSession } from "./session";
import { loadWasm } from "./wasm";

export type ListCheck =
  | { readonly kind: "verified"; readonly version: number }
  | { readonly kind: "unpinned" }
  | { readonly kind: "failed"; readonly message: string };

export interface ModeratorState {
  readonly list: Moderators;
  readonly check: ListCheck;
  /** `false` when the signed-in key's report key is not the listed one. */
  readonly reportKeyMatches: boolean | null;
}

/** The pinned root key (base64url), or `null` when this build pins none. */
export function pinnedRootKey(): Uint8Array | null {
  const raw = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_LAKKO_MODERATION_ROOT_KEY;
  if (raw === undefined || raw.trim() === "") return null;
  try {
    const key = b64ToBytes(raw.trim());
    return key.length === 32 ? key : null;
  } catch {
    return null;
  }
}

export async function loadModerators(): Promise<ModeratorState> {
  const list = await api.moderators();
  const wasm = await loadWasm();
  const root = pinnedRootKey();
  let check: ListCheck = { kind: "unpinned" };
  if (root !== null) {
    try {
      const verified = wasm.verifyModeratorList(b64ToBytes(list.list_b64), root);
      const version = Number(verified.listVersion);
      verified.free();
      check =
        version === list.list_version
          ? { kind: "verified", version }
          : { kind: "failed", message: "The list's signed version differs from what the service reports." };
    } catch (e) {
      check = { kind: "failed", message: e instanceof Error ? e.message : String(e) };
    }
  }
  let reportKeyMatches: boolean | null = null;
  const s = requireSession();
  const me = list.moderators.find((m) => m.moderator_id_b64 === s.moderatorIdB64);
  if (me !== undefined) {
    try {
      const derived = wasm.moderatorReportPublicKey(s.reportSecret);
      reportKeyMatches = bytesEqual(derived, b64ToBytes(me.x25519_public_b64));
    } catch {
      reportKeyMatches = false;
    }
  }
  return { list, check, reportKeyMatches };
}
