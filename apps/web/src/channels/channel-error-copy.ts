// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words for a refused channel operation (ADR-0087 phase B). The channel
 * machinery throws `ChannelError` with a code and holds no copy; the screens
 * map the code here, so one refusal reads the same on every screen.
 */
import { t } from "../l10n";
import { anonTokenMessage } from "../tokens/token-copy";

/** The codes `channels/channel-access.ts` throws. */
export type ChannelErrorCode =
  | "sign-in"
  | "handle-held"
  | "handle-malformed"
  | "too-many-today"
  | "offline"
  | "setup-failed"
  | "photo-upload"
  | "photo-limit"
  | "link-invalid"
  | "link-damaged"
  | "not-publisher"
  | "needs-owner"
  | "failed";

const CODES: ReadonlySet<string> = new Set<ChannelErrorCode>([
  "sign-in",
  "handle-held",
  "handle-malformed",
  "too-many-today",
  "offline",
  "setup-failed",
  "photo-upload",
  "photo-limit",
  "link-invalid",
  "link-damaged",
  "not-publisher",
  "needs-owner",
  "failed",
]);

/** The code of a refusal, or `null` for an error that is not a channel refusal. */
export function channelErrorCode(err: unknown): ChannelErrorCode | null {
  if (typeof err !== "object" || err === null) return null;
  const code = (err as { code?: unknown }).code;
  return typeof code === "string" && CODES.has(code) ? (code as ChannelErrorCode) : null;
}

/** Where a refusal is shown on a form: under the `@name`, under the photo, or
 *  above the buttons. */
export type ChannelErrorPlace = "handle" | "photo" | "form";

export function channelErrorPlace(code: ChannelErrorCode | null): ChannelErrorPlace {
  if (code === "handle-held" || code === "handle-malformed") return "handle";
  if (code === "photo-upload" || code === "photo-limit") return "photo";
  return "form";
}

/** One sentence (or two) for a refusal, in the person's language. `failed` is
 *  the screen's own fallback ("Couldn't save. Try again." on a settings
 *  screen, "Couldn't create this channel..." on the create form). */
export function channelErrorText(err: unknown, failed: () => string): string {
  const code = channelErrorCode(err);
  switch (code) {
    case "sign-in":
      return t("channels.error.sign_in");
    case "handle-held":
      return t("group_handle.error.held");
    case "handle-malformed":
      return t("group_handle.error.bad_form");
    case "too-many-today":
      return t("channels.new.too_many");
    case "offline":
      return t("group_handle.error.offline");
    case "photo-upload":
      return t("group_photo.error.upload");
    case "photo-limit":
      return t("group_photo.error.limit");
    case "link-invalid":
      return t("invite.link.invalid");
    case "link-damaged":
      return t("invite.link.damaged");
    case "not-publisher":
      return t("channels.post.not_publisher");
    case "needs-owner":
      return t("channels.links.needs_owner");
    case "setup-failed":
    case "failed":
      return failed();
    case null:
      break;
  }
  const token = anonTokenMessage(err, t("group_handle.error.offline"));
  return token ?? failed();
}
