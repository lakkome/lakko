// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.6 — recognising a group-closed envelope (a successor CLAIM) in
 * an application plaintext. The twin of `group-name-envelope.ts`.
 *
 * The claim rides an MLS application message; without this arm ahead of
 * the chat parser it would reach `parseBodyEnvelope`, and the three
 * background filters must suppress it like every service envelope. Three
 * outcomes, same as the name:
 *
 *   * `closed` — the domain matched and the envelope is well-formed;
 *   * `malformed` — the domain matched but the envelope failed a bound:
 *     DROPPED, never stored, never handed to the chat parser;
 *   * `none` — not a group-closed envelope.
 *
 * The verdict comes from the core's `decodeGroupClosedEnvelope` and its
 * stable `groupClosedError` code (`wrongDomain` is the only "not mine"
 * answer). No domain bytes are copied into this file.
 */

import { base64Encode } from "../auth/account-id";

/** A group-closed envelope as decoded off the wire. It carries no author:
 *  the sender is the MLS-authenticated one, resolved at receipt (C-68). */
export interface GroupClosedNotice {
  /** Sender-clock UNIX milliseconds, as stamped in the envelope. Not used
   *  for ordering: the journal's order decides (R.6). */
  readonly closedAtMs: bigint;
  /** base64 of the successor group's id, or `null` when the envelope names
   *  none (then it is not a claim and is dropped). */
  readonly successorGroupIdB64: string | null;
}

export type GroupClosedSniff =
  | { readonly kind: "none" }
  | { readonly kind: "malformed" }
  | { readonly kind: "closed"; readonly value: GroupClosedNotice };

/** The one wasm export this needs, structurally. */
export interface GroupClosedDecoder {
  decodeGroupClosedEnvelope(bytes: Uint8Array): {
    readonly closedAtMs: bigint;
    readonly successorGroupId: Uint8Array | undefined;
    free?(): void;
  };
}

/** First byte every `lakko/…` service-envelope domain starts with. A cheap
 *  skip for chat envelopes (JSON, `{`) and call signaling; never a verdict. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** Classify an application plaintext. Never throws. */
export function sniffGroupClosed(decoder: GroupClosedDecoder, bytes: Uint8Array): GroupClosedSniff {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return { kind: "none" };
  let decoded: ReturnType<GroupClosedDecoder["decodeGroupClosedEnvelope"]>;
  try {
    decoded = decoder.decodeGroupClosedEnvelope(bytes);
  } catch (err) {
    const code =
      typeof err === "object" && err !== null
        ? (err as { groupClosedError?: unknown }).groupClosedError
        : undefined;
    return code === undefined || code === "wrongDomain" ? { kind: "none" } : { kind: "malformed" };
  }
  const successor = decoded.successorGroupId;
  const value: GroupClosedNotice = {
    closedAtMs: decoded.closedAtMs,
    successorGroupIdB64:
      successor === undefined || successor.length === 0 ? null : base64Encode(successor),
  };
  try {
    decoded.free?.();
  } catch {
    // Freeing is best-effort.
  }
  return { kind: "closed", value };
}
