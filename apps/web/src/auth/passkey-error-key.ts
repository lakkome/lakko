// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The line Settings > Security > Passkeys shows for a failed passkey
 * action (add, remove). Its own module so it can be tested without the
 * component.
 */
import {
  WebauthnCredentialExistsError,
  WebauthnHttpError,
  WebauthnNetworkError,
  WebauthnUnsupportedError,
  WebauthnUserCancelledError,
} from "./webauthn-client";

/** The catalog key for an error (ADR-0061 section 6: never the server's
 *  or the browser's prose, which goes to the log). */
export function passkeyErrorKey(err: unknown): string {
  if (err instanceof WebauthnUserCancelledError) {
    return "settings.passkeys.error.cancelled";
  }
  if (err instanceof WebauthnCredentialExistsError) {
    return "settings.passkeys.error.already_exists";
  }
  if (err instanceof WebauthnUnsupportedError) {
    return "settings.passkeys.error.unsupported";
  }
  if (err instanceof WebauthnHttpError) {
    if (err.status === 503) {
      return "auth.passkey.error.server_disabled";
    }
    if (err.status === 401) {
      return "common.error.session_expired";
    }
    console.warn("[passkeys] server error", err.status, err.body);
    return "settings.passkeys.error.server_status";
  }
  if (err instanceof WebauthnNetworkError) {
    return "settings.passkeys.error.network";
  }
  console.warn("[passkeys] failed", err);
  return "settings.passkeys.error.generic";
}
