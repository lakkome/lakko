// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 - recognise a registrar-seed envelope among a group's application
 * plaintexts, before the chat parser: the envelope is a `lakko/...` service
 * envelope, and the parser's lossy-UTF-8 fallback would otherwise render it as
 * a bubble (and leak nothing, since the seed is sealed, but still be wrong).
 *
 * Classification goes through the core's `dispatchIncomingEnvelope` (kind
 * `RegistrarSeed`), gated by the one fixed length the codec admits
 * (`registrarSeedEnvelopeLen()`, 173 bytes) and the service-domain first byte,
 * so an ordinary chat message never pays for a wasm call.
 */

/** The part of the wasm namespace the sniff uses. */
export interface RegistrarSeedSniffWasm {
  registrarSeedEnvelopeLen(): number;
  dispatchIncomingEnvelope(plaintext: Uint8Array): { kind: number; free?(): void };
  readonly IncomingEnvelopeKind: { readonly RegistrarSeed: number };
}

/** First byte every `lakko/...` service-envelope domain starts with. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** `true` when `bytes` is a registrar-seed envelope (well-formed enough for
 *  the core to classify it as one). Never throws. */
export function isRegistrarSeedEnvelope(wasm: RegistrarSeedSniffWasm, bytes: Uint8Array): boolean {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return false;
  let expected: number;
  try {
    expected = wasm.registrarSeedEnvelopeLen();
  } catch {
    return false;
  }
  if (bytes.length !== expected) return false;
  let env: { kind: number; free?(): void };
  try {
    env = wasm.dispatchIncomingEnvelope(bytes);
  } catch {
    return false;
  }
  try {
    return env.kind === wasm.IncomingEnvelopeKind.RegistrarSeed;
  } finally {
    try {
      env.free?.();
    } catch {
      /* best effort */
    }
  }
}
