// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recognise the ADR-0087 group service envelopes before the chat parser
 * (whose lossy-UTF-8 fallback would otherwise have nothing to say about them
 * but the core's "service envelope"): the group photo (section 18.1), the
 * allowed reactions (`reactions-and-emoji.md` section 9), the conversation
 * handle (section 11) and the knock seed an owner hands an admin (section 5).
 *
 * Each decoder throws `"<area>.<code>: reason"`; `wrongDomain` means "not
 * this kind", any other code means the domain matched and the body was
 * refused (dropped, never parsed as chat).
 */

import type { Wasm } from "../wasm";
import { avatarFromWasm } from "../profile/avatar-model";
import type { StoredGroupPhoto, StoredGroupReactions } from "./group-settings-store";
import type { AllowedSetting } from "./reaction-rules";

/** A conversation's `@name` as the handle envelope carries it. `envelope` is
 *  the whole envelope, re-sent verbatim on growth. */
export interface StoredConversationHandle {
  readonly handle: string;
  readonly isPublic: boolean;
  readonly updatedAtMs: bigint;
  readonly certificate: Uint8Array;
  readonly envelope: Uint8Array;
}

export type GroupSetting =
  | { readonly kind: "photo"; readonly value: StoredGroupPhoto }
  | { readonly kind: "reactions"; readonly value: StoredGroupReactions }
  | { readonly kind: "handle"; readonly value: StoredConversationHandle }
  | { readonly kind: "knockSeed"; readonly plaintext: Uint8Array };

export type GroupSettingSniff =
  | { readonly kind: "none" }
  | { readonly kind: "malformed" }
  | { readonly kind: "setting"; readonly setting: GroupSetting };

const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

function isWrongDomain(err: unknown): boolean {
  const text = typeof err === "string" ? err : err instanceof Error ? err.message : "";
  return /^[A-Za-z]+\.wrongDomain\b/.test(text) || text.length === 0;
}

function freeQuietly(h: { free?(): void } | undefined): void {
  try {
    h?.free?.();
  } catch {
    /* already freed */
  }
}

/** The allowed reactions of a wasm `AllowedReactions`. */
export function allowedFromWasm(a: { mode: string; emoji: string[]; free?(): void }): AllowedSetting {
  try {
    if (a.mode === "off") return { mode: "off" };
    if (a.mode === "only") return { mode: "only", emoji: [...a.emoji] };
    return { mode: "all" };
  } finally {
    freeQuietly(a);
  }
}

export function sniffGroupSetting(wasm: Wasm, bytes: Uint8Array): GroupSettingSniff {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return { kind: "none" };
  // Group photo.
  try {
    const p = wasm.decodeGroupPhotoEnvelope(bytes);
    const value: StoredGroupPhoto = { avatar: avatarFromWasm(p.avatar), updatedAtMs: p.updatedAtMs };
    freeQuietly(p);
    return { kind: "setting", setting: { kind: "photo", value } };
  } catch (err) {
    if (!isWrongDomain(err)) return { kind: "malformed" };
  }
  // Allowed reactions.
  try {
    const r = wasm.decodeGroupReactionsEnvelope(bytes);
    const value: StoredGroupReactions = { allowed: allowedFromWasm(r.reactions), updatedAtMs: r.updatedAtMs };
    freeQuietly(r);
    return { kind: "setting", setting: { kind: "reactions", value } };
  } catch (err) {
    if (!isWrongDomain(err)) return { kind: "malformed" };
  }
  // Conversation handle.
  try {
    const h = wasm.decodeConversationHandleEnvelope(bytes);
    const value: StoredConversationHandle = {
      handle: h.handle,
      isPublic: h.public,
      updatedAtMs: h.updatedAtMs,
      certificate: h.certificate,
      envelope: bytes,
    };
    freeQuietly(h);
    return { kind: "setting", setting: { kind: "handle", value } };
  } catch (err) {
    if (!isWrongDomain(err)) return { kind: "malformed" };
  }
  // Knock seed: one fixed length.
  if (bytes.length === wasm.knockSeedEnvelopeLen()) {
    try {
      freeQuietly(wasm.decodeKnockSeedEnvelope(bytes));
      return { kind: "setting", setting: { kind: "knockSeed", plaintext: bytes } };
    } catch {
      // Not a knock seed; fall through.
    }
  }
  return { kind: "none" };
}
