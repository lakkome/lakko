// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 1-byte kind discriminator for messages a Lakko mailbox
 * carries (Phase 1.16e3).
 *
 * Up to 1.16d3d, mailbox payloads were always
 * `lakko-contacts::ContactRequest` wire bytes. With 1.16e3 a
 * second message kind appears — `Welcome`, the MLS
 * group-join blob (RFC 9420 §12.4.3) — and the receiver needs
 * to know which decoder to reach for.
 *
 * The simplest non-fragile discriminator: prepend a single
 * byte to every mailbox payload. Receivers strip the byte,
 * dispatch to the right decoder, and warn-and-drop on
 * unknown kinds.
 *
 * The wrapping happens **here** on the TypeScript side rather
 * than inside `lakko-contacts` because:
 *
 *   1. `Welcome` is an MLS concept owned by `lakko-mls`, while
 *      `ContactRequest` is owned by `lakko-contacts` — neither
 *      of them is the natural home for a cross-format
 *      envelope.
 *   2. Phase-1 there are no UniFFI clients yet, so the
 *      discriminator only has to be agreed between two TS
 *      callers (`sendContactRequest` / `sendWelcome` /
 *      `pollMailbox`). When iOS / Android come online we lift
 *      the constants into a shared Rust crate.
 *
 * The discriminator format is intentionally trivial — one
 * byte, no length prefix, no MAC — because:
 *
 *   * The DS server is opaque to it (publishes raw bytes).
 *   * The inner formats already carry their own integrity
 *     (ContactRequest is Ed25519-signed; MLS Welcome is
 *     authenticated by the sender's KeyPackage signature).
 *   * Adding a kind byte does not weaken either inner format
 *     — a flipped kind byte just makes the receiver dispatch
 *     to the wrong decoder, which then rejects the body.
 */

/** Discriminator: body is a `ContactRequest` wire blob
 *  (`compose` output from `lakko-ffi-wasm`'s contacts module). */
export const KIND_CONTACT_REQUEST = 0x01;

/** Discriminator: body is an MLS Welcome wire blob (output of
 *  `Group.addMember(...)` 's `welcome` field). */
export const KIND_WELCOME = 0x02;

/** Discriminator: body is a `ContactAck` wire blob (1.9b1 —
 *  recipient's accept-reply to the original ContactRequest's
 *  sender). */
export const KIND_CONTACT_ACK = 0x03;

/** Discriminated parse output. Unknown kinds surface so the
 *  caller can `console.warn` and skip rather than throw. */
export type ParsedEnvelope =
  | { kind: typeof KIND_CONTACT_REQUEST; body: Uint8Array }
  | { kind: typeof KIND_WELCOME; body: Uint8Array }
  | { kind: typeof KIND_CONTACT_ACK; body: Uint8Array }
  | { kind: "unknown"; rawKind: number; body: Uint8Array };

/** Wrap inner bytes with the given kind discriminator. The
 *  returned `Uint8Array` is fresh — caller may mutate. */
export function wrapEnvelope(
  kind:
    | typeof KIND_CONTACT_REQUEST
    | typeof KIND_WELCOME
    | typeof KIND_CONTACT_ACK,
  body: Uint8Array,
): Uint8Array {
  const out = new Uint8Array(body.length + 1);
  out[0] = kind;
  out.set(body, 1);
  return out;
}

/** Strip the leading kind byte and dispatch. Empty input is
 *  treated as `unknown` rather than throwing — Phase-1 the DS
 *  doesn't lower-bound payload size on read, and we want
 *  garbage to skip-and-log on the receiver, not crash. */
export function parseEnvelope(bytes: Uint8Array): ParsedEnvelope {
  if (bytes.length === 0) {
    return { kind: "unknown", rawKind: -1, body: bytes };
  }
  const k = bytes[0] ?? 0;
  const body = bytes.subarray(1);
  switch (k) {
    case KIND_CONTACT_REQUEST:
      return { kind: KIND_CONTACT_REQUEST, body };
    case KIND_WELCOME:
      return { kind: KIND_WELCOME, body };
    case KIND_CONTACT_ACK:
      return { kind: KIND_CONTACT_ACK, body };
    default:
      return { kind: "unknown", rawKind: k, body };
  }
}
