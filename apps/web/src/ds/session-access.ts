// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 contract points 1 + 2 for the account's OWN mailboxes.
 *
 * ADR-0078: a person has ONE inbound mailbox, named after the account's
 * MAILBOX key (derived from the account seed, so every device of the account
 * derives the same key and address), and a claimed one answers a read only
 * when it is proven under that key. There is no device mailbox any more:
 * nothing claims or reads one. The mailboxes of own predecessor keys (after
 * an ADR-0074 rotation) are read too, each derived from its own seed.
 *
 * `installMailboxAccess` is called whenever an account identity is set (every
 * session landing: login, signup, restore, recover, guest) and:
 *
 *   1. installs the identity's mailbox signer in the registry, so the DS client
 *      signs reads of that mailbox, and drops the signer of the identity it
 *      replaced;
 *   2. claims the mailbox (`PUT /ds/mailbox/<hex>/access`, body
 *      `{"mailbox_pk_b64"}`), fired and not awaited: a failed claim is logged
 *      and retried by the next refused read, and never blocks the UI.
 */
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { loadWasm } from "../wasm";
import { dsAccess } from "./access";
import { DsClient } from "./client";

/** `predecessor` (ADR-0074 section 3.5): the mailbox of the account key the
 *  conversation leaves are still bound to after a proven key rotation. It is
 *  read for as long as leaves name it. */
type Slot = "account" | "predecessor";

const SLOTS: readonly Slot[] = ["account", "predecessor"];
const installed: Record<Slot, string | null> = { account: null, predecessor: null };
const generation: Record<Slot, number> = { account: 0, predecessor: 0 };

/** ADR-0074 section 3.5a - the mailboxes of the INTERMEDIATE own
 *  predecessors (the keys held between two rotations), read inside their own
 *  30-day windows. A list, not a slot: there is one per earlier rotation. */
let chainInstalled: string[] = [];
let chainGeneration = 0;

function stillUsed(hex: string, exceptSlot: Slot | null): boolean {
  return (
    SLOTS.some((other) => other !== exceptSlot && installed[other] === hex) ||
    chainInstalled.includes(hex)
  );
}

function dropSlot(slot: Slot): void {
  const hex = installed[slot];
  installed[slot] = null;
  if (hex === null) return;
  // A predecessor and the chain can name one mailbox for a moment during a
  // rotation: keep the signer while another slot still uses it.
  if (stillUsed(hex, slot)) return;
  dsAccess.dropMailbox(hex);
}

/**
 * Install (or, with `null`, remove) the mailbox signer of one of the session's
 * ACCOUNT identities (the current one or the conversation predecessor), and
 * claim that mailbox. Never pass a device identity: ADR-0078 removed device
 * mailboxes. Never throws.
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
      const held = dsAccess.mailboxSigner(signer.mailboxAddress());
      if (held === null) return;
      await new DsClient().claimMailbox(held);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`ds: ${slot} mailbox claim failed; retried on the next refused read`, err);
    }
  })();
}

/**
 * Install the mailbox signers of the intermediate own predecessors (ADR-0074
 * section 3.5a rule 4), replacing the previous list. The mailboxes are READ
 * only; the claim is the idempotent one this key already made when it was
 * the account's. Never throws.
 */
export function installPredecessorChainAccess(identities: readonly WasmIdentity[]): void {
  chainGeneration += 1;
  const mine = chainGeneration;
  const previous = chainInstalled;
  chainInstalled = [];
  for (const hex of previous) {
    if (!stillUsed(hex, null)) dsAccess.dropMailbox(hex);
  }
  if (identities.length === 0) return;
  void (async (): Promise<void> => {
    try {
      const wasm = await loadWasm();
      for (const identity of identities) {
        if (chainGeneration !== mine) return;
        const signer = wasm.dsMailboxSigner(identity);
        chainInstalled.push(dsAccess.setMailboxSigner(signer));
        const held = dsAccess.mailboxSigner(signer.mailboxAddress());
        if (held === null) continue;
        try {
          await new DsClient().claimMailbox(held);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("ds: predecessor mailbox claim failed; retried on the next refused read", err);
        }
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ds: predecessor mailbox signers not installed", err);
    }
  })();
}

/** Sign-out: forget every mailbox and group signer. */
export function clearDsAccess(): void {
  chainGeneration += 1;
  chainInstalled = [];
  for (const slot of SLOTS) {
    generation[slot] += 1;
    installed[slot] = null;
  }
  dsAccess.clear();
}
