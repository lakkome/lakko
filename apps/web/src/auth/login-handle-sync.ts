// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Bind the login handle of a session's username, whatever path installed
 * the session (ADR-0054 S5).
 *
 * Sign-in by name reaches an account only through the handle bound at auth
 * (`PUT /auth/login-handle`). The web bound it at signup (claim order) and
 * after a sign-in by name; a session established before S5, or through a
 * path that never typed a name (reload restore, passkey, device pairing,
 * another-device confirmation), never bound one, so after a
 * sign-out that person could not sign in by name any more.
 *
 * {@link LoginHandleSync.ensure} closes that: whenever a session is live and
 * the username is known on this device, bind its handle with that session,
 * best-effort:
 *
 * - at most once per `(session token, handle)` in this page's memory (a new
 *   page load may repeat it; the PUT is idempotent);
 * - ADR-0068: the handle is CHECKED first and bound only when the account
 *   holds none ({@link handleSyncAction}); a device holding an old name
 *   binds nothing;
 * - no username (a guest) binds nothing;
 * - `503 login-by-name-disabled` is silent;
 * - `409 handle-taken` and every other failure is logged by status (or error
 *   name) only, never the handle or the token, and changes nothing for the
 *   user: no sign-out, no UI.
 */
import { AuthHttpError } from "./client";
import { tokenAccountSegment } from "./session-pair";
import { LoginHandleUsernameError, type LoginHandle } from "./login-handle";

/** What auth says about the handle of the device's local username
 *  (`POST /auth/login-handle/check`). */
export type HandleState = "same" | "other" | "none";

/**
 * ADR-0068: what a session does about its handle. The device never binds
 * blindly: a device that still holds an OLD name would take the account's
 * handle back and undo a rename made elsewhere.
 *
 *   - `same`: the account holds this handle. Nothing.
 *   - `none`: the account holds no handle. Bind this one (a first bind,
 *     which needs no grant).
 *   - `other`: the account holds another handle, so it was renamed on
 *     another device. Bind NOTHING and wait for the own-username register
 *     to bring the new name.
 */
export function handleSyncAction(state: HandleState): "nothing" | "bind" | "wait" {
  if (state === "same") return "nothing";
  return state === "none" ? "bind" : "wait";
}

/** What one {@link LoginHandleSync.ensure} call did. For tests and logs. */
export type LoginHandleSyncOutcome =
  /** The account already holds this handle. */
  | "same"
  /** The account holds another handle (renamed elsewhere): nothing was
   *  bound; the register brings the new name. */
  | "renamed-elsewhere"
  /** The PUT succeeded. */
  | "bound"
  /** No username (a guest), or one the username rule refuses. */
  | "no-username"
  /** Already bound (or attempted) with this session in this page. */
  | "already"
  /** `503`: sign-in by name is off on this server. Silent. */
  | "disabled"
  /** Anything else, `409 handle-taken` included. Logged, nothing else. */
  | "failed";

/** Injectable collaborators (the app passes the wasm core + `AuthClient`). */
export interface LoginHandleSyncDeps {
  /** The handle of `username`; throws `LoginHandleUsernameError` for a name
   *  the rule refuses. */
  readonly handleFor: (username: string) => Promise<LoginHandle>;
  /** `POST /auth/login-handle/check` with `sessionToken`. */
  readonly check: (sessionToken: string, handleWire: string) => Promise<HandleState>;
  /** `PUT /auth/login-handle` with `sessionToken` (a first bind). */
  readonly bind: (sessionToken: string, handleWire: string) => Promise<void>;
  /** Log sink; defaults to `console.warn`. Receives no handle or token. */
  readonly warn?: (message: string, detail: string | number) => void;
}

export interface LoginHandleSync {
  /** Bind the handle of `username` with `sessionToken`, best-effort. Never
   *  throws. */
  ensure(sessionToken: string, username: string): Promise<LoginHandleSyncOutcome>;
  /** Record that a path already bound (or is binding) `handleWire` with
   *  `sessionToken`, so {@link ensure} does not repeat it. */
  markBound(sessionToken: string, handleWire: string): void;
}

export function createLoginHandleSync(deps: LoginHandleSyncDeps): LoginHandleSync {
  const done = new Set<string>();
  // Keyed on the token's ACCOUNT segment, not the whole token (ADR-0065):
  // an access token is replaced by a refresh every fifteen minutes, and a
  // key on the token would re-bind on every refresh, ticking the server's
  // bind limit for nothing. One bind per account per page.
  const key = (token: string, wire: string): string => `${tokenAccountSegment(token)}\n${wire}`;
  const warn =
    deps.warn ??
    ((message: string, detail: string | number): void => {
      // eslint-disable-next-line no-console
      console.warn(message, detail);
    });

  return {
    markBound(sessionToken, handleWire) {
      done.add(key(sessionToken, handleWire));
    },

    async ensure(sessionToken, username) {
      if (sessionToken.length === 0 || username.trim().length === 0) return "no-username";
      let handle: LoginHandle;
      try {
        handle = await deps.handleFor(username);
      } catch (err) {
        if (err instanceof LoginHandleUsernameError) return "no-username";
        warn("login handle bind skipped", err instanceof Error ? err.name : "unknown");
        return "failed";
      }
      // Check and record with no await in between, so two installs racing
      // for the same session send one PUT.
      const k = key(sessionToken, handle.wire);
      if (done.has(k)) return "already";
      done.add(k);
      try {
        const action = handleSyncAction(await deps.check(sessionToken, handle.wire));
        if (action === "nothing") return "same";
        if (action === "wait") {
          // Asked again at the next landing: by then the register has
          // usually brought the new name, whose handle is `same`.
          done.delete(k);
          return "renamed-elsewhere";
        }
        await deps.bind(sessionToken, handle.wire);
        return "bound";
      } catch (err) {
        if (err instanceof AuthHttpError && err.status === 503) return "disabled";
        warn(
          "login handle bind for the session failed",
          err instanceof AuthHttpError ? err.status : err instanceof Error ? err.name : "unknown",
        );
        return "failed";
      }
    },
  };
}
