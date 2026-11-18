// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a decrypted channel post is, apart from drawing it: `PostCard.tsx`
 * renders these outcomes, and the feed and the channel list read them with no
 * DOM and no router.
 */
import type { ChannelSharedMeta } from "./channel-log-codec";
import { utf8Decode, type DecodedPostResult } from "./composer";
import type { ParsedBodyResult } from "../chat/attachments";
import { t } from "../l10n";

/** The post-decode outcomes:
 *  - `text`       — decrypted plaintext (ADR-0031 decrypt-by-epoch).
 *  - `unreadable` — the device had the material for this post's epoch but
 *                   the open failed (tampered / foreign signer).
 *  - `older-key`  — no material exists for the post's epoch (older than the
 *                   retained window, or a gap). NOT force-decrypted with a
 *                   wrong-epoch seed (invariant #4).
 *  - `meta`       — an ADR-0051 metadata record (name + description). Folded
 *                   into the channel's shared meta, NEVER rendered as a post.
 *  - `dropped`    — a domain match the core refused as a whole (malformed).
 *                   Dropped, never rendered as text. */
export type PostDecode =
  | {
      readonly kind: "text";
      readonly text: string;
      /** ADR-0087 section 13 — the post's chat envelope (text, attachments),
       *  when the caller parsed it. Absent on a caller that passes no parser. */
      readonly body?: Extract<ParsedBodyResult, { kind: "chat" }>;
    }
  /** ADR-0087 section 13 — an edit or delete of an earlier post (its cursor
   *  is the target), never a card of its own. */
  | { readonly kind: "op"; readonly body: Extract<ParsedBodyResult, { kind: "edit" } | { kind: "delete" }> }
  /** ADR-0087 section 13a — the channel's ban list, folded, never shown. */
  | { readonly kind: "ban-list"; readonly plaintext: Uint8Array }
  | { readonly kind: "unreadable" }
  | { readonly kind: "older-key" }
  | {
      readonly kind: "meta";
      readonly meta: ChannelSharedMeta;
      /** The record's plaintext, for a fold that reads the v3 fields
       *  (handle, photo, allowed reactions, flag) from it. */
      readonly plaintext?: Uint8Array;
    }
  | { readonly kind: "dropped" }
  /** ADR-0090 — a poll, checklist, location, contact card or article post. */
  | { readonly kind: "content"; readonly body: Extract<ParsedBodyResult, { kind: "content" }> }
  /** ADR-0090 — an op on an earlier content post (a publisher's close with
   *  its frozen tally, an article edit); never a card. */
  | { readonly kind: "content-op"; readonly body: Extract<ParsedBodyResult, { kind: "content-op" }> };

/** The one wasm export the classification needs, structurally
 *  (`classifyChannelPostPlaintext`). */
export type PostPlaintextClassifier = (plaintext: Uint8Array) => {
  readonly kind: string;
  readonly text: Uint8Array | undefined;
  readonly name: string | undefined;
  readonly description: string | undefined;
  readonly updatedAtMs: bigint | undefined;
  /** ADR-0059 metadata v2 (absent on an older classifier double). */
  readonly topic?: number | undefined;
  readonly language?: string | undefined;
  readonly listed?: boolean | undefined;
  free?(): void;
};

/**
 * Map the composer's low-level decode result onto a render outcome. Every
 * decrypted plaintext goes through `classify` FIRST (ADR-0051 §4): a
 * metadata record must never reach the text path, and a record that fails
 * a bound is dropped rather than shown.
 */
export function toPostDecode(
  res: DecodedPostResult,
  classify: PostPlaintextClassifier,
  /** Parses a post's plaintext as the chat envelope (ADR-0087 section 13).
   *  Without it the plaintext is read as UTF-8 text, as before phase B. */
  parseBody?: (plaintext: Uint8Array) => ParsedBodyResult,
): PostDecode {
  if (res.plaintext === null) {
    return res.hadSeed ? { kind: "unreadable" } : { kind: "older-key" };
  }
  const c = classify(res.plaintext);
  try {
    if (c.kind === "meta" && c.name !== undefined && c.updatedAtMs !== undefined) {
      return {
        kind: "meta",
        plaintext: res.plaintext,
        meta: {
          name: c.name,
          description: c.description ?? "",
          updatedAtMs: c.updatedAtMs,
          ...(c.topic === undefined ? {} : { topic: c.topic }),
          ...(c.language === undefined ? {} : { language: c.language }),
          ...(c.listed === undefined ? {} : { listed: c.listed }),
        },
      };
    }
    if (c.kind === "malformed") return { kind: "dropped" };
    if (c.kind === "ban-list") return { kind: "ban-list", plaintext: res.plaintext };
    const bytes = c.text ?? res.plaintext;
    if (parseBody === undefined) return { kind: "text", text: utf8Decode(bytes) };
    const body = parseBody(bytes);
    if (body.kind === "chat") return { kind: "text", text: body.text, body };
    if (body.kind === "edit" || body.kind === "delete") return { kind: "op", body };
    if (body.kind === "content") return { kind: "content", body };
    if (body.kind === "content-op") return { kind: "content-op", body };
    // Reactions, typing, receipts and service envelopes have no place in a
    // channel: a post carries none of them.
    return { kind: "dropped" };
  } finally {
    try {
      c.free?.();
    } catch {
      // Freeing is best-effort.
    }
  }
}

/** `true` for an outcome the post list must not show at all. */
export function isHiddenPostDecode(decoded: PostDecode | undefined): boolean {
  return (
    decoded !== undefined &&
    (decoded.kind === "meta" ||
      decoded.kind === "dropped" ||
      decoded.kind === "op" ||
      decoded.kind === "content-op" ||
      decoded.kind === "ban-list")
  );
}

/** The one-line reason under "Not available", in plain words. */
export function unavailableReason(decoded: PostDecode): string {
  return decoded.kind === "older-key"
    ? t("channels.post.older_key")
    : t("channels.post.unreadable");
}
