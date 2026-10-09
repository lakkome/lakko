// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 contract points 1 + 2 for the device's OWN mailboxes.
 *
 * A session holds two identities (ADR-0050): the phrase-seeded ACCOUNT
 * identity, whose mailbox receives contact requests and acks, and the DEVICE
 * identity, whose mailbox receives the Welcomes sealed to this device's
 * KeyPackages. Each mailbox is named after its identity key, and since
 * ADR-0053 a claimed one answers a read only when it is proven under that key.
 *
 * `installMailboxAccess` is called whenever either identity is set (every
 * session landing: login, signup, restore, recover, guest) and:
 *
 *   1. installs the identity's mailbox signer in the registry, so the DS client
 *      signs reads of that mailbox, and drops the signer of the identity it
 *      replaced;
 *   2. claims the mailbox (`PUT /ds/mailbox/<hex>/access`), fired and not
 *      awaited: a failed claim is logged and retried by the next refused read,
 *      and never blocks the UI.
 */
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { loadWasm } from "../wasm";
import { dsAccess } from "./access";
import { DsClient } from "./client";

/** `predecessor` (ADR-0066 section 3.5): the mailbox of the account key the
 *  conversation leaves are still bound to after a proven key rotation. It is
 *  read for as long as leaves name it. */
type Slot = "account" | "device" | "predecessor";

const SLOTS: readonly Slot[] = ["account", "device", "predecessor"];
const installed: Record<Slot, string | null> = { account: null, device: null, predecessor: null };
const generation: Record<Slot, number> = { account: 0, device: 0, predecessor: 0 };

function dropSlot(slot: Slot): void {
  const hex = installed[slot];
  installed[slot] = null;
  if (hex === null) return;
  // The device identity falls back to a second instance of the account seed
  // when no device key exists, so both slots can name one mailbox: keep the
  // signer while the other slot still uses it.
  if (SLOTS.some((other) => other !== slot && installed[other] === hex)) return;
  dsAccess.dropMailbox(hex);
}

/**
 * Install (or, with `null`, remove) the mailbox signer of one of the session's
 * identities, and claim that mailbox. Never throws.
 */
export function installMailboxAccess(slot: Slot, identity: WasmIdentity | null): void {
  generation[slot] += 1;
  const mine = generation[slot];
  dropSlot(slot);
  if (identity === null) return;
  void (async (): Promise<void> => {
    try {
      const wasm = await loadWasm();
      // A later install (or a sign-out) superseded this one while wasm was
      // resolving; the identity may already be freed.
      if (generation[slot] !== mine) return;
      const signer = wasm.dsMailboxSigner(identity);
      installed[slot] = dsAccess.setMailboxSigner(signer);
      // `claimMailbox` takes the registry's own signer by reference; it is
      // used synchronously inside the call, before any await.
      const held = dsAccess.mailboxSigner(signer.mailboxPseudonym());
      if (held === null) return;
      await new DsClient().claimMailbox(held);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`ds: ${slot} mailbox claim failed; retried on the next refused read`, err);
    }
  })();
}

/** Sign-out: forget every mailbox and group signer. */
export function clearDsAccess(): void {
  for (const slot of SLOTS) {
    generation[slot] += 1;
    installed[slot] = null;
  }
  dsAccess.clear();
}
