// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §1 — how a received attachment is rendered, decided in one place.
 *
 * Two inputs, two different trust levels, and the rule that keeps them apart:
 *
 *   * `kind` is **sender-chosen** — exactly as trustworthy as the file name.
 *     It picks the bubble SHAPE (a voice player, a round video, an animation)
 *     and nothing else.
 *   * Whether the bytes are handed to an `<img>` / `<audio>` / `<video>` at
 *     all is decided by the MIME type alone, through the core's allowlist
 *     (`inlineMediaType` in `lakko-attachments`, reached over wasm). A
 *     `kind: "voice"` carrying `mime: "text/html"` is a file chip.
 *
 * So a media bubble needs BOTH: the kind asks for the shape, and the
 * allowlisted MIME type must be of the matching media class. Anything else —
 * no kind, an unknown kind, a mismatched class, an absent or refused MIME —
 * is a file chip, which may still offer an inline preview if (and only if) its
 * MIME type is allowlisted.
 */

export type InlineType = "image" | "audio" | "video";
export type MediaBubble = "voice" | "video_note" | "gif" | "file";

/**
 * The wasm `InlineMediaType` enum values (`Image = 0`, `Audio = 1`,
 * `Video = 2`). Pinned here, and asserted against the real enum in the test,
 * so this module does not import the wasm package at runtime.
 */
export const WASM_INLINE_IMAGE = 0;
export const WASM_INLINE_AUDIO = 1;
export const WASM_INLINE_VIDEO = 2;

/** The core's allowlist, as the wasm binding exposes it. */
export type WasmInlineMediaType = (mime: string) => number | undefined;

/** The allowlisted media class of `mime`, or `null`. Fails closed: an absent
 *  MIME, an unknown enum value or a throwing binding all mean "not inline". */
export function inlineTypeOf(
  mime: string | undefined,
  allowlist: WasmInlineMediaType,
): InlineType | null {
  if (mime === undefined || mime.length === 0) return null;
  let t: number | undefined;
  try {
    t = allowlist(mime);
  } catch {
    return null;
  }
  switch (t) {
    case WASM_INLINE_IMAGE:
      return "image";
    case WASM_INLINE_AUDIO:
      return "audio";
    case WASM_INLINE_VIDEO:
      return "video";
    default:
      return null;
  }
}

export interface RenderDecision {
  readonly bubble: MediaBubble;
  /** The allowlisted media class, independent of the bubble — a file chip
   *  may still preview inline when this is non-null. */
  readonly inline: InlineType | null;
}

export function renderDecision(
  ref: { readonly kind?: string | undefined; readonly mime?: string | undefined },
  allowlist: WasmInlineMediaType,
): RenderDecision {
  const inline = inlineTypeOf(ref.mime, allowlist);
  if (ref.kind === "voice" && inline === "audio") return { bubble: "voice", inline };
  if (ref.kind === "video_note" && inline === "video") return { bubble: "video_note", inline };
  if (ref.kind === "gif" && (inline === "image" || inline === "video")) {
    return { bubble: "gif", inline };
  }
  return { bubble: "file", inline };
}

/**
 * The type an object URL's `Blob` is stamped with. Only an allowlisted type
 * is carried; anything else becomes `application/octet-stream`, so a browser
 * never sniffs a refused type into something it renders.
 */
export function blobTypeFor(
  mime: string | undefined,
  allowlist: WasmInlineMediaType,
): string {
  return inlineTypeOf(mime, allowlist) === null || mime === undefined
    ? "application/octet-stream"
    : mime;
}
