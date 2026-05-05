// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Auth context — single source of truth for "is the user logged in?"
 *
 * 1.16b: stub that flipped state in-memory.
 * 1.16c3b: real OPAQUE-PAKE round-trips through `OpaqueRegister` /
 *          `OpaqueLogin`.
 * 1.16c3c1: BIP39 recovery passphrase enrolled at signup.
 * 1.16c3c2: session persistence via WebCrypto + IndexedDB.
 *
 * ## Persistence model (1.16c3c2)
 *
 * Two layers of state survive a page reload:
 *
 *   1. **`localStorage`** — `(accountId, username)`. Plaintext.
 *      Neither field is secret; both are routine markers.
 *   2. **IndexedDB** — the device root `CryptoKey`, derived from
 *      the OPAQUE password export key via HKDF-SHA-512 and
 *      stored as **non-extractable** AES-256-GCM. Only the
 *      WebCrypto handle survives the round-trip; the bytes
 *      never leave the keystore.
 *
 * On `AuthProvider` mount we look both up. If both are present,
 * we restore state directly to `session` — no password re-entry
 * required. The OPAQUE export key bytes themselves are NOT
 * persisted; they live in memory only for the duration of the
 * signup / login call and drop the moment the device key has
 * been derived.
 *
 * `logout()` keeps both layers in place — re-auth via password
 * stays cheap. `forgetDevice()` wipes both — full reset, requires
 * the recovery passphrase to come back.
 *
 * ## What is NOT here yet (1.16c3c3 / 1.16d)
 *
 *   * Recover-from-mnemonic flow at `/recover`.
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
import { deriveDeviceRootKey } from "./device-key";
import { deleteDeviceKey, getDeviceKey, putDeviceKey } from "./key-store";
import { loadWasm } from "../wasm";

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
   *  On success, persists `(accountId, username)` to localStorage,
   *  derives the device root key + writes it to IndexedDB, and
   *  flips state to `session`. The returned [`SignupOutcome`]
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
  /** Run the recover-from-mnemonic flow.
   *
   *  Decodes the typed phrase via `RecoveryMnemonic.fromPhrase`,
   *  drives `AuthClient.recover` to rotate the server's password
   *  envelope, derives a fresh device root key from the new
   *  export key, persists it via the IDB pipeline, and flips
   *  state to `session`. Same single-device limitation as
   *  `login` — until 1.16d, the user must already have an
   *  account_id cached on this device. Throws on any failure —
   *  bad mnemonic, network, server rejection, etc. */
  recover(mnemonicPhrase: string, newPassword: string): Promise<void>;
  /** Drop the in-memory session. Does NOT erase the localStorage
   *  cache or the IDB-stored device key — re-login on the same
   *  device should be cheap, and the device key remains usable
   *  for opening any encrypted local stores 1.16f wires up. Use
   *  [`forgetDevice`] for a clean wipe. */
  logout(): void;
  /** Wipe both the localStorage cache and the IDB-stored device
   *  key for this device, then drop the in-memory session. After
   *  this the only way back into the account is via the recovery
   *  passphrase (1.16c3c3 `/recover` flow). */
  forgetDevice(): Promise<void>;
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
  // `/login`; the onMount hook reads localStorage + IDB, then
  // resolves to either `session` (full restore) or `anon`.
  const [state, setState] = createSignal<AuthState>({ kind: "loading" });

  const client = new AuthClient();

  onMount(() => {
    void hydrateFromStorage().then(setState);
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
      // Derive the device root key right after signup and persist
      // it — failing this AFTER the server commit would leave the
      // user with a working server-side account but no on-device
      // session restore. We surface the failure rather than
      // pretending signup succeeded silently.
      const deviceKey = await deriveDeviceRootKey(result.exportKey, accountIdB64);
      await putDeviceKey(accountIdB64, deviceKey);
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
      const result = await client.login(accountIdBytes, utf8(password));
      // OPAQUE export_key is deterministic for the same password,
      // so re-deriving the device root key here yields identical
      // bytes (and an equivalent CryptoKey). Persisting it again
      // is harmless — the IDB `put` upserts.
      const deviceKey = await deriveDeviceRootKey(
        result.exportKey,
        cached.accountIdB64,
      );
      await putDeviceKey(cached.accountIdB64, deviceKey);
      setState({
        kind: "session",
        account: {
          accountId: cached.accountIdB64,
          username: cached.username,
        },
      });
    },
    async recover(mnemonicPhrase, newPassword) {
      const cached = loadPersisted();
      if (cached === null) {
        throw new Error(
          "no account on this device — recovery requires the " +
            "account_id cached locally (cross-device recovery " +
            "arrives in 1.16d via the discovery service)",
        );
      }
      // Parse the phrase up front so a bad mnemonic surfaces
      // before we burn a PoW solve. `fromPhrase` collapses
      // unknown-word and bad-checksum into one error message —
      // by design (preserves BIP39's no-probing property).
      const wasm = await loadWasm();
      const mnemonic = wasm.RecoveryMnemonic.fromPhrase(mnemonicPhrase);
      const seed = mnemonic.toSeed();
      try {
        const accountIdBytes = base64Decode(cached.accountIdB64);
        const result = await client.recover(
          accountIdBytes,
          seed,
          utf8(newPassword),
        );
        // Re-derive the device root key from the NEW export key.
        // The previous device key (if any) is no longer reachable
        // from the new password; the upsert in IDB drops it.
        const deviceKey = await deriveDeviceRootKey(
          result.exportKey,
          cached.accountIdB64,
        );
        await putDeviceKey(cached.accountIdB64, deviceKey);
        setState({
          kind: "session",
          account: {
            accountId: cached.accountIdB64,
            username: cached.username,
          },
        });
      } finally {
        // Best-effort wipe of the seed buffer. It's a Uint8Array
        // we own; zeroing it now means the next GC pass cannot
        // hand it to a leaked closure.
        seed.fill(0);
      }
    },
    logout() {
      // Keep localStorage + IDB intact. Re-login on this device
      // remains cheap, and the device key stays available for
      // any encrypted local stores 1.16f wires up.
      const cached = loadPersisted();
      setState({
        kind: "anon",
        ...(cached !== null ? { cachedUsername: cached.username } : {}),
      });
    },
    async forgetDevice() {
      const cached = loadPersisted();
      // Order matters: clear the IDB key BEFORE clearing the
      // localStorage marker. If we crash between, an IDB key
      // without a localStorage marker is harmless on next mount;
      // a localStorage marker without an IDB key would loop
      // through the broken-restore path forever.
      if (cached !== null) {
        try {
          await deleteDeviceKey(cached.accountIdB64);
        } catch {
          // Ignore — best-effort wipe; localStorage clear below
          // is still worth running.
        }
      }
      clearPersisted();
      setState({ kind: "anon" });
    },
  };

  return (
    <AuthContext.Provider value={api}>{props.children}</AuthContext.Provider>
  );
}

/**
 * Inspect localStorage + IndexedDB and resolve the appropriate
 * initial `AuthState`:
 *
 *   * No localStorage record → fresh device, `anon`.
 *   * localStorage but no IDB key → cached identity, password
 *     re-entry required, `anon` with `cachedUsername`.
 *   * Both present → resume `session` directly, no password.
 */
async function hydrateFromStorage(): Promise<AuthState> {
  const cached = loadPersisted();
  if (cached === null) {
    return { kind: "anon" };
  }
  let key: CryptoKey | null = null;
  try {
    key = await getDeviceKey(cached.accountIdB64);
  } catch {
    // IDB unreachable (private mode in some browsers, quota
    // exhaustion, …). Treat as "no key cached" and ask for the
    // password again rather than crash the app.
    key = null;
  }
  if (key === null) {
    return { kind: "anon", cachedUsername: cached.username };
  }
  return {
    kind: "session",
    account: { accountId: cached.accountIdB64, username: cached.username },
  };
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
