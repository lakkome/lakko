// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — recognising a profile envelope in an application plaintext.
 *
 * A profile rides an MLS application message, and `parseBodyEnvelope`'s
 * lossy-UTF-8 legacy fallback would otherwise render it as a chat bubble
 * reading `lakko/profile/v1` plus the length bytes, the sender's name and
 * username. The receive loop runs this BEFORE the chat parser.
 *
 * Three outcomes, and the middle one is the reason this is not a boolean:
 *
 *   * `profile` — the domain matched and the envelope is well-formed and in
 *     bounds;
 *   * `malformed` — the domain matched but the envelope failed a bound or its
 *     shape. The core refuses a bad profile AS A WHOLE, and so does this: it is
 *     DROPPED — never stored, and never handed to the chat parser, which would
 *     render it;
 *   * `none` — not a profile envelope; the caller carries on to the chat parse.
 *
 * The verdict comes from the core's `decodeProfileEnvelope` and its stable
 * `profileError` code (`wrongDomain` is the only "not mine" answer). No domain
 * bytes are copied into this file.
 */

/** A decoded profile, detached from the wasm handle. */
export interface ReceivedProfile {
  readonly displayName: string | null;
  readonly username: string | null;
  readonly updatedAtMs: bigint;
  /** The sender's 16-byte auth account id (ADR-0048 v2), or `null` when the
   *  envelope carried none. The sender's CLAIM, like the name: it is what the
   *  call roster derives the ADR-0021 peer tag from, and nothing else. Copied
   *  out of the wasm handle, so it outlives `free()`. */
  readonly accountId: Uint8Array | null;
  /** ADR-0060 profile v3 — the discovery-signed username certificate the
   *  sender carries, or `null`. Verified by the viewer before the username
   *  is shown (`verifyUsernameBinding`). */
  readonly usernameCertificate?: Uint8Array | null;
  /** ADR-0060 profile v3 — the reach grants the envelope carried, as
   *  37-byte grant wires in base64 with their class byte and version.
   *  SECRET (each lets its holder write to the sender's mailbox): kept only
   *  under the rules of `reach/grant-rules.ts`, never logged. */
  readonly grants?: readonly ReceivedGrantWire[];
}

/** One grant from a profile, detached from the wasm handle. */
export interface ReceivedGrantWire {
  /** The class wire byte (1 contact .. 7 fof). */
  readonly classByte: number;
  readonly version: number;
  /** Standard base64 of the 37-byte grant wire. */
  readonly wireB64: string;
}

export type ProfileSniff =
  | { readonly kind: "none" }
  | { readonly kind: "malformed" }
  | { readonly kind: "profile"; readonly profile: ReceivedProfile };

/** The one wasm export this needs, structurally. */
export interface ProfileDecoder {
  decodeProfileEnvelope(bytes: Uint8Array): {
    readonly displayName: string | undefined;
    readonly username: string | undefined;
    readonly updatedAtMs: bigint;
    readonly accountId?: Uint8Array | undefined;
    readonly usernameCertificate?: Uint8Array | undefined;
    readonly grants?: readonly { readonly class: number; readonly version: number; free?(): void }[];
    free?(): void;
  };
  /** Encodes a grant to its 37-byte wire (`encodeReachGrant`); absent in a
   *  decoder that never sees grants. */
  encodeReachGrant?(grant: never): Uint8Array;
}

/** First byte every `lakko/…` service-envelope domain starts with. A cheap
 *  skip for chat envelopes (JSON, `{`) and call signaling; never a verdict. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** Classify an application plaintext. Never throws. */
export function sniffProfile(decoder: ProfileDecoder, bytes: Uint8Array): ProfileSniff {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return { kind: "none" };
  let decoded: ReturnType<ProfileDecoder["decodeProfileEnvelope"]>;
  try {
    decoded = decoder.decodeProfileEnvelope(bytes);
  } catch (err) {
    const code =
      typeof err === "object" && err !== null
        ? (err as { profileError?: unknown }).profileError
        : undefined;
    // `wrongDomain` is "not a profile". Any other code means the domain
    // matched and the body was refused — drop it. A throw with no code at all
    // is not evidence of a profile, and treating it as one could swallow a
    // real message; let the chat parser have it.
    return code === undefined || code === "wrongDomain" ? { kind: "none" } : { kind: "malformed" };
  }
  const grants: ReceivedGrantWire[] = [];
  for (const g of decoded.grants ?? []) {
    try {
      if (decoder.encodeReachGrant !== undefined) {
        grants.push({
          classByte: g.class,
          version: g.version,
          wireB64: bytesToBase64(decoder.encodeReachGrant(g as never)),
        });
      }
    } finally {
      try {
        g.free?.();
      } catch {
        /* best-effort */
      }
    }
  }
  const profile: ReceivedProfile = {
    displayName: decoded.displayName ?? null,
    username: decoded.username ?? null,
    updatedAtMs: decoded.updatedAtMs,
    accountId: decoded.accountId === undefined ? null : decoded.accountId.slice(),
    usernameCertificate:
      decoded.usernameCertificate === undefined ? null : decoded.usernameCertificate.slice(),
    grants,
  };
  try {
    decoded.free?.();
  } catch {
    // Freeing is best-effort.
  }
  return { kind: "profile", profile };
}

function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i] ?? 0);
  return btoa(bin);
}
