// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — the session-wide profile runtime.
 *
 * Owns, for the signed-in account:
 *
 *   * the viewer's OWN profile (name + registered username) and the envelope
 *     published from it (`own-profile.ts`);
 *   * the store of RECEIVED profiles, keyed on authenticated signature keys
 *     (`profile-store.ts`, encrypted in IndexedDB by `profile-idb.ts`);
 *   * username VERIFICATION through discovery (`username-verification.ts`);
 *   * WHEN to publish into groups (`publish-scheduler.ts`) and the publishing
 *     itself, through `ChatClient.broadcastControl` — the locked,
 *     persist-before-publish service-envelope path the push routing secret and
 *     the committer beacon use;
 *   * the ONE display resolver every screen calls (`resolver.ts`).
 *
 * Mounted inside `<AuthProvider>` and `<ChatProvider>` and above the router,
 * so the mailbox loop (Welcome joins, contact requests) and every route share
 * one instance.
 */

import {
  createContext,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { useChats } from "../chat/ChatContext";
import { ChatClient } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import { DiscoveryClient } from "../discovery/client";
import { loadRenames, subscribeAcceptedChanged } from "../contacts/local-store";
import { loadWasm, type Wasm } from "../wasm";
import { EMPTY_OWN_PROFILE, loadOwnProfile, saveOwnProfile, type OwnProfile } from "./own-profile";
import { IdbProfileBackend } from "./profile-idb";
import { ProfileStore, type IncomingProfile } from "./profile-store";
import { ProfilePublishScheduler, type PublishReason } from "./publish-scheduler";
import { resolveDisplay, type ResolvedName } from "./resolver";
import { UsernameVerifier } from "./username-verification";
import { isOwnKeyB64 } from "./own-key";
import {
  decideProfileRequest,
  lastProfileRequestAt,
  recordProfileRequest,
} from "./profile-request";

export interface ProfileApi {
  /** Bumped on any change a resolved name may depend on. `resolve` reads it,
   *  so a reactive caller re-renders on its own. */
  readonly version: Accessor<number>;
  /** The viewer's own saved profile. */
  readonly ownProfile: Accessor<OwnProfile>;
  /** The viewer's registered username, `null` for a guest account. */
  readonly ownUsername: Accessor<string | null>;
  /** Save the viewer's display name (already validated) and publish it into
   *  every group this device is in. `null` clears the name. */
  saveOwnDisplayName(name: string | null): Promise<void>;
  /** Resolve a person — a 32-byte key or its base64. Reactive. */
  resolve(key: Uint8Array | string): ResolvedName;
  /** File a received profile under its AUTHENTICATED sender key. */
  ingest(senderKeyB64: string, profile: IncomingProfile): void;
  /** This device joined `groupIdB64` (Welcome, founding, accept). Publishes
   *  the own profile and, right after it, a profile request (ADR-0048 §2
   *  trigger 4) so the members already there introduce themselves. */
  groupJoined(groupIdB64: string): void;
  /** `groupIdB64` gained at least one member. */
  groupGrew(groupIdB64: string): void;
  /** Another member asked `groupIdB64` to introduce itself: answer with the
   *  own profile through the throttled growth path. A device with no own
   *  profile publishes nothing, as on growth. */
  requested(groupIdB64: string): void;
  /** On opening `groupIdB64`: ask the group to introduce itself if some
   *  person in `memberKeysB64` (the roster's people, base64 account keys)
   *  still has no name on this device, at most once a day per group. */
  requestProfilesOnOpen(groupIdB64: string, memberKeysB64: readonly string[]): Promise<void>;
  /** Record a username binding a lookup already proved. */
  learnUsername(signatureKeyB64: string, username: string): void;
  /** The own profile envelope, or `null` when there is nothing to publish. */
  ownProfileEnvelope(): Promise<Uint8Array | null>;
}

const ProfileContext = createContext<ProfileApi>();

export function ProfileProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();

  const [version, setVersion] = createSignal(0);
  const bump = (): void => {
    setVersion((v) => v + 1);
  };

  // Derived synchronously from the signed-in account, not set by an effect:
  // a route rendered on a cold load reads it before any effect of this
  // provider has run. `ownProfileTick` re-reads it after a save.
  const [ownProfileTick, setOwnProfileTick] = createSignal(0);
  const [renames, setRenames] = createSignal<Map<string, string>>(loadRenames());
  let wasm: Wasm | null = null;
  void loadWasm().then((w) => {
    wasm = w;
  });

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const ownProfile = createMemo<OwnProfile>(() => {
    ownProfileTick();
    const account = accountId();
    return account === null ? EMPTY_OWN_PROFILE : loadOwnProfile(account);
  });
  const ownUsername = (): string | null => {
    const s = auth.state();
    if (s.kind !== "session") return null;
    return s.account.username.length > 0 ? s.account.username : null;
  };

  const store = new ProfileStore(
    new IdbProfileBackend(getDeviceKey, () => untrack(renames)),
    (incoming, stored) => (wasm !== null ? wasm.profileIsNewer(incoming, stored) : incoming > stored),
    (err) => {
      // eslint-disable-next-line no-console
      console.warn("profile store persist failed", err);
    },
  );
  const unsubscribeStore = store.subscribe(bump);

  const discovery = new DiscoveryClient();
  const verifier = new UsernameVerifier({
    resolve: async (username) => {
      const lookup = await discovery.lookup(username);
      if (lookup.account === null) return { kind: "unregistered" };
      const bundle = await discovery.identityFetch(lookup.account);
      if (bundle === null) return { kind: "unregistered" };
      return { kind: "key", signatureKey: base64Decode(bundle.signature_pubkey) };
    },
    validate: (username) => {
      if (wasm === null) throw new Error("core not loaded");
      return wasm.validateUsername(username);
    },
    cached: (keyB64, username) => store.verification(keyB64, username),
    record: (keyB64, username, v) => {
      const account = accountId();
      if (account !== null) store.recordVerification(account, keyB64, username, v);
    },
    now: () => Date.now(),
    decodeKey: base64Decode,
  });

  function verifyIfClaimed(keyB64: string, username: string | null): void {
    if (username === null) return;
    // The core must be loaded first: `validate` refusing for want of it would
    // otherwise be recorded as a mismatch.
    void loadWasm()
      .then(() => {
        const done = verifier.ensure(keyB64, username).then(bump);
        bump(); // show `pending` while the lookup runs
        return done;
      })
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[profile] username verification did not run", err);
      });
  }

  // The chat client used ONLY to publish profiles. Its echoes are shared with
  // every other ChatClient on the page (`chat/own-published.ts`).
  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const account = accountId();
      if (account === null) return;
      await chats.persistSnapshot(account);
    },
  });

  async function ownProfileEnvelope(): Promise<Uint8Array | null> {
    const own = ownProfile();
    const username = ownUsername();
    // A device that never saved a name and has no username has nothing to
    // say. Once a name was ever saved, an empty envelope is meaningful: it
    // clears the name on the receivers.
    if (own.displayName === null && username === null && own.updatedAtMs === 0) return null;
    const w = await loadWasm();
    return w.composeProfileEnvelope(own.displayName, username, BigInt(own.updatedAtMs));
  }

  async function publishInto(groupIdB64: string, reason: PublishReason): Promise<void> {
    if (accountId() === null) return;
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    // A group this device is no longer in can never carry another message.
    if (readIsActive(entry.group) === false) return;
    const envelope = await ownProfileEnvelope();
    if (envelope === null) return;
    void reason;
    await publisher.broadcastControl(secrets, entry.group, envelope);
  }

  /**
   * ADR-0048 §2 trigger 4 — publish the field-less "introduce yourselves"
   * envelope into `groupIdB64`, through the same locked application-send
   * path the profile envelope uses. Stamps the persisted last-asked time
   * BEFORE the publish, so a slow or failing send cannot let the on-open
   * check fire a second request moments later. Never throws.
   */
  async function publishProfileRequest(groupIdB64: string): Promise<void> {
    const account = accountId();
    if (account === null) return;
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    if (readIsActive(entry.group) === false) return;
    recordProfileRequest(account, groupIdB64, Date.now());
    try {
      const w = await loadWasm();
      await publisher.broadcastControl(secrets, entry.group, w.composeProfileRequest());
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[profile] request not published", err);
    }
  }

  const scheduler = new ProfilePublishScheduler<ReturnType<typeof setTimeout>>({
    now: () => Date.now(),
    setTimer: (fn, ms) => setTimeout(fn, ms),
    clearTimer: (t) => clearTimeout(t),
    random: () => Math.random(),
    publish: publishInto,
    onError: (_groupIdB64, err) => {
      // eslint-disable-next-line no-console
      console.warn("[profile] publish failed", err);
    },
  });

  // Bind everything to the signed-in account.
  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      if (account === null) {
        scheduler.stop();
        store.close();
        return;
      }
      scheduler.start();
      void store.open(account).then(() => {
        // Verify every held claim that has no fresh verdict yet.
        for (const [keyB64, p] of store.entries()) verifyIfClaimed(keyB64, p.username);
      });
    });
  });

  const unsubscribeRenames = subscribeAcceptedChanged(() => {
    setRenames(loadRenames());
    bump();
    // Re-write the row so the service worker's rename mirror follows.
    const account = accountId();
    if (account !== null) store.resave(account);
  });
  onCleanup(() => {
    unsubscribeStore();
    unsubscribeRenames();
    scheduler.stop();
  });

  const api: ProfileApi = {
    version,
    ownProfile,
    ownUsername,
    async saveOwnDisplayName(name) {
      const account = accountId();
      if (account === null) throw new Error("no session");
      saveOwnProfile(account, name, Date.now());
      setOwnProfileTick((t) => t + 1);
      bump();
      await scheduler.profileChanged(chats.groups().map((g) => g.groupIdB64));
    },
    resolve(key) {
      version();
      let keyBytes: Uint8Array;
      let keyB64: string;
      if (typeof key === "string") {
        keyB64 = key;
        try {
          keyBytes = base64Decode(key);
        } catch {
          keyBytes = new Uint8Array(0);
        }
      } else {
        keyBytes = key;
        keyB64 = base64Encode(key);
      }
      // The viewer's own row. Rosters key a person on the ACCOUNT key
      // (ADR-0050), the device's signature key names one of their
      // phones; a key that matches either is this account, and it is
      // named from the own profile rather than from the received store,
      // which never holds it (the device never receives its own
      // envelope). Comparing the device key alone left the viewer's own
      // row reading `Member` on every group screen (2026-09-19).
      const own = chats.identity();
      if (own !== null && isOwnKeyB64(own, keyB64)) {
        const mine = ownProfile();
        const username = ownUsername();
        return resolveDisplay({
          signatureKey: keyBytes,
          profile: { displayName: mine.displayName, username },
          // The account's own registered username needs no lookup.
          verification: "verified",
        });
      }
      const profile = store.get(keyB64);
      return resolveDisplay({
        signatureKey: keyBytes,
        rename: renames().get(keyB64),
        profile,
        verification:
          profile?.username !== null && profile?.username !== undefined
            ? verifier.state(keyB64, profile.username)
            : undefined,
      });
    },
    ingest(senderKeyB64, profile) {
      const account = accountId();
      if (account === null) return;
      void store.ingest(account, senderKeyB64, profile).then((replaced) => {
        if (replaced) verifyIfClaimed(senderKeyB64, store.get(senderKeyB64)?.username ?? null);
      });
    },
    groupJoined(groupIdB64) {
      // The own profile first — the members already there cannot learn the
      // newcomer's name any other way — then the request for theirs. `joined`
      // never rejects (the scheduler retries a failed publish itself), so the
      // request is not gated on the profile having landed.
      void scheduler.joined(groupIdB64).then(() => publishProfileRequest(groupIdB64));
    },
    groupGrew(groupIdB64) {
      scheduler.groupGrew(groupIdB64);
    },
    requested(groupIdB64) {
      // The growth path: throttled to one publish per group per 30 s and
      // jittered, so N members answering one request do not land N envelopes
      // in the same second. `publishInto` stays silent without an own profile.
      scheduler.groupGrew(groupIdB64);
    },
    async requestProfilesOnOpen(groupIdB64, memberKeysB64) {
      const account = accountId();
      const own = chats.identity();
      if (account === null || own === null) return;
      const w = await loadWasm();
      // The count reads the SAME resolver every screen renders from, so
      // "unnamed" here is exactly a row that shows as `Member`.
      const decision = decideProfileRequest(
        {
          joinedJustNow: false,
          memberKeysB64,
          isOwnKey: (k) => isOwnKeyB64(own, k),
          sourceOf: (k) => api.resolve(k).source,
          lastRequestedAtMs: lastProfileRequestAt(account, groupIdB64),
          nowMs: Date.now(),
        },
        (joined, unnamed, last, now) => w.shouldRequestProfiles(joined, unnamed, last, now),
      );
      if (!decision.ask) return;
      await publishProfileRequest(groupIdB64);
    },
    learnUsername(signatureKeyB64, username) {
      if (accountId() === null) return;
      verifier.learn(signatureKeyB64, username);
      bump();
    },
    ownProfileEnvelope,
  };

  return <ProfileContext.Provider value={api}>{props.children}</ProfileContext.Provider>;
}

export function useProfiles(): ProfileApi {
  const ctx = useContext(ProfileContext);
  if (ctx === undefined) throw new Error("useProfiles() called outside <ProfileProvider>");
  return ctx;
}
