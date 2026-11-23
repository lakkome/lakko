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
 * allowlisted MIME type must be of the matching media class.
 *
 * UX spec section 10 adds the one case with NO kind: a plain attachment whose
 * allowlisted class is an image is a photo bubble, and one whose class is
 * video (`video/mp4`) is a video bubble. Anything else — an unknown kind, a
 * mismatched class, an absent or refused MIME (HEIC, SVG) — is a file chip.
 */

export type InlineType = "image" | "audio" | "video";
export type MediaBubble = "voice" | "video_note" | "gif" | "photo" | "video" | "file";

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
  if (ref.kind === undefined) {
    if (inline === "image") return { bubble: "photo", inline };
    if (inline === "video") return { bubble: "video", inline };
  }
  return { bubble: "file", inline };
}

/**
 * The bubble of a message that is NOTHING but one piece of media — no text, no
 * second attachment, not a tombstone, not open in the edit composer — or
 * `null` when the message is anything else.
 *
 * "Nothing but" is the load-bearing half. A voice message sent WITH a caption,
 * or beside another attachment, is an ordinary bubble that happens to contain
 * a player: it has a paragraph to sit under, so it keeps the bubble's own meta
 * row. Only the lone one gets a slot of its own.
 */
export function soleMediaBubble(
  args: {
    readonly deleted: boolean;
    readonly text: string;
    readonly attachments: ReadonlyArray<{
      readonly kind?: string | undefined;
      readonly mime?: string | undefined;
    }>;
  },
  allowlist: WasmInlineMediaType,
): MediaBubble | null {
  const only = args.attachments[0];
  if (args.deleted || args.text.length > 0 || args.attachments.length !== 1) return null;
  if (only === undefined) return null;
  return renderDecision(only, allowlist).bubble;
}

/**
 * `docs/ux/media-messages.md` section 11 — a message that is nothing but
 * photos and videos (one or several, no text, nothing else attached): its
 * mosaic fills the bubble and the time goes on the mosaic in a pill, as for
 * one photo.
 */
export function onlyVisualMedia(
  args: {
    readonly deleted: boolean;
    readonly text: string;
    readonly attachments: ReadonlyArray<{
      readonly kind?: string | undefined;
      readonly mime?: string | undefined;
    }>;
  },
  allowlist: WasmInlineMediaType,
): boolean {
  if (args.deleted || args.text.length > 0 || args.attachments.length === 0) return false;
  return args.attachments.every((a) => {
    const b = renderDecision(a, allowlist).bubble;
    return b === "photo" || b === "video";
  });
}

/**
 * Where a message's meta line — the time, the edited marker, the delivery
 * ticks — is drawn.
 *
 * Four answers, because four bubble shapes have four different places to put
 * it, and the rule is stated once here rather than as the conditionals at the
 * call site that used to encode it and could disagree (the voice case fell
 * through all of them and got a second line below the row).
 *
 *  * `bubble-row` — a row under the content, inside the bubble's surface. The
 *    default, and where a paragraph wants it.
 *  * `voice-line` — the voice row's own quiet line, at its trailing end (UX
 *    spec §2). A voice message is a ROW, not a paragraph: putting the length
 *    of the recording and the time it was sent on two different levels makes
 *    the bubble taller than it has any reason to be and reads as two unrelated
 *    facts.
 *  * `media-pill` — a translucent pill over the media (UX spec §10), because a
 *    lone photo or video is drawn edge to edge with no chrome to carry a row.
 *  * `note-pill` — a translucent pill inside the chromeless video-note circle,
 *    bottom trailing (UX spec §3, amended 2026-11-22): the circle has no
 *    surface to carry a row, and a line under it read as loose text floating
 *    under the video.
 */
export type BubbleMetaSlot = "bubble-row" | "voice-line" | "media-pill" | "note-pill";

/** The meta slot for a message whose sole media bubble is `sole`
 *  (`soleMediaBubble`, `null` for every other message). */
export function bubbleMetaSlot(sole: MediaBubble | null): BubbleMetaSlot {
  switch (sole) {
    case "voice":
      return "voice-line";
    case "photo":
    case "video":
      return "media-pill";
    case "video_note":
      return "note-pill";
    // A GIF keeps the row: it is a bubble with chrome, and a pill over an
    // animation that may be 40px tall would cover it.
    default:
      return "bubble-row";
  }
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
