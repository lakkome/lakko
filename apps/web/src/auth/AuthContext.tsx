// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Auth context — single source of truth for "is the user logged in?"
 *
 * Phase 1.16b stub. The real shape is going to be:
 *
 *   - `state`        — discriminated union: `{kind: "loading"}`,
 *                      `{kind: "anon"}`, `{kind: "session", account}`.
 *   - `signup(...)`  — drives the OPAQUE-PAKE registration flow
 *                      against `lakko-svc-auth`.
 *   - `login(...)`   — drives the OPAQUE-PAKE login flow.
 *   - `logout()`     — clears the session and any cached secrets.
 *
 * 1.16b only exposes the surface; the implementations are
 * placeholders that flip the state in-memory. 1.16c replaces the
 * placeholders with real OPAQUE calls through `lakko-ffi-wasm`.
 *
 * State persistence (localStorage / IndexedDB) is intentionally not
 * here yet — sessions reset on reload. That keeps the surface
 * trivial to reason about while we wire routing; persistence lands
 * with 1.16c (auth) and 1.16f (IndexedDB store).
 */
import {
  createContext,
  createSignal,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";

/** Account record exposed to the UI when a session is active. */
export interface SessionAccount {
  /** Stable opaque id; in 1.16c this becomes the OPAQUE account_id. */
  readonly accountId: string;
  /** Human-visible handle, e.g. `@alice`. */
  readonly username: string;
}

/** Discriminated union for the auth state. */
export type AuthState =
  | { readonly kind: "loading" }
  | { readonly kind: "anon" }
  | { readonly kind: "session"; readonly account: SessionAccount };

/** Public surface of the auth context. */
export interface AuthApi {
  /** Reactive accessor — re-renders subscribers on state change. */
  readonly state: Accessor<AuthState>;
  /**
   * Stub signup — flips state to a fake session. Phase 1.16c
   * replaces this with the OPAQUE-PAKE registration round-trip.
   */
  signup(username: string, password: string): Promise<void>;
  /**
   * Stub login — flips state to a fake session. Phase 1.16c
   * replaces this with the OPAQUE-PAKE login round-trip.
   */
  login(username: string, password: string): Promise<void>;
  /** Drop the current session. */
  logout(): void;
}

const AuthContext = createContext<AuthApi>();

/**
 * Provider — wrap the application root with this exactly once.
 * Children call `useAuth()` to read / drive the session.
 */
export function AuthProvider(props: { children: JSX.Element }): JSX.Element {
  // 1.16b: start in the "anon" state instead of "loading" because
  // there is no persisted session to rehydrate from yet. 1.16c will
  // flip the initial value to "loading" and resolve once IndexedDB
  // has answered.
  const [state, setState] = createSignal<AuthState>({ kind: "anon" });

  const api: AuthApi = {
    state,
    async signup(username, _password) {
      // Stub — see file-level docstring. The real OPAQUE flow lands
      // in 1.16c and goes through `lakko-ffi-wasm`'s auth surface.
      await Promise.resolve();
      setState({
        kind: "session",
        account: {
          accountId: `stub:${username}`,
          username,
        },
      });
    },
    async login(username, _password) {
      // Stub — same story as `signup`.
      await Promise.resolve();
      setState({
        kind: "session",
        account: {
          accountId: `stub:${username}`,
          username,
        },
      });
    },
    logout() {
      setState({ kind: "anon" });
    },
  };

  return (
    <AuthContext.Provider value={api}>{props.children}</AuthContext.Provider>
  );
}

/**
 * Hook — every component that needs to read or drive the session
 * calls this. Throws if invoked outside `<AuthProvider>` because
 * that is a wiring bug, not a runtime condition we want to paper
 * over.
 */
export function useAuth(): AuthApi {
  const ctx = useContext(AuthContext);
  if (ctx === undefined) {
    throw new Error("useAuth() called outside <AuthProvider>");
  }
  return ctx;
}
