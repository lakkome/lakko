// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The order of the pins in All chats as this device draws it: the synced
 * `ConversationOrder` register, or - while a write of it is on its way - the
 * list that write carries (`docs/ux/navigation.md` 2.2.1: "The new order
 * applies on this device at once, on drop").
 *
 * The register is written through the DeviceGroup journal and applied locally
 * only once the publish returns, so without this a dropped pin would jump
 * back to its old place for the length of a round trip and then jump again.
 * The pending list is dropped when its write finishes, by which time the
 * register already holds it (or, on a failure, holds what it held, which is
 * then the truth again).
 */
import { createSignal } from "solid-js";

interface Pending {
  readonly token: number;
  readonly keys: readonly Uint8Array[];
}

const [pending, setPending] = createSignal<Pending | null>(null);
let nextToken = 0;

/** The keys to order the pins by: the write on its way, else the register. */
export function effectivePinOrderKeys(
  stored: { readonly groupIds: readonly Uint8Array[] } | null,
): readonly Uint8Array[] {
  const p = pending();
  if (p !== null) return p.keys;
  return stored?.groupIds ?? [];
}

/**
 * Write the order: shown at once, then published. A later write replaces an
 * earlier one's pending list; an earlier write finishing does not drop the
 * later one's.
 */
export async function writePinOrder(
  sync: { setOrder(groupIds: readonly Uint8Array[]): Promise<void> },
  keys: readonly Uint8Array[],
): Promise<void> {
  nextToken += 1;
  const token = nextToken;
  const copy = keys.map((k) => new Uint8Array(k));
  setPending({ token, keys: copy });
  try {
    await sync.setOrder(copy);
  } finally {
    if (pending()?.token === token) setPending(null);
  }
}
