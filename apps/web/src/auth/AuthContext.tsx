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
import {
  DiscoveryClient,
  DiscoveryHttpError,
} from "../discovery/client";
import { clearAll as clearContactsStore } from "../contacts/local-store";
import { useChats } from "../chat/ChatContext";
import { deleteMlsSnapshot } from "../chat/mls-store";
import { rehydrateMlsState } from "../chat/rehydrate";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

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

/** Identity secrets held in memory for the current session.
 *
 *  Populated whenever the user has just authenticated (signup,
 *  login, recover) — those flows have the OPAQUE export key in
 *  scope and can derive the Identity. **Empty after a
 *  page reload** that restored the session via the IDB device
 *  root key (1.16c3c2) — the export key is not persisted, so
 *  the secrets cannot be re-derived without a password.
 *
 *  Operations that need to sign on behalf of the user
 *  (`ContactsClient.sendContactRequest`, MLS commits, …) require
 *  this to be set; surface a "please re-enter your password"
 *  prompt when it's `null`.
 *
 *  The bytes themselves are held — not a `wasm.Identity`
 *  instance — because:
 *   * they're cheap (96 bytes total) and clone-friendly;
 *   * a WASM instance carries lifetime baggage and a heavy
 *     `free()` requirement;
 *   * downstream callers reconstruct an `Identity` on demand
 *     via `wasm.Identity.fromSeed(exportKey)` when needed —
 *     each reconstruction is sub-millisecond.
 */
export interface IdentitySecrets {
  /** Raw 16-byte UUIDv7. */
  readonly accountId: Uint8Array;
  /** OPAQUE password export key (64 bytes for `LakkoCipherSuite`).
   *  Used as the seed for `Identity.fromSeed`. */
  readonly exportKey: Uint8Array;
}

/** Public surface of the auth context. */
export interface AuthApi {
  readonly state: Accessor<AuthState>;
  /** In-memory Identity secrets (1.16d3c). `null` whenever the
   *  user has not just authenticated in the current page session
   *  — see [`IdentitySecrets`] for the full shape and lifetime
   *  story. */
  readonly identitySecrets: Accessor<IdentitySecrets | null>;
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
  // 1.16d3c: in-memory Identity secrets. Filled on signup /
  // login / recover, dropped on logout, NOT populated on a
  // session restore from IDB (the export key isn't persisted).
  const [identitySecrets, setIdentitySecrets] =
    createSignal<IdentitySecrets | null>(null);

  const client = new AuthClient();
  const discovery = new DiscoveryClient();
  // 1.16e5: drop active groups on logout/forgetDevice so MLS
  // state doesn't bleed across user sessions on the same tab.
  const chats = useChats();

  onMount(() => {
    void hydrateFromStorage().then(setState);
  });

