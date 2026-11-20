// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 section 1 - attested chat messages on the web client.
 *
 * A chat plaintext is `lakko/attested-message/v1\0 || 0x01 || sig(64) ||
 * envelope`, signed by the sending device's conversation leaf key over the
 * group id and the envelope. The core owns the format; this module owns the
 * two client rules around it:
 *
 * - SEND: which outgoing envelopes are wrapped. Every chat, edit, delete and
 *   reaction envelope (a reply and a forward are chat envelopes); never a
 *   typing signal, a read receipt or a `lakko/...` service envelope. The
 *   caller says which kind it sends ({@link attestOutgoing}'s `attest`), and a
 *   service envelope is never wrapped whatever the caller says.
 * - RECEIVE: the envelope inside ({@link unwrapReceived}) goes to the chat
 *   parser exactly as before, and the evidence a report needs is captured AT
 *   RECEIPT ({@link evidenceAtReceipt}): the verdict, the raw attested bytes,
 *   the sender leaf key and the leaf's device binding. At receipt because a
 *   leaf is reused after a commit (C-68), so a binding read later can belong
 *   to somebody else.
 *
 * A failed or absent attestation drops nothing: the message shows as it
 * always did. The verdict is only what a moderator later sees.
 */

import { base64Decode, base64Encode } from "../auth/account-id";

/** What a received plaintext's signature shows. */
export type AttestationVerdict = "verified" | "failed" | "absent";

/** The wasm functions this module needs, structurally (tests pass fakes). */
export interface AttestationWasm {
  attestMessage(identity: never, groupId: Uint8Array, envelope: Uint8Array): Uint8Array;
  unwrapAttested(plaintext: Uint8Array): Uint8Array;
  messageAttestation(plaintext: Uint8Array, groupId: Uint8Array, senderLeafKey: Uint8Array): string;
}

/** The prefix every Lakko service envelope opens with. */
const SERVICE_PREFIX = new TextEncoder().encode("lakko/");

/** `true` when `bytes` open with `lakko/`: a service envelope, never wrapped. */
export function isServiceEnvelopeBytes(bytes: Uint8Array): boolean {
  if (bytes.length < SERVICE_PREFIX.length) return false;
  for (let i = 0; i < SERVICE_PREFIX.length; i += 1) {
    if (bytes[i] !== SERVICE_PREFIX[i]) return false;
  }
  return true;
}

/**
 * The plaintext to encrypt for an outgoing application message. `attest` is
 * the caller's statement that `plaintext` is a chat, edit, delete or reaction
 * envelope; a service envelope is returned unchanged whatever it says. A
 * failure of the core (a group id it refuses) throws: the send does not go
 * out half-described.
 */
export function attestOutgoing<I>(
  wasm: { attestMessage(identity: I, groupId: Uint8Array, envelope: Uint8Array): Uint8Array },
  identity: I,
  groupId: Uint8Array,
  plaintext: Uint8Array,
  attest: boolean,
): Uint8Array {
  if (!attest || isServiceEnvelopeBytes(plaintext)) return plaintext;
  return wasm.attestMessage(identity, groupId, plaintext);
}

/**
 * The envelope inside a received plaintext, or the plaintext itself when it
 * is not attested. A malformed wrapper stays whole and so classifies as a
 * service envelope downstream, which is dropped as service envelopes are.
 * Never throws: a core that cannot answer leaves the bytes as they are.
 */
export function unwrapReceived(
  wasm: Pick<AttestationWasm, "unwrapAttested"> | null,
  plaintext: Uint8Array,
): Uint8Array {
  if (wasm === null) return plaintext;
  try {
    return wasm.unwrapAttested(plaintext);
  } catch {
    return plaintext;
  }
}

/** What a report needs of one received message, stored with it at receipt. */
export interface MessageEvidence {
  readonly attestation: AttestationVerdict;
  /** The raw attested plaintext (standard base64), as decrypted. */
  readonly plaintextB64: string;
  /** The sender's MLS leaf signature key (standard base64), read at receipt. */
  readonly leafKeyB64: string;
  /** The leaf's ADR-0050 device binding (standard base64), read at receipt,
   *  or `null` for a leaf that carries none. */
  readonly bindingB64: string | null;
}

/** Read one verdict string into the closed set; anything else is `absent`. */
export function verdictOf(raw: string): AttestationVerdict {
  return raw === "verified" || raw === "failed" ? raw : "absent";
}

/**
 * The evidence of one received message, or `undefined` when there is no
 * sender leaf key to record it under (a control frame, a torn roster read).
 */
export function evidenceAtReceipt(
  wasm: Pick<AttestationWasm, "messageAttestation"> | null,
  raw: Uint8Array,
  groupId: Uint8Array,
  leafKey: Uint8Array | undefined,
  binding: Uint8Array | undefined,
): MessageEvidence | undefined {
  if (leafKey === undefined || leafKey.length !== 32) return undefined;
  let attestation: AttestationVerdict = "absent";
  if (wasm !== null) {
    try {
      attestation = verdictOf(wasm.messageAttestation(raw, groupId, leafKey));
    } catch {
      attestation = "absent";
    }
  }
  return {
    attestation,
    plaintextB64: base64Encode(raw),
    leafKeyB64: base64Encode(leafKey),
    bindingB64: binding === undefined || binding.length === 0 ? null : base64Encode(binding),
  };
}

/** A stored evidence row read back, `undefined` when it is malformed. */
export function evidenceFromPersisted(value: unknown): MessageEvidence | undefined {
  if (value === null || typeof value !== "object") return undefined;
  const v = value as Record<string, unknown>;
  if (typeof v.plaintextB64 !== "string" || typeof v.leafKeyB64 !== "string") return undefined;
  const binding = typeof v.bindingB64 === "string" ? v.bindingB64 : null;
  return {
    attestation: verdictOf(typeof v.attestation === "string" ? v.attestation : ""),
    plaintextB64: v.plaintextB64,
    leafKeyB64: v.leafKeyB64,
    bindingB64: binding,
  };
}

/** The bytes a report's message evidence carries, or `null` when any is
 *  unreadable (the message is then left out of the report). */
export function evidenceBytes(e: MessageEvidence): {
  readonly plaintext: Uint8Array;
  readonly leafKey: Uint8Array;
  readonly binding: Uint8Array;
} | null {
  try {
    return {
      plaintext: base64Decode(e.plaintextB64),
      leafKey: base64Decode(e.leafKeyB64),
      binding: e.bindingB64 === null ? new Uint8Array(0) : base64Decode(e.bindingB64),
    };
  } catch {
    return null;
  }
}
