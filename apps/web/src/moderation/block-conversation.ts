// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 6 - the block rule applied to one live conversation: who
 * the other person of a one-to-one is, and whether this device withholds its
 * signals from it. The rule is `block-rules.ts`; this is the read of the
 * group it needs.
 */

import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { conversationKindOf } from "../chat/conversation-kind";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { wasmSync } from "../wasm";
import { withholdsSignal } from "./block-rules";

/** The other person of a one-to-one (account key, standard base64), or `null`
 *  for a group, a torn roster, or a one-to-one with this account alone. */
export function oneToOnePeer(group: WasmGroup, identity: WasmIdentity | null): string | null {
  if (conversationKindOf(group) !== "chat") return null;
  const own = ownPersonKeyB64(identity);
  return personKeysOf(group).find((k) => k !== own) ?? null;
}

/** Whether a typing signal, a read receipt or a profile update to `group` is
 *  withheld: the one-to-one with a blocked person. Never throws. */
export function signalsWithheldFrom(group: WasmGroup, identity: WasmIdentity | null): boolean {
  try {
    const peer = oneToOnePeer(group, identity);
    if (peer === null) return false;
    return withholdsSignal(wasmSync(), "one-to-one", peer);
  } catch {
    return false;
  }
}
