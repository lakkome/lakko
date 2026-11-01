// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a decrypted channel post is, apart from drawing it: `PostCard.tsx`
 * renders these outcomes, and the feed and the channel list read them with no
 * DOM and no router.
 */
import type { ChannelSharedMeta } from "./channel-log-codec";
import { utf8Decode, type DecodedPostResult } from "./composer";
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
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "unreadable" }
  | { readonly kind: "older-key" }
  | { readonly kind: "meta"; readonly meta: ChannelSharedMeta }
  | { readonly kind: "dropped" };

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
export function toPostDecode(res: DecodedPostResult, classify: PostPlaintextClassifier): PostDecode {
  if (res.plaintext === null) {
    return res.hadSeed ? { kind: "unreadable" } : { kind: "older-key" };
  }
  const c = classify(res.plaintext);
  try {
    if (c.kind === "meta" && c.name !== undefined && c.updatedAtMs !== undefined) {
      return {
        kind: "meta",
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
    return { kind: "text", text: utf8Decode(c.text ?? res.plaintext) };
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
  return decoded !== undefined && (decoded.kind === "meta" || decoded.kind === "dropped");
}

/** The one-line reason under "Not available", in plain words. */
export function unavailableReason(decoded: PostDecode): string {
  return decoded.kind === "older-key"
    ? t("channels.post.older_key")
    : t("channels.post.unreadable");
}
