// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a failed call says, from the kind of failure and never from its prose
 * (ADR-0061 section 6: errors render from their code).
 *
 * A call can fail in the call-admission mint (`call-jwt-client.ts`), in media
 * capture, in the browser's lack of media encryption, or somewhere deeper in
 * the SFU or MLS path. The first three have a sentence of their own; anything
 * else is the flow's own fallback. The error's `.message` goes to the log
 * only: it is written for a developer, and before this module it reached the
 * screen verbatim ("... failed with HTTP 429: ...").
 */
import { keyedErrorText } from "../l10n/keyed-error";
import { t } from "../l10n";
import { CallJwtHttpError, CallJwtNetworkError } from "./call-jwt-client";
import { anonTokenMessage } from "../tokens/token-copy";
import { captureFailureReason } from "./capture-fallback";
import type { CallText } from "./call-slot";
import { isSFrameTransformSupported } from "./sframe-transform";

/**
 * The sentence for a group or call-link call (both SFrame-encrypted through
 * the SFU) that failed with `err`, or `null` when the failure has no sentence
 * of its own. Not for the 1x1 path, which does not use SFrame.
 */
export function callFailureReason(err: unknown): CallText | null {
  if (keyedErrorText(err) !== null) return () => keyedErrorText(err) ?? "";
  // Invariant #4: a browser that cannot encrypt call media never gets a call.
  // Asked directly rather than read out of the refusal's prose.
  if (!isSFrameTransformSupported()) return () => t("call.error.no_media_encryption");
  // The admission token is paid for by an anonymous `call-join` token
  // (ADR-0078 section 2): an allowance used up, or no token to be had,
  // speaks the token copy, never a lost session.
  const tokenCopy = anonTokenMessage(err, t("common.error.offline"));
  if (tokenCopy !== null) return () => tokenCopy;
  if (err instanceof CallJwtHttpError) {
    if (err.status === 429) return () => t("call.error.not_accepting");
    if (err.status === 401 || err.status === 503) return () => t("call.error.unavailable");
    return null;
  }
  if (err instanceof CallJwtNetworkError) return () => t("common.error.offline");
  switch (captureFailureReason(err)) {
    case "not-allowed":
      return () => t("call.error.microphone_not_allowed");
    case "not-found":
      return () => t("call.error.microphone_not_found");
    default:
      return null;
  }
}

/** {@link callFailureReason}, or the flow's own `fallback`. */
export function callFailureText(err: unknown, fallback: CallText): CallText {
  return callFailureReason(err) ?? fallback;
}
