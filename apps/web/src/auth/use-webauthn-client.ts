// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A `WebauthnClient` bound to the live session token. Rebuilt whenever the
 * token rotates, so a section can re-bind across logins without remounting.
 * Shared by the passkeys, data-export and delete-account sections.
 */
import { useAuth } from "./AuthContext";
import { WebauthnClient } from "./webauthn-client";

export function useWebauthnClient(): {
  readonly client: () => WebauthnClient | null;
  readonly username: () => string | null;
} {
  const auth = useAuth();
  const username = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : null;
  };
  const client = (): WebauthnClient | null => {
    const secrets = auth.identitySecrets();
    if (secrets === null) return null;
    const c = new WebauthnClient();
    c.setSessionToken(secrets.sessionToken);
    return c;
  };
  return { client, username };
}