  const api: AuthApi = {
    state,
    identitySecrets,
    async signup(username, password) {
      const trimmed = username.trim();
      if (trimmed.length === 0) {
        throw new Error("username must be non-empty");
      }
      const result = await client.signup(utf8(password));
      const accountIdB64 = base64Encode(result.accountId);

      // 1.16d2: claim the username server-side. We do this AFTER
      // the OPAQUE flow commits — if /discovery/register fails
      // the account still exists but the username is unbound,
      // which is recoverable on a future signup attempt with the
      // same desired handle (the user picks a different name)
      // or via a "set username" UI later. We ALSO surface the
      // failure rather than silently continuing — a 409 (taken)
      // is meaningful for the form.
      let normalisedUsername: string;
      try {
        const reg = await discovery.registerUsername(
          result.accountId,
          trimmed,
        );
        normalisedUsername = reg.normalisedUsername;
      } catch (err) {
        // Re-throw a friendlier message for the common case.
        if (err instanceof DiscoveryHttpError && err.status === 409) {
          throw new Error(
            `username @${trimmed} is already taken — pick another`,
          );
        }
        throw err;
      }

      savePersisted({
        v: 1,
        accountIdB64,
        username: normalisedUsername,
      });
      // Derive the device root key right after signup and persist
      // it — failing this AFTER the server commit would leave the
      // user with a working server-side account but no on-device
      // session restore. We surface the failure rather than
      // pretending signup succeeded silently.
      const deviceKey = await deriveDeviceRootKey(result.exportKey, accountIdB64);
      await putDeviceKey(accountIdB64, deviceKey);

      // 1.16e6c1: build the canonical session-wide Identity
      // before uploading the KeyPackage so the freshly-minted
      // init/leaf private keys land in the provider we'll
      // snapshot. ChatProvider holds the reference for the
      // lifetime of the session and will free() it on
      // logout/forgetDevice.
      const wasm = await loadWasm();
      const canonical = wasm.Identity.fromSeed(result.exportKey);
      chats.setIdentity(canonical);

      // 1.16e6c3: rehydrate prior MLS state if any. For signup
      // this is a no-op (brand-new accountId, no snapshot row),
      // but routing it through the same helper keeps the four
      // auth flows uniform.
      await rehydrateMlsState(chats, accountIdB64);

      // 1.16d3b2: derive the Identity deterministically from the
      // OPAQUE export key, upload its public bundle to the
      // discovery directory. Because Identity::fromSeed is
      // deterministic, the SAME bytes will be uploaded on every
      // future login — the server upserts. We do this BEFORE
      // flipping state so a failure here surfaces as a signup
      // error rather than a half-broken session.
      await uploadIdentityBundle(discovery, result.accountId, canonical);

      // 1.16e6c1: persist the freshly-built provider state
      // (the just-uploaded KeyPackage's private key material)
      // so a peer's Welcome can be opened after a reload.
      // Best-effort — a snapshot failure here shouldn't block
      // the user from getting into a working session.
      try {
        await chats.persistSnapshot(accountIdB64);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("initial MLS snapshot failed", err);
      }

      // 1.16d3c: hand off the export key for in-memory use by
      // ContactsClient + similar. Cleared on logout.
      setIdentitySecrets({
        accountId: result.accountId,
        exportKey: result.exportKey,
      });
      setState({
        kind: "session",
        account: { accountId: accountIdB64, username: normalisedUsername },
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

      // Resolve the typed username to an account_id. Two paths:
      //   1. Cache hit on this device — same username as cached:
      //      use the cached id directly. Saves one round-trip
      //      and works offline (well, against a cached id; the
      //      OPAQUE flow itself still needs the server).
      //   2. Cache miss OR mismatch — call discovery.lookup. The
      //      result either binds the typed username to its
      //      account_id (cross-device login from a fresh browser)
      //      or returns null (unknown username).
      let accountIdBytes: Uint8Array;
      let resolvedUsername: string;
      if (cached !== null && cached.username === trimmed) {
        accountIdBytes = base64Decode(cached.accountIdB64);
        resolvedUsername = cached.username;
      } else {
        const lookup = await discovery.lookup(trimmed);
        if (lookup.account === null) {
          // Indistinguishable from "wrong password" — we don't
          // help an attacker probe the username space.
          throw new Error("invalid credentials");
        }
        accountIdBytes = lookup.account;
        resolvedUsername = lookup.normalisedUsername;
      }

      const result = await client.login(accountIdBytes, utf8(password));
      const accountIdB64 = base64Encode(accountIdBytes);

      // OPAQUE export_key is deterministic for the same password,
      // so re-deriving the device root key here yields identical
      // bytes (and an equivalent CryptoKey). Persisting it again
      // is harmless — the IDB `put` upserts. We also persist the
      // (accountId, username) marker so subsequent reloads on
      // this device skip the lookup round-trip.
      const deviceKey = await deriveDeviceRootKey(result.exportKey, accountIdB64);
      await putDeviceKey(accountIdB64, deviceKey);
      savePersisted({ v: 1, accountIdB64, username: resolvedUsername });

      // 1.16e6c1: build the canonical Identity for this session
      // (see signup() above for the rationale). ChatProvider
      // free()s any prior canonical Identity automatically.
      const wasm = await loadWasm();
      const canonical = wasm.Identity.fromSeed(result.exportKey);
      chats.setIdentity(canonical);

      // 1.16e6c3: rehydrate prior MLS state from the IDB
      // snapshot. This is the load case that justifies the
      // entire 1.16e6 sub-sprint — same accountId on the same
      // device picks up every active group from the prior
      // session. Failures inside the helper are logged + best-
      // effort; the user still gets a working authenticated
      // session.
      await rehydrateMlsState(chats, accountIdB64);

      // 1.16d3b2: refresh the identity bundle on every login.
      // Best-effort — a network blip here shouldn't block the
      // user from getting into a session they already
      // authenticated. The bundle is deterministic, so the
      // next login will retry naturally.
      try {
        await uploadIdentityBundle(discovery, accountIdBytes, canonical);
      } catch (err) {
        // Surface as a console warning, not a thrown error.
        // eslint-disable-next-line no-console
        console.warn("identity bundle refresh failed", err);
      }

      // 1.16e6c1: persist the freshly-uploaded KeyPackage's
      // private key state. Best-effort.
      try {
        await chats.persistSnapshot(accountIdB64);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("post-login MLS snapshot failed", err);
      }

      // 1.16d3c: stash the export key for ContactsClient.
      setIdentitySecrets({
        accountId: accountIdBytes,
        exportKey: result.exportKey,
      });
      setState({
        kind: "session",
        account: { accountId: accountIdB64, username: resolvedUsername },
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

        // 1.16e6c1: post-recovery the user's password (and so
        // the export key) has changed, which means any
        // previously-persisted MLS snapshot was wrapped under
        // a device key derived from the OLD password and is
        // now unreadable. Drop it; the user is starting MLS
        // state fresh on this device. Active groups will
        // re-bootstrap via the discovery directory's KeyPackage
        // upserts on the peer's next refresh.
        try {
          await deleteMlsSnapshot(cached.accountIdB64);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("deleteMlsSnapshot during recover failed", err);
        }

        // 1.16e6c1: build the new canonical Identity from the
        // rotated export key. ChatProvider free()s the prior
        // (pre-recovery) Identity if anything was lingering.
        const canonical = wasm.Identity.fromSeed(result.exportKey);
        chats.setIdentity(canonical);

        // 1.16e6c3: rehydrate path is a guaranteed no-op here —
        // we just deleted the snapshot row above — but routing
        // through the same helper keeps the auth flows uniform
        // and resilient to future re-orderings.
        await rehydrateMlsState(chats, cached.accountIdB64);

        // Re-publish the identity bundle + a fresh KeyPackage
        // (the recovered Identity has the same long-lived
        // pubkeys, but the KeyPackage's init key is new and
        // its private bytes live in the canonical provider).
        try {
          await uploadIdentityBundle(discovery, accountIdBytes, canonical);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("post-recover identity bundle upload failed", err);
        }
        try {
          await chats.persistSnapshot(cached.accountIdB64);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("post-recover MLS snapshot failed", err);
        }

        // 1.16d3c: stash the new export key so ContactsClient
        // works without a re-login round-trip.
        setIdentitySecrets({
          accountId: accountIdBytes,
          exportKey: result.exportKey,
        });
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
      // any encrypted local stores 1.16f wires up. Drop the
      // in-memory Identity secrets — the user just signed out.
      const cached = loadPersisted();
      setIdentitySecrets(null);
      chats.clear();
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
        // 1.16e6c1: drop the encrypted MLS snapshot for this
        // account. After `deleteDeviceKey` above the snapshot
        // is unreadable anyway (the wrapping AES-256-GCM key is
        // gone), but leaving the row behind means a later
        // signup on the same accountId would surface stale
        // ciphertext on hydration — clean wipe is the
        // contract `forgetDevice` advertises.
        try {
          await deleteMlsSnapshot(cached.accountIdB64);
        } catch {
          // Ignore — same best-effort policy as above.
        }
      }
      clearPersisted();
      // 1.16d3d: drop the on-device contacts cache too —
      // "forget device" should leave nothing per-account
      // behind on this browser.
      clearContactsStore();
      // 1.16e5: drop active MLS group state.
      // 1.16e6c1: also free()s the canonical Identity.
      chats.clear();
      setIdentitySecrets(null);
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
 * Publish everything the directory needs to know about this
 * account: the identity bundle (Ed25519 + X25519 pubkeys,
 * deterministic from the export key — 1.16d3b2) and the MLS
 * KeyPackage (random per call — 1.16e2). Both upserts run in
 * parallel; the calls are independent and the server is
 * idempotent on either side.
 *
 * **Pre-1.16e6c1 footgun fixed.** This used to spin up a
 * throwaway `Identity.fromSeed(exportKey)` and drop it after
 * the upload. That worked for the public-key fields (Ed25519
 * + X25519 are deterministic, so re-derivation reproduces them
 * byte-for-byte), but `createKeyPackage` mints **new private
 * key material per call** and writes it into the provider's
 * storage. Discarding that provider meant any peer who later
 * sent a Welcome targeting our published KeyPackage would hit
 * "init key not in keystore" on our side. We now require the
 * caller to pass the **canonical** Identity that ChatProvider
 * is holding — so the freshly-generated init / leaf private
 * keys land in the provider we'll snapshot at persist time.
 */
async function uploadIdentityBundle(
  discovery: DiscoveryClient,
  accountId: Uint8Array,
  identity: WasmIdentity,
): Promise<void> {
  // 1.16e2: generate a fresh KeyPackage. RFC 9420 forward
  // secrecy on new groups argues for one-shot init_keys; in
  // practice we publish a freshly-randomised one on every
  // login, and the Phase-1 server just upserts.
  const keyPackageBytes = identity.createKeyPackage();

  // Run both uploads in parallel. They're independent — the
  // identity bundle and the KeyPackage live in different
  // tables, neither depends on the other's success. If one
  // fails the caller gets the failure (Promise.all rejects on
  // first); we deliberately do not swallow either error
  // silently because both are required for a usable account.
  await Promise.all([
    discovery.uploadIdentityBundle(
      accountId,
      identity.signaturePublicKey,
      identity.x25519PublicKey,
    ),
    discovery.uploadKeyPackage(accountId, keyPackageBytes),
  ]);
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
