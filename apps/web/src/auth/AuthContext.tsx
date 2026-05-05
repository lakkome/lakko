// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Auth context — single source of truth for "is the user logged in?"
 *
 * 1.16b shipped this as a stub that flipped state in-memory. 1.16c3b
 * replaces the stubs with real OPAQUE-PAKE round-trips through
 * `lakko-ffi-wasm`'s `OpaqueRegister` / `OpaqueLogin` classes,
 * driven by the fetch-based [`AuthClient`].
 *
 * ## Account-id model (Phase 1.16c3b minimal scope)
 *
 * The Lakko server identifies accounts by an opaque 16-byte
 * UUIDv7 the client picks at signup. There is no `username →
 * account_id` directory until `lakko-svc-discovery` lands in
 * 1.16d. To make signup→logout→login cycles work on a single
 * device today, we cache `(accountId, username)` as plaintext
 * in `localStorage`:
 *
 *   * the `accountId` itself is **not secret** — the server
 *     returns it on every successful login, and a network
 *     observer with HTTPS broken sees it anyway;
 *   * the `username` is purely a UI label until 1.16d wires
 *     discovery.
 *
 * What is NOT in `localStorage`:
 *
 *   * The OPAQUE export key. It lives in memory only; the user
 *     re-types the password on every page reload. 1.16c3c will
 *     persist a derived KEK in non-extractable WebCrypto.
 *   * The session key. Same story — single-page-load lifetime.
 *
 * ## What is NOT here yet (1.16c3c / 1.16d)
 *
 *   * BIP39 recovery passphrase generation on signup.
 *   * Persistent device root key (WebCrypto-backed).
 *   * `username → accountId` lookup for first-time login on a
 *     fresh device.
 *   * Multi-account on a single device.
 */
import {
  createContext,
  createSignal,
  onMount,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";

import { AuthClient } from "./client";
import { base64Decode, base64Encode } from "./account-id";

/** Account record exposed to the UI when a session is active. */
export interface SessionAccount {
  /** base64(account_id) — the canonical id we send to the server. */
  readonly accountId: string;
  /** Human-visible handle, e.g. `@alice`. Local-only until 1.16d. */
  readonly username: string;
}

/** Discriminated union for the auth state. */
export type AuthState =
  | { readonly kind: "loading" }
  | {
      readonly kind: "anon";
      /** If a previous session is cached on this device, expose
       *  the username so the login form can pre-fill it. */
      readonly cachedUsername?: string;
    }
  | { readonly kind: "session"; readonly account: SessionAccount };

/** Result handed back to the signup form so it can display the
 *  freshly-generated 24-word recovery passphrase exactly once. */
export interface SignupOutcome {
  /** The 24-word BIP39 recovery phrase. The form must show this
   *  to the user and require an explicit "I have written it down"
   *  acknowledgement before navigating away. */
  recoveryPhrase: string;
}

/** Public surface of the auth context. */
export interface AuthApi {
  readonly state: Accessor<AuthState>;
  /** Run the OPAQUE-PAKE registration flow.
   *
   *  Throws on network / server errors (the form surfaces them).
   *  On success, persists `(accountId, username)` to localStorage
   *  and flips state to `session`. The returned [`SignupOutcome`]
   *  carries the BIP39 recovery phrase that the UI MUST display
   *  exactly once. */
  signup(username: string, password: string): Promise<SignupOutcome>;
  /** Run the OPAQUE-PAKE login flow.
   *
   *  In 1.16c3b, login is only available for the cached account
   *  on this device. If the typed `username` does not match the
   *  cached one, throws — the user has to sign up first or wait
   *  for 1.16d's discovery service. */
  login(username: string, password: string): Promise<void>;
  /** Drop the in-memory session. Does NOT erase the cached
   *  `(accountId, username)` — re-login on the same device
   *  should work. Use [`forgetDevice`] for a clean wipe. */
  logout(): void;
  /** Erase the localStorage cache for this device, then logout.
   *  Phase 1.16c3c will extend this to also clear the
   *  WebCrypto-stored device root key. */
  forgetDevice(): void;
}

const AuthContext = createContext<AuthApi>();

const STORAGE_KEY = "lakko.auth.v1";

interface PersistedSession {
  /** Schema marker — bump if the storage shape changes so old
   *  rows are recognised and dropped instead of mis-parsed. */
  v: 1;
  accountIdB64: string;
  username: string;
}

function loadPersisted(): PersistedSession | null {
  // SSR-safety: AuthProvider runs in the browser, but a defensive
  // check costs us nothing.
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedSession>;
    if (
      parsed.v === 1 &&
      typeof parsed.accountIdB64 === "string" &&
      typeof parsed.username === "string"
    ) {
      return {
        v: 1,
        accountIdB64: parsed.accountIdB64,
        username: parsed.username,
      };
    }
    return null;
  } catch {
    return null;
  }
}

function savePersisted(record: PersistedSession): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(record));
}

function clearPersisted(): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(STORAGE_KEY);
}

/** Encode a UTF-8 string into bytes — `password` enters
 *  OPAQUE-PAKE as raw bytes. */
function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

export function AuthProvider(props: { children: JSX.Element }): JSX.Element {
  // Start in `loading` so the very first paint doesn't snap to
  // `/login`; the onMount hook reads localStorage and resolves.
  const [state, setState] = createSignal<AuthState>({ kind: "loading" });

  const client = new AuthClient();

  onMount(() => {
    // Synchronous localStorage read — no need for a Promise here.
    const cached = loadPersisted();
    if (cached === null) {
      setState({ kind: "anon" });
    } else {
      // We have a cached identity but no in-memory secrets — the
      // user must re-authenticate. Stay anon, expose the cached
      // username so the login form pre-fills.
      setState({ kind: "anon", cachedUsername: cached.username });
    }
  });

  const api: AuthApi = {
    state,
    async signup(username, password) {
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error("username must be non-empty");
      }
      const result = await client.signup(utf8(password));
      const accountIdB64 = base64Encode(result.accountId);
      savePersisted({ v: 1, accountIdB64, username: trimmed });
      setState({
        kind: "session",
        account: { accountId: accountIdB64, username: trimmed },
      });
      if (result.recoveryPhrase === undefined) {
        // 1.16c3c1 always enrols recovery — a missing phrase here
        // means a code-path bypass we want to fail loud on, not
        // silently navigate the user past their only recovery
        // option.
        throw new Error(
          "internal: signup completed without a recovery phrase",
        );
      }
      return { recoveryPhrase: result.recoveryPhrase };
    },
    async login(username, password) {
      const trimmed = username.trim();
      const cached = loadPersisted();
      if (cached === null) {
        throw new Error(
          "no account on this device — sign up first " +
            "(cross-device login arrives in 1.16d)",
        );
      }
      if (cached.username !== trimmed) {
        throw new Error(
          `this device only knows @${cached.username}; sign up @${trimmed} first`,
        );
      }
      const accountIdBytes = base64Decode(cached.accountIdB64);
      await client.login(accountIdBytes, utf8(password));
      setState({
        kind: "session",
        account: {
          accountId: cached.accountIdB64,
          username: cached.username,
        },
      });
    },
    logout() {
      const cached = loadPersisted();
      setState({
        kind: "anon",
        ...(cached !== null ? { cachedUsername: cached.username } : {}),
      });
    },
    forgetDevice() {
      clearPersisted();
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
