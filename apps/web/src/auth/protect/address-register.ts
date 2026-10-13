// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery address as the account's own devices keep it (ADR-0075,
 * "Amended 2026-10-13: the account's devices keep the address").
 *
 * The address lives in ONE place: the recovery-email register of the synced
 * state (`lakko-sync`), which is stored encrypted under the device key with
 * the rest of that state, travels to the account's other devices over the
 * DeviceGroup, and rides the backup. `DeviceGroupSyncProvider` owns that
 * state and installs its port here; this module is how the screens above it
 * read the address and write it.
 *
 * A write made before the synced state is loaded (the landing that follows
 * a recovery by email, where the person just typed the address) waits in
 * memory and is applied when the port arrives. The address is never logged
 * and never written to plain storage.
 */
import { createSignal } from "solid-js";

/** The synced state's half, installed by `DeviceGroupSyncProvider`. */
export interface AddressRegisterPort {
  readonly accountIdB64: string;
  /** Write the register (`null`: the account has no recovery email) and
   *  send it to the account's other devices. Never throws. */
  readonly set: (address: string | null) => Promise<void>;
}

interface Pending {
  readonly accountIdB64: string;
  readonly address: string | null;
}

let port: AddressRegisterPort | null = null;
let pending: Pending | null = null;
const [held, setHeld] = createSignal<{ readonly accountIdB64: string; readonly address: string } | null>(null);

/** The address this device holds for `accountIdB64`, or `null`. Reactive. */
export function heldAddress(accountIdB64: string | null): string | null {
  const h = held();
  return h !== null && h.accountIdB64 === accountIdB64 ? h.address : null;
}

/** The synced state's register as it is now (the provider calls this on
 *  every fold). */
export function publishHeldAddress(accountIdB64: string, address: string | null): void {
  const current = held();
  if (address === null) {
    if (current !== null && current.accountIdB64 === accountIdB64) setHeld(null);
    return;
  }
  if (current === null || current.accountIdB64 !== accountIdB64 || current.address !== address) {
    setHeld({ accountIdB64, address });
  }
}

/** The synced state of `accountIdB64` is loaded: writes can go through. A
 *  write that was waiting for it is applied now. Returns the uninstaller. */
export function installAddressRegister(next: AddressRegisterPort): () => void {
  port = next;
  const waiting = pending;
  if (waiting !== null && waiting.accountIdB64 === next.accountIdB64) {
    pending = null;
    void next.set(waiting.address);
  }
  return () => {
    if (port === next) port = null;
    setHeld(null);
  };
}

/**
 * Keep `address` as the account's recovery address (`null`: it has none any
 * more). Applied at once when the synced state is loaded, else when it is.
 */
export function rememberAddress(accountIdB64: string, address: string | null): void {
  if (port !== null && port.accountIdB64 === accountIdB64) {
    pending = null;
    void port.set(address);
    return;
  }
  pending = { accountIdB64, address };
}

/** Forget a write that never reached the synced state (sign-out). */
export function dropPendingAddress(): void {
  pending = null;
}
