// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How included evidence is marked and counted (UX section 5.3, ADR-0089
 * section 1). A message is `Written by this account` only when its signature
 * checks out against the report's target; any other message is `Not
 * verified` and shown greyed. Contact requests, channel posts and listings
 * are carried but not checked yet, and say so.
 */

import type { Checks, DiscoveryRead } from "../api/types";

export type EvidenceMark = "by-target" | "not-verified" | "not-checked";

/** The mark of one evidence item, from its kind and `evidenceIsByTarget`. */
export function evidenceMark(kind: string | undefined, isByTarget: boolean): EvidenceMark {
  if (kind !== "message") return "not-checked";
  return isByTarget ? "by-target" : "not-verified";
}

export function markLabel(mark: EvidenceMark): string {
  switch (mark) {
    case "by-target":
      return "Written by this account";
    case "not-verified":
      return "Not verified";
    case "not-checked":
      return "Not checked";
  }
}

/** Greyed: counts for nothing in the order. */
export function isGreyed(mark: EvidenceMark): boolean {
  return mark !== "by-target";
}

/**
 * What the browser verified in one report, for `POST .../checks`: messages
 * written by the target, and messages that did not verify. Other kinds are
 * not messages and count in neither. Each count fits the API's 0..255.
 */
export function countChecks(items: readonly { readonly kind: string | undefined; readonly isByTarget: boolean }[]): Checks {
  let checked = 0;
  let unchecked = 0;
  for (const item of items) {
    const mark = evidenceMark(item.kind, item.isByTarget);
    if (mark === "by-target") checked += 1;
    else if (mark === "not-verified") unchecked += 1;
  }
  return { checked: Math.min(checked, 255), unchecked: Math.min(unchecked, 255) };
}

/** `true` when the service's stored checks are absent or differ. */
export function checksNeedPosting(stored: Checks | null, computed: Checks): boolean {
  return stored === null || stored.checked !== computed.checked || stored.unchecked !== computed.unchecked;
}

/** How an attachment is shown before anyone asks to see it. */
export type AttachmentGate = "blurred" | "on-request";

/**
 * Photos and videos are blurred until clicked; anything else (audio, files)
 * opens only on an explicit click. `inline` is `inlineMediaType(mime)`'s
 * category, or `null` when the MIME type is not one shown inline.
 */
export function attachmentGate(inline: "image" | "video" | "audio" | null): AttachmentGate {
  return inline === "image" || inline === "video" ? "blurred" : "on-request";
}

/** The attachment's one-line description. */
export function attachmentLabel(args: {
  readonly kind: string | undefined;
  readonly inline: "image" | "video" | "audio" | null;
  readonly name: string | undefined;
  readonly size: bigint;
}): string {
  const what =
    args.kind === "voice"
      ? "Voice message"
      : args.kind === "video_note"
        ? "Video message"
        : args.kind === "gif"
          ? "GIF"
          : args.inline === "image"
            ? "Photo"
            : args.inline === "video"
              ? "Video"
              : args.inline === "audio"
                ? "Audio"
                : "File";
  const name = args.name !== undefined && args.name !== "" ? `: ${args.name}` : "";
  return `${what}${name} (${formatSize(args.size)})`;
}

export function formatSize(size: bigint): string {
  const n = Number(size);
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * The account's username line in a case header. A username is shown only
 * when the case's evidence names one; otherwise the discovery read says
 * whether it has one at all.
 */
export function usernameLine(evidenceUsername: string | null, discovery: DiscoveryRead | null): string {
  if (evidenceUsername !== null && evidenceUsername !== "") return `@${evidenceUsername}`;
  if (discovery === null) return "Username unknown: the directory didn't answer";
  return discovery.has_username ? "Has a username" : "No username";
}

/**
 * The type the decrypted bytes are given in the browser: the attachment's
 * own MIME type only when it is one the app shows inline (the
 * `inlineMediaType` allowlist), else an opaque download. A sender-chosen
 * type is never trusted further than that.
 */
export function blobContentType(inline: "image" | "video" | "audio" | null, mime: string | undefined): string {
  if (inline === null || mime === undefined) return "application/octet-stream";
  return mime.split(";", 1)[0]?.trim().toLowerCase() ?? "application/octet-stream";
}

/** The file name offered when saving: the sender's name or a plain default. */
export function saveName(name: string | undefined): string {
  const trimmed = (name ?? "").trim();
  return trimmed === "" ? "evidence-file" : trimmed;
}

/** Why a fetched file was not shown, as a sentence. */
export function fileProblem(kind: "mismatch" | "decrypt"): string {
  return kind === "mismatch"
    ? "The server returned a different file than the report names, so it wasn't opened."
    : "The file couldn't be decrypted with the key in the report, so it wasn't opened.";
}
