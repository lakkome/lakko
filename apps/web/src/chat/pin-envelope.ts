// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 3 — recognising a shared-pin envelope in an application
 * plaintext. The twin of `group-name-envelope.ts`.
 *
 * A pin rides an MLS application message, and `parseBodyEnvelope`'s
 * lossy-UTF-8 legacy fallback would otherwise render it as a chat bubble
 * reading `lakko/pin/v1` plus binary. The receive loop runs this BEFORE the
 * chat parser.
 *
 * Three outcomes, and the middle one is why this is not a boolean:
 *
 *   * `pin` — the domain matched and the envelope is well-formed;
 *   * `malformed` — the domain matched but the envelope failed a bound
 *     (length, version, action, an id past what a DS id can be). DROPPED,
 *     never stored, never handed to the chat parser;
 *   * `none` — not a pin envelope; carry on to the chat parse.
 *
 * The verdict comes from the core's `decodePinEnvelope` and its stable
 * `pinError` code (`wrongDomain` is the only "not mine" answer). No domain
 * bytes are copied into this file.
 */

import type { IncomingPin } from "./pin-store";

export type PinSniff =
  | { readonly kind: "none" }
  | { readonly kind: "malformed" }
  | { readonly kind: "pin"; readonly value: IncomingPin };

/** The one wasm export this needs, structurally. */
export interface PinDecoder {
  decodePinEnvelope(bytes: Uint8Array): {
    readonly messageId: bigint;
    readonly pinned: boolean;
    readonly updatedAtMs: bigint;
    free?(): void;
  };
}

/** First byte every `lakko/…` service-envelope domain starts with. A cheap
 *  skip for chat envelopes (JSON, `{`) and call signaling; never a verdict. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** Classify an application plaintext. Never throws. */
export function sniffPin(decoder: PinDecoder, bytes: Uint8Array): PinSniff {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return { kind: "none" };
  let decoded: ReturnType<PinDecoder["decodePinEnvelope"]>;
  try {
    decoded = decoder.decodePinEnvelope(bytes);
  } catch (err) {
    const code =
      typeof err === "object" && err !== null ? (err as { pinError?: unknown }).pinError : undefined;
    // `wrongDomain` is "not a pin". Any other code means the domain matched
    // and the body was refused — drop it. A throw with no code at all is not
    // evidence of a pin; let the chat parser have it.
    return code === undefined || code === "wrongDomain" ? { kind: "none" } : { kind: "malformed" };
  }
  const id = decoded.messageId;
  const pinned = decoded.pinned;
  const updatedAtMs = decoded.updatedAtMs;
  try {
    decoded.free?.();
  } catch {
    // Freeing is best-effort.
  }
  // A DS id is a JS number everywhere else in the client; an id this device
  // could never have seen is refused rather than rounded onto a real one.
  if (id < 0n || id > BigInt(Number.MAX_SAFE_INTEGER)) return { kind: "malformed" };
  return { kind: "pin", value: { messageId: Number(id), pinned, updatedAtMs } };
}
