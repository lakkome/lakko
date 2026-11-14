// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 1 — a PublisherGroup leaf is this DEVICE: the ADR-0050
 * bound device identity that holds the device's conversation leaves
 * (`chats.identity()`), never the account identity. The group lives in that
 * identity's provider, so it is restored and persisted with the device's
 * conversation snapshot (`chat/mls-store.ts`), and a device admitted by a
 * sibling opens its Welcome with the KeyPackage it published to the
 * directory from that same provider.
 *
 * The account identity (`chats.conversationAccountIdentity()`, here called
 * `lineage`) stays what it was for everything that is about the account: the
 * owner's account seed the group id and the owner key derive from, the
 * channel proofs, the journal root, the envelopes sealed to the account.
 *
 * The session installs where the device identity comes from
 * (`ChannelsHydration`); a channel path asks for it when it opens a group.
 */

import type { Wasm } from "../wasm";

type Identity = InstanceType<Wasm["Identity"]>;

/** No device identity is installed (signed out, or the session is still
 *  starting): nothing is opened, nothing is founded. */
export class PublisherDeviceUnavailableError extends Error {
  override name = "PublisherDeviceUnavailableError";
  constructor() {
    super("the device identity is not available");
  }
}

let source: (() => Identity | null) | null = null;

/** Install (or with `null` withdraw) where the bound device identity comes
 *  from. Returns the uninstaller. */
export function installPublisherDeviceSource(fn: (() => Identity | null) | null): () => void {
  source = fn;
  return () => {
    if (source === fn) source = null;
  };
}

/** The device identity, or `null` when none is installed. */
export function publisherDeviceOrNull(): Identity | null {
  try {
    return source?.() ?? null;
  } catch {
    return null;
  }
}

/** The device identity; throws {@link PublisherDeviceUnavailableError}. */
export function publisherDevice(): Identity {
  const d = publisherDeviceOrNull();
  if (d === null) throw new PublisherDeviceUnavailableError();
  return d;
}

/**
 * Whether `device` carries an ADR-0050 binding to the account whose key is
 * `accountKey` (the account identity's signature key): only then may it hold
 * a leaf of a PublisherGroup of that account. Pure over the bytes.
 */
export function deviceIsBoundTo(deviceAccountKey: Uint8Array | undefined, accountKey: Uint8Array): boolean {
  if (deviceAccountKey === undefined || deviceAccountKey.length !== accountKey.length) return false;
  for (let i = 0; i < accountKey.length; i += 1) if (deviceAccountKey[i] !== accountKey[i]) return false;
  return true;
}
