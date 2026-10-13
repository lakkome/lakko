// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account's username as its own devices agree on it (ADR-0076): the
 * own-username register of the synced state (`lakko-sync`), stored with that
 * state, sent to the account's other devices over the DeviceGroup, and in
 * the backup. `DeviceGroupSyncProvider` owns the state and installs its port
 * here; the auth layer reads the register and writes it.
 *
 * What a device does about the register is one rule,
 * {@link usernameRegisterAction}: a device with a name auth confirmed as the
 * account's, and an empty register, seeds it once; a register that names another name than the device's local
 * marker is adopted (the account was renamed on another device). A write
 * this device made that has not reached the synced state yet is never
 * overruled by what the state held before it.
 */
import { createSignal } from "solid-js";

export interface UsernameRegisterPort {
  readonly accountIdB64: string;
  /** Write the register and send it to the account's other devices. Never
   *  throws. */
  readonly set: (username: string) => Promise<void>;
}

interface Held {
  readonly accountIdB64: string;
  /** The register's name, or `null` when it was never written (or names
   *  none). */
  readonly username: string | null;
}

let port: UsernameRegisterPort | null = null;
const [held, setHeld] = createSignal<Held | null>(null);
const [pending, setPending] = createSignal<{ readonly accountIdB64: string; readonly username: string } | null>(
  null,
);

/** What the synced state of `accountIdB64` holds: `undefined` while that
 *  state is not loaded, `null` for an empty register. Reactive. */
export function registerUsername(accountIdB64: string | null): string | null | undefined {
  const h = held();
  return h !== null && h.accountIdB64 === accountIdB64 ? h.username : undefined;
}

/** Whether a write of this device is still waiting for the synced state. */
export function usernameWritePending(accountIdB64: string | null): boolean {
  const p = pending();
  return p !== null && p.accountIdB64 === accountIdB64;
}

/** The register as it is now (the provider calls this on every fold). */
export function publishRegisterUsername(accountIdB64: string, username: string | null): void {
  const current = held();
  if (current === null || current.accountIdB64 !== accountIdB64 || current.username !== username) {
    setHeld({ accountIdB64, username });
  }
}

/** The synced state is loaded: a waiting write is applied now. */
export function installUsernameRegister(next: UsernameRegisterPort): () => void {
  port = next;
  const waiting = pending();
  if (waiting !== null && waiting.accountIdB64 === next.accountIdB64) {
    void next.set(waiting.username).finally(() => setPending(null));
  }
  return () => {
    if (port === next) port = null;
    setHeld(null);
  };
}

/** Write `username` as the account's name (a claim or a rename made here,
 *  or the one-time seed). Applied at once when the synced state is loaded,
 *  else when it is. */
export function rememberUsername(accountIdB64: string, username: string): void {
  if (port !== null && port.accountIdB64 === accountIdB64) {
    setPending(null);
    void port.set(username);
    return;
  }
  setPending({ accountIdB64, username });
}

export function dropPendingUsername(): void {
  setPending(null);
}

/** What a device does about the register. */
export type UsernameRegisterAction =
  /** Nothing to do. */
  | { readonly kind: "nothing" }
  /** The device has a name and the register is empty (an account from
   *  before the register existed): write the device's name, once. */
  | { readonly kind: "seed"; readonly username: string }
  /** The register names another name: the account was renamed (or its name
   *  chosen) on another device. Adopt it. */
  | { readonly kind: "adopt"; readonly username: string };

export function usernameRegisterAction(input: {
  /** The device's local account marker (`""` for none). */
  readonly local: string;
  /** The register: `undefined` while the synced state is not loaded. */
  readonly register: string | null | undefined;
  /** A write of this device has not reached the synced state yet. */
  readonly writePending: boolean;
  /** Auth confirmed in this page that the account holds the handle of the
   *  local name (the handle check answered `same`, or this device bound
   *  it). Only then may the device seed: a device that still holds an OLD
   *  name, and has not received the rename's register yet, would otherwise
   *  write its stale name with a newer stamp and undo the rename. */
  readonly localConfirmed: boolean;
}): UsernameRegisterAction {
  if (input.register === undefined || input.writePending) return { kind: "nothing" };
  const local = input.local.trim();
  const register = (input.register ?? "").trim();
  if (register.length === 0) {
    return local.length > 0 && input.localConfirmed ? { kind: "seed", username: local } : { kind: "nothing" };
  }
  return register === local ? { kind: "nothing" } : { kind: "adopt", username: register };
}
