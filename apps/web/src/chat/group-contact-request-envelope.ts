// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` section 6.4 - recognise a contact request
 * sealed to one member inside a group (ADR-0086 section 6), before the chat
 * parser: it is a `lakko/...` service envelope, never a bubble, never a
 * notification body. Gated by the codec's length bounds and the service-domain
 * first byte, so an ordinary chat message never pays for a wasm call.
 */

/** The part of the wasm namespace the sniff uses. */
export interface GroupContactRequestSniffWasm {
  groupContactRequestEnvelopeMinLen(): number;
  groupContactRequestEnvelopeMaxLen(): number;
  isGroupContactRequestEnvelope(bytes: Uint8Array): boolean;
}

/** First byte every `lakko/...` service-envelope domain starts with. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** `true` when `bytes` is a group contact request envelope. Never throws. */
export function isGroupContactRequestEnvelope(wasm: GroupContactRequestSniffWasm, bytes: Uint8Array): boolean {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return false;
  try {
    if (bytes.length < wasm.groupContactRequestEnvelopeMinLen()) return false;
    if (bytes.length > wasm.groupContactRequestEnvelopeMaxLen()) return false;
    return wasm.isGroupContactRequestEnvelope(bytes);
  } catch {
    return false;
  }
}
