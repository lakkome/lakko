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
 *   * username VERIFICATION against the discovery-signed certificate a
 *     profile carries (ADR-0060; `username-verification.ts`);
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
import { loadKeyAliases, loadRenames, subscribeAcceptedChanged } from "../contacts/local-store";
import { loadWasm, type Wasm } from "../wasm";
import { EMPTY_OWN_PROFILE, loadOwnProfile, saveOwnProfile, type OwnProfile } from "./own-profile";
import { IdbProfileBackend } from "./profile-idb";
import {
  ACCOUNT_ID_BYTES,
  ProfileStore,
  accountIdHexOfBytes,
  type IncomingProfile,
} from "./profile-store";
import { ProfilePublishScheduler, type PublishReason } from "./publish-scheduler";
import { resolveDisplay, type ResolvedName } from "./resolver";
import type { ReceivedGrantWire } from "./profile-envelope";
import { UsernameVerifier } from "./username-verification";
import { isOwnKeyB64 } from "./own-key";
import {
  decideProfileRequest,
  lastProfileRequestAt,
  recordProfileRequest,
} from "./profile-request";
import { journalHex } from "../ds/access";
import { rosterOf } from "../chat/client";
import { reachSession, reachSessionSignal, reachVersionsTick } from "../reach/reach-service";
import { foldProfileGrants } from "../reach/grant-intake";
import {
  installProfileResend,
  owedProfileConversations,
  type ResendConversation,
} from "./rotation-resend";
import { resolveAccountKeyHex } from "../contacts/address-resolver";
import { ScopedDiscoveryClient, usernameCertKey } from "../reach/scoped-discovery";
import { visibilitySetting } from "../reach/visibility-store";
import { grantSignatureOf, recordGrantSignature } from "./grant-signatures";
import { applyHeldServiceEnvelopes } from "../reach/service-envelope-intake";
import {
  bindOwnProfileSync,
  ownProfileChanged,
  subscribeOwnProfileAdopted,
  unbindOwnProfileSync,
} from "./own-profile-sync";

export interface ProfileApi {
  /** Bumped on any change a resolved name may depend on. `resolve` reads it,
   *  so a reactive caller re-renders on its own. */
  readonly version: Accessor<number>;
  /** The viewer's own saved profile. */
  readonly ownProfile: Accessor<OwnProfile>;
  /** The viewer's registered username, `null` for a guest account. */
  readonly ownUsername: Accessor<string | null>;
  /** Save the viewer's display name (already validated), publish it into
   *  every group this device is in and send it to this account's other
   *  devices. `null` clears the name. */
  saveOwnDisplayName(name: string | null): Promise<void>;
  /** Resolve a person — a 32-byte key or its base64. Reactive. */
  resolve(key: Uint8Array | string): ResolvedName;
  /** File a received profile under its AUTHENTICATED sender key. With a
   *  `context`, the reach grants it carried are kept under the ADR-0060 rules
   *  for that conversation. */
  ingest(
    senderKeyB64: string,
    profile: IncomingProfile & { readonly grants?: readonly ReceivedGrantWire[] },
    context?: { readonly groupIdB64: string },
  ): void;
  /** This device joined `groupIdB64` (Welcome, founding, accept). Publishes
   *  the own profile and, right after it, a profile request (ADR-0048 §2
   *  trigger 4) so the members already there introduce themselves. */
  groupJoined(groupIdB64: string): void;
  /** `groupIdB64` gained at least one member. */
  groupGrew(groupIdB64: string): void;
  /** ADR-0060 — `groupIdB64` lost a member (the ADR-0043 signature-key set
   *  difference): rotate this account's `group` and `member` keys for it and
   *  republish the profile. */
  groupShrank(groupIdB64: string): Promise<void>;
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
  /** The own profile envelope, or `null` when there is nothing to publish.
   *  With `groupIdB64` it carries the ADR-0060 grants that conversation gets
   *  (`member` always, `group` when the setting allows, `contact` in a
   *  one-to-one); without, none (a contact request's profile). */
  ownProfileEnvelope(groupIdB64?: string): Promise<Uint8Array | null>;
  /** ADR-0060 — this account's username certificate, `null` until fetched
   *  (or for a guest account). */
  ownUsernameCertificate(): Uint8Array | null;
  /** The 16-byte auth account id a person CLAIMED in their profile, as 32
   *  lower-case hex characters, or `null` when none is held. For the viewer's
   *  own key it is the signed-in account. Reactive. What the call roster
   *  derives the ADR-0021 peer tag from. */
  accountIdHexOf(keyB64: string): string | null;
  /** Every stored profile that carries an account id, as
   *  `(personKeyB64, accountIdHex)` pairs. The own account is not among them
   *  (the device never receives its own envelope). Reactive. */
  accountIdBindings(): readonly AccountIdBinding[];
}

