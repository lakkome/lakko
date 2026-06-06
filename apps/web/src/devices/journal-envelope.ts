// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * DeviceGroup journal envelope (G-13, ADR-0015 sprint 5).
 *
 * Every DeviceGroup DS-journal entry is wrapped in this envelope so that a
 * Remove commit can carry the peer-verification *attestation* alongside the
 * MLS message it belongs to. Peer devices, on reading the journal, unwrap
 * the envelope and feed `(message, attestation)` to
 * `DeviceGroup.processIncomingVerifyingRemovals` — which refuses a removal
 * that the group's policy requires to be attested but isn't.
 *
 * Layout (all integers big-endian):
 *
 * ```text
 *   version      : u8  = 1
 *   attestation_len : u32
 *   attestation  : attestation_len bytes   (the ActionAttestation; 0 = none)
 *   message      : the remaining bytes      (the MLS wire message)
 * ```
 *
 * Non-remove entries (device adds, application messages, the policy-set
 * commit itself) carry `attestation_len = 0`. The wrap is applied at every
 * DeviceGroup `publishGroup` site and unwrapped at the single DeviceGroup
 * receive loop, so the format is internal to the DeviceGroup journal and
 * does not affect any other MLS group's traffic.
 *
 * There is intentionally no raw-message fallback: the DeviceGroup journal is
 * pre-launch, so every entry is enveloped from the start.
 */

/** Current envelope version. */
const ENVELOPE_VERSION = 1;

/** Fixed header size: 1 version byte + 4 length bytes. */
const HEADER_LEN = 5;

/** A decoded DeviceGroup journal entry. */
export interface JournalEntry {
  /** The MLS wire message (commit / welcome-less commit / application). */
  readonly message: Uint8Array;
  /** The removal attestation, or `null` when the entry carries none. */
  readonly attestation: Uint8Array | null;
}

/**
 * Wrap an MLS message (and an optional removal attestation) into a
 * DeviceGroup journal envelope ready to `publishGroup`.
 */
export function encodeJournalEntry(
  message: Uint8Array,
  attestation: Uint8Array | null,
): Uint8Array {
  const attLen = attestation ? attestation.length : 0;
  const out = new Uint8Array(HEADER_LEN + attLen + message.length);
  out[0] = ENVELOPE_VERSION;
  // attestation_len as big-endian u32.
  new DataView(out.buffer).setUint32(1, attLen, false);
  if (attestation) {
    out.set(attestation, HEADER_LEN);
  }
  out.set(message, HEADER_LEN + attLen);
  return out;
}

/**
 * Unwrap a DeviceGroup journal envelope produced by
 * [`encodeJournalEntry`].
 *
 * @throws if the bytes are too short, the version is unknown, or the
 *   attestation length overflows the buffer.
 */
export function decodeJournalEntry(bytes: Uint8Array): JournalEntry {
  if (bytes.length < HEADER_LEN) {
    throw new Error("DeviceGroup journal entry is shorter than the envelope header");
  }
  if (bytes[0] !== ENVELOPE_VERSION) {
    throw new Error(`unsupported DeviceGroup journal envelope version ${bytes[0]}`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const attLen = view.getUint32(1, false);
  if (HEADER_LEN + attLen > bytes.length) {
    throw new Error("DeviceGroup journal entry attestation length overflows the buffer");
  }
  const attestation = attLen > 0 ? bytes.subarray(HEADER_LEN, HEADER_LEN + attLen) : null;
  const message = bytes.subarray(HEADER_LEN + attLen);
  return { message, attestation };
}
