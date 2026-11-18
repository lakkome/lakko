// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The three things a key rotation needs from the session's DeviceGroup handle
 * (ADR-0074 section 3.3), installed by `DeviceGroupSyncProvider` and used by
 * the auth layer and the rotation runtime, which sit above it:
 *
 *   - `epoch`: the handle's current epoch (read before the removal);
 *   - `post`: seal the `key-rotation` envelope AFTER the removal and publish
 *     it to the DeviceGroup journal, through the one ordering seam
 *     (`sealKeyRotationAfterRemoval`);
 *   - `catchUp`: read the journal to its head once (a superseded device with
 *     no pending envelope does this before it treats the change as a reset);
 *   - `loaded`: whether a DeviceGroup handle is loaded and polled. The
 *     provider is mounted for the whole page, so the port is always
 *     installed; a handle exists only inside a session.
 */

/** What reading the journal to its head came to. */
export type JournalCatchUp =
  /** Every row up to the head was fetched and applied. */
  | "read"
  /** The Delivery Service refused the read: nothing more can be learned. */
  | "unreadable"
  /** No answer (offline, a server error): ask again later. */
  | "offline"
  /** No DeviceGroup handle is loaded in this page (no session: a page load
   *  whose refresh was refused, the new-phrase screen after a reset). Not
   *  "offline": outside a session nothing will load one. */
  | "not-loaded";

export interface KeyRotationPort {
  readonly epoch: () => bigint | null;
  readonly post: (input: {
    readonly removedDeviceKey: Uint8Array;
    readonly preRemovalEpoch: bigint | null;
    readonly envelope: Uint8Array;
  }) => Promise<{ readonly epoch: bigint }>;
  readonly catchUp: () => Promise<JournalCatchUp>;
  readonly loaded: () => boolean;
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

/**
 * The journal read of a superseded device (ADR-0074 section 3.3, "read its
 * DeviceGroup journal to the head, once"). Inside a session the handle may
 * still be loading (a page that just restored its session), so it is
 * waited for up to `waitMs`; outside a session nothing will load it, so
 * nothing is waited for and the answer is `not-loaded` at once (the caller
 * then treats the change as a reset, as Android does after its one wait).
 */
export async function readJournalOnce(input: {
  readonly port: KeyRotationPort | null;
  readonly inSession: boolean;
  readonly waitMs: number;
  readonly stepMs?: number;
  readonly sleep?: (ms: number) => Promise<void>;
  readonly now?: () => number;
}): Promise<JournalCatchUp> {
  const port = input.port;
  if (!input.inSession || port === null) return "not-loaded";
  const now = input.now ?? Date.now;
  const sleep = input.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const deadline = now() + input.waitMs;
  while (!port.loaded()) {
    if (now() >= deadline) return "not-loaded";
    await sleep(input.stepMs ?? 250);
  }
  return port.catchUp();
}
