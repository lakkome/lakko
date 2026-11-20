// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/saved-messages-and-file-lifetime.md sections 4.1 and 4.2 — what
 * a failed download or upload of a file means to the person:
 *
 *   * `gone`: the server no longer holds the file (404). Two lines, no
 *     Retry: trying again cannot help.
 *   * `limit`: opening (or sending) files is limited for now. One line with
 *     Try again (opening), or a composer note (sending).
 *   * `failed`: anything else, including a limit service that cannot be
 *     reached and an nginx refusal with no code: the existing generic
 *     failure with Try again.
 *
 * One classification for every media kind, so the six bubbles cannot tell
 * the person six different things.
 */

import { t } from "../l10n";
import { isStorageFull } from "../storage/blob-access";

export { isStorageFull } from "../storage/blob-access";

export type AttachmentFailure = "gone" | "limit" | "failed";

/** Walk an error and its causes by name (the errors cross module lines). */
function named(err: unknown, name: string): unknown {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && typeof current === "object" && current !== null; depth += 1) {
    const r = current as { name?: unknown; cause?: unknown };
    if (r.name === name) return current;
    current = r.cause;
  }
  return null;
}

/** Classify a failure to open (download) a file. */
export function openFailureOf(err: unknown): AttachmentFailure {
  if (named(err, "AttachmentMissingError") !== null) return "gone";
  const limit = named(err, "StorageLimitError") as { kind?: unknown } | null;
  if (limit !== null && limit.kind === "read") return "limit";
  return "failed";
}

/** Whether a failure to send (upload) a file is the sending limit. */
export function isSendLimit(err: unknown): boolean {
  const limit = named(err, "StorageLimitError") as { kind?: unknown } | null;
  return limit !== null && limit.kind === "upload";
}

/** Section 4.1 — the two lines of a file that is no longer available. */
export function goneLines(): { readonly title: string; readonly body: string } {
  return { title: t("chat.attachment.gone.title"), body: t("chat.attachment.gone.body") };
}

/** Section 4.2 — opening is limited. */
export function openLimitText(): string {
  return t("chat.attachment.limit.open");
}

/** Section 4.2 — sending is limited (the composer note). */
export function sendLimitText(): string {
  return t("chat.attachment.limit.send");
}

/** docs/ux/storage-full.md — the server has no room for new files (the
 *  composer note; the text and the chosen file stay). */
export function sendFullText(): string {
  return t("chat.attachment.full.send");
}

/**
 * The composer note for a failed send, when it is one of the two refusals
 * that have their own sentence: the sending limit or a full server. `null`
 * for anything else (the caller keeps its generic error).
 */
export function sendRefusalText(err: unknown): string | null {
  if (isStorageFull(err)) return sendFullText();
  if (isSendLimit(err)) return sendLimitText();
  return null;
}

/**
 * The line under a group photo whose upload failed: a full server, the
 * sending limit (also read from an error that only says so in its message,
 * as the group screens always did), or the generic upload failure.
 */
export function groupPhotoUploadErrorText(err: unknown): string {
  if (isStorageFull(err)) return t("group_photo.error.full");
  const text = err instanceof Error ? err.message : "";
  if (isSendLimit(err) || /limit|429/i.test(text)) return t("group_photo.error.limit");
  return t("group_photo.error.upload");
}
