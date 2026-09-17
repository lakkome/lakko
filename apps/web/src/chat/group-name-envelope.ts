// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 §1 — recognising a group-name envelope in an application
 * plaintext. The twin of `profile/profile-envelope.ts`.
 *
 * A group name rides an MLS application message, and `parseBodyEnvelope`'s
 * lossy-UTF-8 legacy fallback would otherwise render it as a chat bubble
 * reading `lakko/group-name/v1` plus the stamp bytes and the name. The
 * receive loop runs this BEFORE the chat parser.
 *
 * Three outcomes, and the middle one is why this is not a boolean:
 *
 *   * `name` — the domain matched and the envelope is well-formed;
 *   * `malformed` — the domain matched but the envelope failed a bound. The
 *     core refuses it AS A WHOLE, and so does this: DROPPED, never stored,
 *     never handed to the chat parser;
 *   * `none` — not a group-name envelope; carry on to the chat parse.
 *
 * The verdict comes from the core's `decodeGroupNameEnvelope` and its
 * stable `groupNameError` code (`wrongDomain` is the only "not mine"
 * answer). No domain bytes are copied into this file.
 */

import type { StoredGroupName } from "./group-name-store";

export type GroupNameSniff =
  | { readonly kind: "none" }
  | { readonly kind: "malformed" }
  | { readonly kind: "name"; readonly value: StoredGroupName };

/** The one wasm export this needs, structurally. */
export interface GroupNameDecoder {
  decodeGroupNameEnvelope(bytes: Uint8Array): {
    readonly name: string;
    readonly updatedAtMs: bigint;
    free?(): void;
  };
}

/** First byte every `lakko/…` service-envelope domain starts with. A cheap
 *  skip for chat envelopes (JSON, `{`) and call signaling; never a verdict. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** Classify an application plaintext. Never throws. */
export function sniffGroupName(decoder: GroupNameDecoder, bytes: Uint8Array): GroupNameSniff {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return { kind: "none" };
  let decoded: ReturnType<GroupNameDecoder["decodeGroupNameEnvelope"]>;
  try {
    decoded = decoder.decodeGroupNameEnvelope(bytes);
  } catch (err) {
    const code =
      typeof err === "object" && err !== null
        ? (err as { groupNameError?: unknown }).groupNameError
        : undefined;
    // `wrongDomain` is "not a group name". Any other code means the domain
    // matched and the body was refused — drop it. A throw with no code at
    // all is not evidence of a group name; let the chat parser have it.
    return code === undefined || code === "wrongDomain" ? { kind: "none" } : { kind: "malformed" };
  }
  const value: StoredGroupName = { name: decoded.name, updatedAtMs: decoded.updatedAtMs };
  try {
    decoded.free?.();
  } catch {
    // Freeing is best-effort.
  }
  return { kind: "name", value };
}