/** One person's claimed account id, keyed on the signature key it was
 *  filed under. */
export interface AccountIdBinding {
  readonly personKeyB64: string;
  readonly accountIdHex: string;
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
  // ADR-0065 R.2: every retired key names the same person as the contact
  // row (display aliases for stored history only), so old bubbles keep the
  // person's name after the row was re-keyed.
  const [aliases, setAliases] = createSignal<Map<string, string>>(loadKeyAliases());
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
  /** The signed-in account's 16-byte id, or `null` when there is no session
   *  or the stored id is not 16 bytes (the core refuses any other length, and
   *  a profile without an id is better than no profile). */
  const ownAccountIdBytes = (): Uint8Array | null => {
    const account = accountId();
    if (account === null) return null;
    try {
      const bytes = base64Decode(account);
      return bytes.length === ACCOUNT_ID_BYTES ? bytes : null;
    } catch {
      return null;
    }
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

  const scoped = new ScopedDiscoveryClient(() => auth.identitySecrets()?.sessionToken ?? null);
  const [ownCert, setOwnCert] = createSignal<Uint8Array | null>(null);
  let certFetch: Promise<void> | null = null;
  /** Fetch this account's username certificate once (retried on the next
   *  publish when it failed). */
  function ensureOwnCertificate(): Promise<void> {
    if (ownCert() !== null || ownUsername() === null) return Promise.resolve();
    if (certFetch === null) {
      certFetch = scoped
        .usernameCertificate()
        .then((c) => {
          if (c !== null) setOwnCert(c);
        })
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("[profile] username certificate not fetched; retried later", err);
        })
        .finally(() => {
          certFetch = null;
        });
    }
    return certFetch;
  }
  const verifier = new UsernameVerifier({
    // ADR-0060 section 5 — a claimed username is verified against the
    // discovery-signed certificate the person's own profile carries: one OPRF
    // evaluation and the pinned key, no lookup (a hidden person answers no
    // lookup). No certificate, or one that does not bind this username to the
    // account the profile claims, is a mismatch.
    resolve: async (username, keyB64) => {
      const held = store.get(keyB64);
      const certB64 = held?.usernameCertificateB64 ?? null;
      const accountHex = held?.accountIdHex ?? null;
      if (certB64 === null || accountHex === null) return { kind: "unregistered" };
      const w = await loadWasm();
      // The certificate binds the username's LOOKUP KEY, never its OPRF output.
      const [round, key] = await Promise.all([scoped.usernameOprf(username), usernameCertKey()]);
      const accountId = new Uint8Array(16);
      for (let i = 0; i < 16; i++) accountId[i] = Number.parseInt(accountHex.slice(i * 2, i * 2 + 2), 16);
      try {
        w.verifyUsernameBinding(base64Decode(certB64), key, round.lookupKey, accountId).free();
      } catch {
        return { kind: "unregistered" };
      }
      return { kind: "key", signatureKey: base64Decode(keyB64) };
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

  async function ownProfileEnvelope(groupIdB64?: string): Promise<Uint8Array | null> {
    const own = ownProfile();
    const username = ownUsername();
    // A device that never saved a name and has no username has nothing to
    // say. Once a name was ever saved, an empty envelope is meaningful: it
    // clears the name on the receivers.
    if (own.displayName === null && username === null && own.updatedAtMs === 0) {
      // ADR-0060 — the grants still have to travel: a group's members reach
      // us through them. An envelope with no name is a valid profile.
      if (groupIdB64 === undefined) return null;
    }
    const w = await loadWasm();
    await ensureOwnCertificate();
    // ADR-0060 — the grants this conversation gets. A missing reach session
    // (signed out mid-publish) sends none.
    const grants = groupIdB64 === undefined ? [] : grantsFor(groupIdB64);
    try {
      // ADR-0048 v2 — the own account id rides along so a member who never
      // accepted this account as a contact can still derive its ADR-0021 peer
      // tag and name its call tile. `undefined` for a session without one.
      // ADR-0060 v3 — the username certificate, and the grants.
      return w.composeProfileEnvelope(
        own.displayName,
        username,
        BigInt(own.updatedAtMs),
        ownAccountIdBytes() ?? undefined,
        username === null ? undefined : (ownCert() ?? undefined),
        grants,
      );
    } finally {
      // `composeProfileEnvelope` takes the grants by value (moved into the
      // envelope): freeing a moved handle throws, one not moved is freed.
      for (const g of grants) {
        try {
          g.free();
        } catch {
          /* moved */
        }
      }
    }
  }

  /** ADR-0060 — the grants a profile into `groupIdB64` carries. */
  function grantsFor(groupIdB64: string): ReturnType<NonNullable<ReturnType<typeof reachSession>>["profileGrants"]> {
    const session = reachSession();
    const entry = chats.get(groupIdB64);
    if (session === null || entry === undefined) return [];
    return session.profileGrants(base64Decode(groupIdB64), isOneToOne(entry.group));
  }

  async function publishInto(groupIdB64: string, reason: PublishReason): Promise<void> {
    if (accountId() === null) return;
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    // A group this device is no longer in can never carry another message.
    if (readIsActive(entry.group) === false) return;
    // ADR-0060 / ADR-0066 section 3.5a rule 3 - what the grants composed
    // below carry, read BEFORE composing: recorded only once the publish
    // landed, so a conversation whose profile did not get out stays owed.
    const account = accountId();
    const signature = grantSignatureNow(groupIdB64);
    const envelope = await ownProfileEnvelope(groupIdB64);
    if (envelope === null) return;
    void reason;
    await publisher.broadcastControl(secrets, entry.group, envelope);
    if (account !== null && signature !== null && grantSignatureNow(groupIdB64) === signature) {
      recordGrantSignature(account, groupIdB64, signature);
    }
  }

  /** The grant signature a profile into `groupIdB64` would carry now, or
   *  `null` without a reach session or the group. */
  function grantSignatureNow(groupIdB64: string): string | null {
    const session = reachSession();
    const entry = chats.get(groupIdB64);
    if (session === null || entry === undefined) return null;
    return session.profileGrantSignature(base64Decode(groupIdB64), isOneToOne(entry.group));
  }

  /** The conversations whose profile (and grants) is not published yet. */
  function owedConversations(): string[] | null {
    const account = accountId();
    const session = reachSession();
    const own = chats.accountIdentity();
    if (account === null || session === null || own === null) return null;
    // The reach session of ANOTHER key (it restarts after a rotation) would
    // sign grants for the mailbox that was left.
    if (session.accountKeyHex !== journalHex(own.signaturePublicKey)) return null;
    const rows: ResendConversation[] = [];
    for (const g of chats.groups()) {
      rows.push({
        groupIdB64: g.groupIdB64,
        active: readIsActive(g.group) !== false,
        publishedSignature: grantSignatureOf(account, g.groupIdB64),
        currentSignature: session.profileGrantSignature(base64Decode(g.groupIdB64), isOneToOne(g.group)),
      });
    }
    return owedProfileConversations(rows);
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

  /** Conversations the grant-signature effect has a publish out for. */
  const resendInFlight = new Set<string>();

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
        unbindOwnProfileSync();
        scheduler.stop();
        store.close();
        return;
      }
      // ADR-0048 section 4a — the own name between this account's devices.
      bindOwnProfileSync(account, ownAccountIdBytes());
      void applyHeldServiceEnvelopes();
      scheduler.start();
      void store.open(account).then(() => {
        // Verify every held claim that has no fresh verdict yet.
        for (const [keyB64, p] of store.entries()) verifyIfClaimed(keyB64, p.username);
      });
    });
  });

  // ADR-0060 — the grants a profile carries follow the "who can find me"
  // setting and the key versions: whenever what a conversation's profile
  // WOULD carry differs from what was last published there (a setting change,
  // a rotation, a conversation that never received grants), the profile is
  // republished into it. The reach session registers or removes the keys
  // itself. The published signatures are versions only, no key material.
  createEffect(() => {
    const setting = visibilitySetting();
    const session = reachSessionSignal();
    const groups = chats.groups();
    // A key version changed here or on another device (ADR-0060 section 7).
    reachVersionsTick();
    untrack(() => {
      const account = accountId();
      if (setting === null || session === null || account === null) return;
      // The signature is recorded by `publishInto` once the publish landed
      // (ADR-0066 section 3.5a rule 3): a conversation whose profile did not
      // get out is due again the next time this runs.
      const due: string[] = [];
      for (const g of groups) {
        if (readIsActive(g.group) === false) continue;
        const signature = session.profileGrantSignature(base64Decode(g.groupIdB64), isOneToOne(g.group));
        if (grantSignatureOf(account, g.groupIdB64) === signature) continue;
        if (resendInFlight.has(g.groupIdB64)) continue;
        resendInFlight.add(g.groupIdB64);
        due.push(g.groupIdB64);
      }
      if (due.length > 0) {
        void scheduler.profileChanged(due).finally(() => {
          for (const g of due) resendInFlight.delete(g);
        });
      }
    });
  });

  // ADR-0066 section 3.5a rule 3 - the rotating device's `profile` step:
  // send every owed profile now and answer how many are still owed.
  installProfileResend(async () => {
    const owed = owedConversations();
    if (owed === null) return null;
    if (owed.length > 0) await scheduler.publishAll(owed);
    return owedConversations()?.length ?? null;
  });
  onCleanup(() => installProfileResend(null));

  // ADR-0048 section 4a — a name set on another device of this account was
  // adopted: re-read the own profile. Not republished into groups (the device
  // that made the change already did).
  const unsubscribeOwnAdopted = subscribeOwnProfileAdopted(() => {
    setOwnProfileTick((t) => t + 1);
    bump();
  });

  const unsubscribeRenames = subscribeAcceptedChanged(() => {
    setRenames(loadRenames());
    setAliases(loadKeyAliases());
    bump();
    // Re-write the row so the service worker's rename mirror follows.
    const account = accountId();
    if (account !== null) store.resave(account);
  });
  onCleanup(() => {
    unsubscribeStore();
    unsubscribeRenames();
    unsubscribeOwnAdopted();
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
      // ADR-0048 section 4a — and to this account's other devices.
      ownProfileChanged();
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
      // ADR-0065 R.2: an alias (a retired key) resolves through the contact's
      // row key: its rename, and its profile when the alias key sent none.
      const canonical = aliases().get(keyB64) ?? keyB64;
      const profile = store.get(keyB64) ?? (canonical === keyB64 ? undefined : store.get(canonical));
      const profileKeyB64 = store.get(keyB64) !== undefined ? keyB64 : canonical;
      return resolveDisplay({
        signatureKey: keyBytes,
        rename: renames().get(canonical),
        profile,
        verification:
          profile?.username !== null && profile?.username !== undefined
            ? verifier.state(profileKeyB64, profile.username)
            : undefined,
      });
    },
    ingest(senderKeyB64, profile, context) {
      const account = accountId();
      if (account === null) return;
      void store.ingest(account, senderKeyB64, profile).then((replaced) => {
        if (replaced) verifyIfClaimed(senderKeyB64, store.get(senderKeyB64)?.username ?? null);
      });
      // ADR-0060 — the grants, kept under the rules for this conversation and
      // filed under the sender key resolved at receipt (C-68).
      const session = reachSession();
      const entry = context === undefined ? undefined : chats.get(context.groupIdB64);
      if (session !== null && context !== undefined && entry !== undefined) {
        let senderHex: string;
        try {
          // ADR-0066 section 3.5a rule 2 - keyed on the PERSON: a leaf bound
          // to a proven predecessor files under the account's current key
          // (and this account's own other devices read as this account).
          senderHex = resolveAccountKeyHex(journalHex(base64Decode(senderKeyB64)));
        } catch {
          return;
        }
        const oneToOne = isOneToOne(entry.group);
        void session
          .updateReceivedGrants((g) =>
            foldProfileGrants(g, {
              senderHex,
              ownHex: session.accountKeyHex,
              scopeHex: journalHex(base64Decode(context.groupIdB64)),
              oneToOne,
              grants: profile.grants ?? [],
              nowMs: Date.now(),
            }),
          )
          .then(() => bump());
      }
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
    async groupShrank(groupIdB64) {
      // ADR-0060 section 3 — new `group` and `member` keys, registered before
      // the old ones go, then the profile carrying them republished. Called
      // strictly after the removal commit merged (the C-61 shape): the
      // evicted member cannot open the republished profile.
      const session = reachSession();
      if (session === null) return;
      try {
        await session.rotateOnShrink(base64Decode(groupIdB64));
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[profile] reach rotation after a shrink failed; retried at the next sync", err);
      }
      await scheduler.profileChanged([groupIdB64]);
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
    ownUsernameCertificate: () => ownCert(),
    accountIdHexOf(keyB64) {
      version();
      const own = chats.identity();
      if (own !== null && isOwnKeyB64(own, keyB64)) return accountIdHexOfBytes(ownAccountIdBytes());
      return store.get(keyB64)?.accountIdHex ?? null;
    },
    accountIdBindings() {
      version();
      const out: AccountIdBinding[] = [];
      for (const [personKeyB64, p] of store.entries()) {
        if (p.accountIdHex !== null) out.push({ personKeyB64, accountIdHex: p.accountIdHex });
      }
      return out;
    },
  };

  return <ProfileContext.Provider value={api}>{props.children}</ProfileContext.Provider>;
}

export function useProfiles(): ProfileApi {
  const ctx = useContext(ProfileContext);
  if (ctx === undefined) throw new Error("useProfiles() called outside <ProfileProvider>");
  return ctx;
}

/**
 * ADR-0060 — a conversation is a one-to-one when its roster holds exactly two
 * people (a person being one account key, however many devices). A torn
 * roster is not one: a `contact` grant is then neither sent nor kept.
 */
export function isOneToOne(group: Parameters<typeof rosterOf>[0]): boolean {
  const people = new Set<string>();
  for (const m of rosterOf(group)) people.add(journalHex(m.accountKey ?? m.signaturePublicKey));
  return people.size === 2;
}

