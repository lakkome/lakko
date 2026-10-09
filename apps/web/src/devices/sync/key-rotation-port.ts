// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The three things a key rotation needs from the session's DeviceGroup handle
 * (ADR-0066 section 3.3), installed by `DeviceGroupSyncProvider` and used by
 * the auth layer and the rotation runtime, which sit above it:
 *
 *   - `epoch`: the handle's current epoch (read before the removal);
 *   - `post`: seal the `key-rotation` envelope AFTER the removal and publish
 *     it to the DeviceGroup journal, through the one ordering seam
 *     (`sealKeyRotationAfterRemoval`);
 *   - `catchUp`: read the journal to its head once (a superseded device with
 *     no pending envelope does this before it treats the change as a reset).
 */

/** What reading the journal to its head came to. */
export type JournalCatchUp =
  /** Every row up to the head was fetched and applied. */
  | "read"
  /** The Delivery Service refused the read: nothing more can be learned. */
  | "unreadable"
  /** No answer (offline, a server error): ask again later. */
  | "offline";

export interface KeyRotationPort {
  readonly epoch: () => bigint | null;
  readonly post: (input: {
    readonly removedDeviceKey: Uint8Array;
    readonly preRemovalEpoch: bigint | null;
    readonly envelope: Uint8Array;
  }) => Promise<{ readonly epoch: bigint }>;
  readonly catchUp: () => Promise<JournalCatchUp>;
}

let installed: KeyRotationPort | null = null;
const waiters = new Set<(port: KeyRotationPort | null) => void>();

export function installKeyRotationPort(port: KeyRotationPort): () => void {
  installed = port;
  for (const wake of [...waiters]) wake(port);
  return () => {
    if (installed === port) installed = null;
  };
}

export function keyRotationPort(): KeyRotationPort | null {
  return installed;
}

/** The port, waiting up to `timeoutMs` for the DeviceGroup handle to load
 *  (a page that just restored its session). `null` when it did not. */
export function awaitKeyRotationPort(timeoutMs: number): Promise<KeyRotationPort | null> {
  if (installed !== null) return Promise.resolve(installed);
  return new Promise((resolve) => {
    const wake = (port: KeyRotationPort | null): void => {
      clearTimeout(timer);
      waiters.delete(wake);
      resolve(port);
    };
    const timer = setTimeout(() => wake(installed), timeoutMs);
    waiters.add(wake);
  });
}
