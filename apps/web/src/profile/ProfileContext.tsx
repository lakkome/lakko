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
import { conversationLabelsOf, deletedLabelsOf, realNameOf } from "./member-label";
import { getDeviceKey } from "../auth/key-store";
import { useChats } from "../chat/ChatContext";
import { ChatClient, isConversationBehind } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import type { FoundName } from "../contacts/contact-list-model";
import { isRemovedContact, loadAccepted, loadKeyAliases, loadRenames, subscribeAcceptedChanged } from "../contacts/local-store";
import { loadPendingContacts, pendingContacts, type PendingContact } from "../contacts/pending-contacts";
import { loadWasm, type Wasm } from "../wasm";
import {
  EMPTY_OWN_PROFILE,
  loadOwnProfile,
  saveOwnProfile,
  saveOwnProfileFields,
  restampOwnProfile,
  type OwnProfile,
} from "./own-profile";
import { avatarToWasm, type AvatarData } from "./avatar-model";
import { OWN_AVATAR_KEY, avatarLookupChanged, bindAvatarRuntime, setAvatarLookup } from "./avatar-runtime";
import { IdbProfileBackend } from "./profile-idb";
import {
  ACCOUNT_ID_BYTES,
  FOUND_NAME_STAMP,
  ProfileStore,
  accountIdHexOfBytes,
  awaitsOwnProfile,
  profileCardShareable,
  type IncomingProfile,
} from "./profile-store";
import { ownCardFlagFlipped, ownCardShareable, usernameForConversation, withCardShareSignature } from "./card-share";
import { installBeforeSend } from "../chat/before-send";
import { ProfilePublishScheduler, type PublishReason } from "./publish-scheduler";
import { resolveDisplay, resolvePerson, type ResolvedName } from "./resolver";
import { t } from "../l10n";
import {
  isDeletedAccount,
  personStatusEntries,
  personStatusVersion,
  usernameMayShow,
} from "../people/person-status";
import type { ReceivedGrantWire } from "./profile-envelope";
import { UsernameVerifier } from "./username-verification";
import { isOwnKeyB64, ownKeysB64Of } from "./own-key";
import { subscribeCarriedNames, takeCarriedProfiles } from "../devices/carried-state";
import {
  decideProfileRequest,
  lastProfileRequestAt,
  recordProfileRequest,
} from "./profile-request";
import { journalHex } from "../ds/access";
import { rosterOf } from "../chat/client";
import { reachSession, reachSessionSignal, reachVersionsTick } from "../reach/reach-service";
import { foldProfileGrants, grantsWithVerifiedBindings } from "../reach/grant-intake";
import { personKeysOf } from "../reach/mailbox-write";
import { pruneGrantsOfScope } from "../reach/write-plan";
import {
  installProfileResend,
  owedProfileConversations,
  type ResendConversation,
} from "./rotation-resend";
import { resolveAccountKeyHex } from "../contacts/address-resolver";
import { AccountSlot } from "./account-slot";
import { ScopedDiscoveryClient, usernameCertKey } from "../reach/scoped-discovery";
import { visibilitySetting } from "../reach/visibility-store";
import { grantSignatureOf, recordGrantSignature } from "./grant-signatures";
import { applyHeldServiceEnvelopes } from "../reach/service-envelope-intake";
import {
  bindOwnProfileSync,
  ownProfileChanged,
  subscribeOwnProfileAdopted,
  subscribeOwnStateRestored,
  unbindOwnProfileSync,
} from "./own-profile-sync";
import { durableSnapshotHook } from "../chat/persist-hooks";
import { ownPersonKeyB64, personKeysOf as groupPeopleOf } from "../chat/person-key";
import { signalsWithheldFrom } from "../moderation/block-conversation";
import { profileKeyFor } from "./profile-key";

export interface ProfileApi {
  /** Bumped on any change a resolved name may depend on. `resolve` reads it,
   *  so a reactive caller re-renders on its own. */
  readonly version: Accessor<number>;
  /** The account whose stored profiles have been read, or `null` before
   *  that (and with no session). Until it is the signed-in account,
   *  `resolve` cannot name anybody who is named on this device, so a list
   *  or a conversation header waits for it rather than drawing a
   *  placeholder or a key fingerprint (docs/ux/navigation.md section 2.4). */
  readonly loadedFor: Accessor<string | null>;
  /** The viewer's own saved profile. */
  readonly ownProfile: Accessor<OwnProfile>;
  /** The viewer's registered username, `null` for a guest account. */
  readonly ownUsername: Accessor<string | null>;
  /** Save the viewer's display name (already validated), publish it into
   *  every group this device is in and send it to this account's other
   *  devices. `null` clears the name. */
  saveOwnDisplayName(name: string | null): Promise<void>;
  /** ADR-0084 — save the name and the photo together (the photo already
   *  uploaded), publish into every group and send to this account's other
   *  devices. A new or removed photo is a new profile, like a name. */
  saveOwnProfileFields(fields: { readonly displayName: string | null; readonly avatar: AvatarData | null }): Promise<void>;
  /** The account's username was just claimed (a guest chose one): keep the
   *  certificate the register call returned (`null`: fetched as usual) and
   *  publish the own profile, now with the username and the certificate, to
   *  every conversation and to the account's other devices. */
  usernameClaimed(certificate: Uint8Array | null): Promise<void>;
  /** Resolve a person — a 32-byte key or its base64. Reactive. */
  resolve(key: Uint8Array | string): ResolvedName;
  /** The username the stored profile of `keyB64` CLAIMS, verified or not,
   *  or `null`. Never shown with `@` unless `resolve` says it is verified; a
   *  call tile shows it as plain text while verification is still running
   *  (`calls/tile-name.ts`). Reactive. */
  claimedUsernameOf(keyB64: string): string | null;
  /** ADR-0092 section 7 — whether the newest profile held from `keyB64` lets
   *  others share the person's contact card; `true` when none is held.
   *  Reactive. */
  cardShareableOf(keyB64: string): boolean;
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
  /** A new contact's name as a search found it, filed under their key at
   *  {@link FOUND_NAME_STAMP}: what they send replaces it. Never over a
   *  profile already held. */
  fileFoundName(signatureKeyB64: string, found: FoundName): void;
  /** The own profile envelope, or `null` when there is nothing to publish.
   *  With `groupIdB64` it carries the ADR-0060 grants that conversation gets
   *  (`member` always, `group` when the setting allows, `contact` in a
   *  one-to-one); without, none (a contact request's profile). */
  ownProfileEnvelope(groupIdB64?: string): Promise<Uint8Array | null>;
  /** ADR-0060 — this account's username certificate, `null` until fetched
   *  (or for a guest account). */
  ownUsernameCertificate(): Uint8Array | null;
  /** The certificate, fetched first when none is held yet. `null` when it
   *  could not be had; asked again next time. */
  fetchOwnUsernameCertificate(): Promise<Uint8Array | null>;
  /** The 16-byte auth account id a person CLAIMED in their profile, as 32
   *  lower-case hex characters, or `null` when none is held. For the viewer's
   *  own key it is the signed-in account. Reactive. */
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
    avatarLookupChanged();
  };
  const [loadedFor, setLoadedFor] = createSignal<string | null>(null);

  // Derived synchronously from the signed-in account, not set by an effect:
  // a route rendered on a cold load reads it before any effect of this
  // provider has run. `ownProfileTick` re-reads it after a save.
  const [ownProfileTick, setOwnProfileTick] = createSignal(0);
  const [renames, setRenames] = createSignal<Map<string, string>>(loadRenames());
  // ADR-0073 R.2: every retired key names the same person as the contact
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
    new IdbProfileBackend(
      getDeviceKey,
      () => untrack(renames),
      () => untrack(conversationLabels),
      () => untrack(deletedLabels),
    ),
    (incoming, stored) => (wasm !== null ? wasm.profileIsNewer(incoming, stored) : incoming > stored),
    (err) => {
      // eslint-disable-next-line no-console
      console.warn("profile store persist failed", err);
    },
  );
  const unsubscribeStore = store.subscribe(bump);
  /** The held profile's stamp for a key (`profile-key.ts`). */
  const stampOf = (k: string): bigint | undefined => store.get(k)?.updatedAtMs;
  // ADR-0084 — the one avatar component looks a person's photo up here.
  setAvatarLookup((keyB64) => {
    const own = chats.identity();
    if (keyB64 === OWN_AVATAR_KEY || (own !== null && isOwnKeyB64(own, keyB64))) {
      return untrack(ownProfile).avatar;
    }
    const profileKey = profileKeyFor(keyB64, untrack(aliases), stampOf);
    return profileKey === undefined ? null : store.avatarOf(profileKey);
  });
  onCleanup(() => setAvatarLookup(null));

  const scoped = new ScopedDiscoveryClient(() => auth.identitySecrets()?.sessionToken ?? null);
  const [ownCert, setOwnCert] = createSignal<Uint8Array | null>(null);
  // The certificate is ONE account's: it binds that account's username to
  // that account's id. The slot is emptied on every change of account (the
  // account-bind effect below), and a fetch that returns after the change
  // fills nothing - otherwise the next account's profile envelopes would
  // carry the previous account's certificate.
  const ownCertSlot = new AccountSlot<Uint8Array>((value) => setOwnCert(value));
  /** Fetch this account's username certificate once (retried on the next
   *  publish when it failed). */
  function ensureOwnCertificate(): Promise<void> {
    if (ownUsername() === null) return Promise.resolve();
    return ownCertSlot.ensure(accountId(), () => scoped.usernameCertificate()).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[profile] username certificate not fetched; retried later", err);
    });
  }
  // ADR-0076: the name changed under this session (a rename here, or one
  // adopted from the own-username register): the cached certificate is the
  // OLD name's. Drop it, fetch the current one, and publish the profile
  // again. The first run (the landing) only records the name.
  let certUsername: string | null | undefined;
  createEffect(() => {
    const username = ownUsername();
    const previous = certUsername;
    certUsername = username;
    if (previous === undefined || previous === username) return;
    ownCertSlot.reset();
    if (username === null) return;
    void ensureOwnCertificate().then(() => {
      bump();
      ownProfileChanged();
      return scheduler.profileChanged(chats.groups().map((g) => g.groupIdB64));
    }).catch(() => undefined);
  });
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
    // A lookup that could not reach the directory tries again by itself; the
    // screen shows the name without the username meanwhile.
    scheduleRetry: (retry, delayMs) => {
      setTimeout(() => {
        void retry().then(bump);
      }, delayMs);
    },
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
    onPersist: durableSnapshotHook(chats, accountId),
  });

  /** UX `who-can-find-me.md` section 6.2 — whether a group's profile may
   *  carry the username: a one-to-one always, a group while the setting
   *  lets its members find this person. `null` for no conversation (a
   *  contact request, a call announcement). */
  function usernameScopeOf(groupIdB64: string | undefined): { oneToOne: boolean; groupAllowed: boolean } | null {
    if (groupIdB64 === undefined) return null;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return null;
    if (isOneToOne(entry.group)) return { oneToOne: true, groupAllowed: true };
    const session = reachSession();
    let groupAllowed = false;
    try {
      groupAllowed = session !== null && session.allowsGroupMembers(base64Decode(groupIdB64));
    } catch {
      groupAllowed = false;
    }
    return { oneToOne: false, groupAllowed };
  }

  async function ownProfileEnvelope(groupIdB64?: string): Promise<Uint8Array | null> {
    const own = ownProfile();
    const username = usernameForConversation(ownUsername(), usernameScopeOf(groupIdB64));
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
      // ADR-0048 v2 — the own account id rides along (the username
      // certificate binds the username to it). `undefined` for a session
      // without one.
      // ADR-0060 v3 — the username certificate, and the grants.
      return w.composeProfileEnvelope(
        own.displayName,
        username,
        BigInt(own.updatedAtMs),
        ownAccountIdBytes() ?? undefined,
        username === null ? undefined : (ownCert() ?? undefined),
        grants,
        // ADR-0084 — the saved photo, in every envelope this account sends.
        own.avatar === null ? undefined : avatarToWasm<InstanceType<Wasm["ProfileAvatar"]>>(w, own.avatar),
        // ADR-0092 section 7 — off exactly when "who can find me" is Nobody.
        ownCardShareable(visibilitySetting()),
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

  /** ADR-0060 section 1 (as amended 2026-11-06): a one-to-one carries the
   *  current contact grant only while its other person is a contact; a
   *  removed contact's conversation is not handed the rotated key. */
  function carriesContactGrant(group: Parameters<typeof isOneToOne>[0]): boolean {
    if (!isOneToOne(group)) return false;
    const own = ownPersonKeyB64(chats.identity());
    const peer = groupPeopleOf(group).find((k) => k !== own);
    return peer !== undefined && !isRemovedContact(peer);
  }

  /** ADR-0060 — the grants a profile into `groupIdB64` carries. */
  function grantsFor(groupIdB64: string): ReturnType<NonNullable<ReturnType<typeof reachSession>>["profileGrants"]> {
    const session = reachSession();
    const entry = chats.get(groupIdB64);
    if (session === null || entry === undefined) return [];
    return session.profileGrants(base64Decode(groupIdB64), isOneToOne(entry.group), carriesContactGrant(entry.group));
  }

  async function publishInto(groupIdB64: string, reason: PublishReason): Promise<void> {
    if (accountId() === null) return;
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    // A group this device is no longer in can never carry another message.
    if (readIsActive(entry.group) === false) return;
    // ADR-0099 section 6 - no profile update goes to a blocked person.
    if (signalsWithheldFrom(entry.group, chats.identity())) return;
    // ADR-0060 / ADR-0074 section 3.5a rule 3 - what the grants composed
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
    return profileSignatureOf(session, groupIdB64, entry.group);
  }

  /** What a profile into a conversation carries that may change without a
   *  new name: its grants (ADR-0060), the card flag (ADR-0092 section 7)
   *  and, in a group, whether the username goes (UX `who-can-find-me.md`
   *  section 6.2). A conversation whose published signature differs gets the
   *  profile again. */
  function profileSignatureOf(
    session: NonNullable<ReturnType<typeof reachSession>>,
    groupIdB64: string,
    group: Parameters<typeof isOneToOne>[0],
  ): string {
    const grants = session.profileGrantSignature(base64Decode(groupIdB64), isOneToOne(group), carriesContactGrant(group));
    const scope = usernameScopeOf(groupIdB64);
    const withUser = scope !== null && !scope.oneToOne && !scope.groupAllowed ? `${grants}|nouser` : grants;
    return withCardShareSignature(withUser, ownCardShareable(visibilitySetting()));
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
        currentSignature: profileSignatureOf(session, g.groupIdB64, g.group),
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
    if (signalsWithheldFrom(entry.group, chats.identity())) return;
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
      // ADR-0091: a conversation this device fell behind in sends nothing
      // until it is back; that is the stop list working, not a failure.
      if (isConversationBehind(err)) return;
      // The DS answer (status and error code) explains a failure; the
      // endpoint is left out because it names the conversation.
      const cause = (err as { cause?: unknown }).cause as
        | { name?: unknown; status?: unknown; body?: unknown; message?: unknown }
        | undefined;
      const detail =
        cause === undefined || cause === null
          ? ""
          : ` (${String(cause.name ?? "")}${
              typeof cause.status === "number" ? ` ${cause.status} ${String(cause.body ?? "").slice(0, 80)}` : ""
            })`;
      // eslint-disable-next-line no-console
      console.warn(`[profile] publish failed${detail}`, err);
    },
  });

  // Bind everything to the signed-in account.
  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      // The previous account's username certificate, held or on its way.
      ownCertSlot.rebind(account);
      bindAvatarRuntime(account);
      setLoadedFor(null);
      if (account === null) {
        unbindOwnProfileSync();
        scheduler.stop();
        store.close();
        return;
      }
      // ADR-0079 — the pending contacts name the inviters a link added
      // before any profile of theirs arrived (`resolve`). Loaded here too, so
      // a group screen opened first still names them.
      const pendingRead = loadPendingContacts(account);
      // ADR-0048 section 4a — the own name between this account's devices.
      bindOwnProfileSync(account, ownAccountIdBytes());
      void applyHeldServiceEnvelopes();
      scheduler.start();
      const opened = store.open(account);
      // The stored names are in (received profiles, and the inviters a link
      // added): rows and headers may be drawn now. A store that could not be
      // read is an answer too (neither read rejects).
      void Promise.all([opened, pendingRead]).then(() => {
        if (boundAccount === account) setLoadedFor(account);
      });
      void opened.then(() => {
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
      // (ADR-0074 section 3.5a rule 3): a conversation whose profile did not
      // get out is due again the next time this runs.
      const due: string[] = [];
      for (const g of groups) {
        if (readIsActive(g.group) === false) continue;
        const signature = profileSignatureOf(session, g.groupIdB64, g.group);
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

  // ADR-0092 section 7 — a switch to or from `Nobody` changes the card flag
  // every profile carries. Receivers keep a strictly newer profile only, so
  // the own profile is restamped (the same name and photo, a new stamp) and
  // published everywhere, and sent to this account's other devices like an
  // edit. Loading the setting (from `null`) is not a switch.
  let lastCardFlag: { account: string; shareable: boolean } | null = null;
  createEffect(() => {
    const setting = visibilitySetting();
    const account = accountId();
    untrack(() => {
      if (account === null || setting === null) {
        if (account === null) lastCardFlag = null;
        return;
      }
      const shareable = ownCardShareable(setting);
      const previous = lastCardFlag !== null && lastCardFlag.account === account ? lastCardFlag.shareable : null;
      lastCardFlag = { account, shareable };
      if (!ownCardFlagFlipped(previous, shareable)) return;
      restampOwnProfile(account, Date.now());
      setOwnProfileTick((t) => t + 1);
      bump();
      ownProfileChanged();
      void scheduler.profileChanged(chats.groups().map((g) => g.groupIdB64));
    });
  });

  // ADR-0074 section 3.5a rule 3 - the rotating device's `profile` step:
  // send every owed profile now and answer how many are still owed.
  installProfileResend(async () => {
    const owed = owedConversations();
    if (owed === null) return null;
    if (owed.length > 0) await scheduler.publishAll(owed);
    return owedConversations()?.length ?? null;
  });
  onCleanup(() => installProfileResend(null));
  // ADR-0048 §2 — a profile owed to a group leaves before this device's next
  // message there (`chat/before-send.ts`).
  onCleanup(installBeforeSend((groupIdB64) => scheduler.beforeSend(groupIdB64)));

  // ADR-0048 section 4a — a name set on another device of this account was
  // adopted: re-read the own profile. Not republished into groups (the device
  // that made the change already did).
  const unsubscribeOwnAdopted = subscribeOwnProfileAdopted(() => {
    setOwnProfileTick((t) => t + 1);
    bump();
    // ADR-0084 — a photo saved on another device is the own photo here too,
    // and the member cards follow; the groups already got it.
  });

  // A restore: the stored profile may be new, and the person is told again
  // everywhere, as after any change (the groups this device holds now; the
  // ones it joins later get it from `groupJoined`).
  const unsubscribeOwnRestored = subscribeOwnStateRestored(() => {
    setOwnProfileTick((t) => t + 1);
    bump();
    ownProfileChanged();
    void scheduler.profileChanged(chats.groups().map((g) => g.groupIdB64));
  });

  /** The pending contact (ADR-0079) whose account key is `keyB64` or the
   *  canonical key it aliases, if any. Reactive on the pending list. */
  function pendingContactOf(keyB64: string, canonical: string): PendingContact | undefined {
    return pendingContacts().find(
      (c) => c.signaturePubkeyB64 === keyB64 || c.signaturePubkeyB64 === canonical,
    );
  }

  // ADR-0079 — a pending contact added, removed or handed over to a real
  // contact changes what `resolve` returns for its key: re-resolve.
  //
  // A link-stated username is held to the same rule as a profile's claim:
  // shown only once verified for the key (`verifier.state`). No lookup is
  // started for it here: verification checks the discovery certificate the
  // person's own PROFILE carries (ADR-0060 section 5), and a link carries
  // none, so a lookup could only answer `mismatch` and would put a false
  // "Username not verified" on the contact. The username is verified when
  // their profile arrives (`ingest`), or when a lookup this device already
  // made proved it (`learnUsername`).
  createEffect(() => {
    pendingContacts();
    untrack(bump);
  });

  const unsubscribeRenames = subscribeAcceptedChanged(() => {
    setRenames(loadRenames());
    setAliases(loadKeyAliases());
    bump();
    // Re-write the row so the service worker's rename mirror follows.
    const account = accountId();
    if (account !== null) store.resave(account);
  });

  /**
   * The pairing state bundle v3 (`devices/carried-state.ts`): the profiles
   * the confirming device holds, taken for the bound account once this
   * device knows its own keys, and filed through the same `ingest` a
   * received profile goes through ("strictly newer" wins, a newer one held
   * here stays). A profile filed under one of this account's own keys is
   * never stored.
   */
  function takeCarriedProfilesNow(): void {
    const account = accountId();
    const own = chats.identity();
    if (account === null || own === null) return;
    const ownKeys = ownKeysB64Of(own, [chats.conversationAccountIdentity()?.signaturePublicKey]);
    const carried = takeCarriedProfiles(account);
    for (const [keyB64, profile] of carried) {
      if (ownKeys.includes(keyB64)) continue;
      void store.ingest(account, keyB64, profile).then((replaced) => {
        if (replaced) verifyIfClaimed(keyB64, store.get(keyB64)?.username ?? null);
      });
    }
  }
  createEffect(() => {
    accountId();
    chats.identity();
    untrack(takeCarriedProfilesNow);
  });
  const unsubscribeCarried = subscribeCarriedNames(takeCarriedProfilesNow);

  // The service worker names a person with what the app shows for them in a
  // conversation (`conversationLabels`): the row is written again when the
  // groups, the renames or the key aliases move, not only on a new profile.
  let labelsTimer: ReturnType<typeof setTimeout> | null = null;
  createEffect(() => {
    chats.groups();
    renames();
    aliases();
    pendingContacts();
    // ADR-0098: a person becoming deleted, or a pull-derived deletion being
    // reverted, rewrites the worker's deleted-name mirror.
    personStatusVersion();
    const account = accountId();
    if (account === null) return;
    if (labelsTimer !== null) clearTimeout(labelsTimer);
    labelsTimer = setTimeout(() => store.resave(account), 1000);
  });

  onCleanup(() => {
    if (labelsTimer !== null) clearTimeout(labelsTimer);
    unsubscribeStore();
    unsubscribeRenames();
    unsubscribeOwnAdopted();
    unsubscribeOwnRestored();
    unsubscribeCarried();
    scheduler.stop();
  });

  /**
   * The label the app shows for each member of each conversation this device
   * holds (a real name only; a person the app cannot name is left out),
   * under every key the service worker can be handed for that person: the
   * account key a leaf is bound to and the leaf's own key. Mirrored into the
   * row for the worker, so a notification names a person exactly as the chat
   * does, a group member who is not a contact included.
   */
  function conversationLabels(): Map<string, string> {
    // Every contact, every received profile, and every retired key of a
    // contact, beside the rosters: a sender is always one of these.
    const others = new Set<string>();
    for (const c of loadAccepted()) others.add(c.signaturePubkeyB64);
    for (const c of pendingContacts()) others.add(c.signaturePubkeyB64);
    for (const [key] of store.entries()) others.add(key);
    for (const key of renames().keys()) others.add(key);
    for (const key of aliases().keys()) others.add(key);
    return conversationLabelsOf(
      chats.groups().map((g) => rosterOf(g.group)),
      // A name the app does not know is not written: no placeholder word.
      (person) => realNameOf(api.resolve(person)),
      base64Encode,
      others,
    );
  }

  /**
   * ADR-0098 — "Deleted account" for every key of every person this device
   * holds as deleted, mirrored for the service worker (`x` in the row), which
   * reads it before a rename, a profile or a label.
   */
  function deletedLabels(): Map<string, string> {
    const others = new Set<string>();
    for (const c of loadAccepted()) others.add(c.signaturePubkeyB64);
    for (const c of pendingContacts()) others.add(c.signaturePubkeyB64);
    for (const [key] of store.entries()) others.add(key);
    for (const key of aliases().keys()) others.add(key);
    for (const [key] of personStatusEntries()) others.add(key);
    return deletedLabelsOf(
      chats.groups().map((g) => rosterOf(g.group)),
      (person) => {
        const canonical = aliases().get(person) ?? person;
        return isDeletedAccount(person) || (canonical !== person && isDeletedAccount(canonical));
      },
      base64Encode,
      others,
      t("person.deleted.name"),
    );
  }

  const api: ProfileApi = {
    version,
    loadedFor,
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
    async saveOwnProfileFields(fields) {
      const account = accountId();
      if (account === null) throw new Error("no session");
      saveOwnProfileFields(account, fields, Date.now());
      setOwnProfileTick((t) => t + 1);
      bump();
      ownProfileChanged();
      await scheduler.profileChanged(chats.groups().map((g) => g.groupIdB64));
    },
    async usernameClaimed(certificate) {
      if (certificate !== null) ownCertSlot.put(accountId(), certificate);
      else await ensureOwnCertificate();
      // The username is part of the profile, so a new one is a new profile:
      // re-stamp the saved fields. With the old stamp every receiver took the
      // republished envelope for one it already held, and a rename never
      // reached a contact or a group (base scenario 3, 2026-11-11).
      const account = accountId();
      if (account !== null) {
        const mine = ownProfile();
        saveOwnProfileFields(account, { displayName: mine.displayName, avatar: mine.avatar }, Date.now());
        setOwnProfileTick((t) => t + 1);
      }
      bump();
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
      // ADR-0073 R.2: an alias (a retired key) resolves through the contact's
      // row key: its rename, and its profile when the alias key sent none;
      // and the row key, while its new identity sent no profile yet, through
      // the profile a retired key sent (`profile-key.ts`, UX 16.3).
      const canonical = aliases().get(keyB64) ?? keyB64;
      const profileKey = profileKeyFor(keyB64, aliases(), stampOf);
      const profile = profileKey === undefined ? undefined : store.get(profileKey);
      // ADR-0079 — with no received profile, a pending contact's invite
      // link states the inviter's name and username (signed by their
      // account key). `resolvePerson` decides the precedence; the username
      // is verified under the key the claim is filed under.
      const pending = profile === undefined ? pendingContactOf(keyB64, canonical) : undefined;
      const claimKeyB64 = profileKey ?? pending?.signaturePubkeyB64 ?? keyB64;
      // ADR-0098: a deleted account reads "Deleted account" everywhere; a
      // username is shown only while the account was recently confirmed.
      const deleted = isDeletedAccount(keyB64) || (canonical !== keyB64 && isDeletedAccount(canonical));
      return resolvePerson({
        signatureKey: keyBytes,
        rename: renames().get(canonical),
        received: profile,
        linkStatement: pending,
        verificationOf: (username) => verifier.state(claimKeyB64, username),
        deleted,
        deletedName: deleted ? t("person.deleted.name") : undefined,
        usernameShowable: usernameMayShow(keyB64) || (canonical !== keyB64 && usernameMayShow(canonical)),
      });
    },
    claimedUsernameOf(keyB64) {
      version();
      // A pending contact's link-stated username is a claim like a
      // profile's (ADR-0079), used only when no profile was received.
      const profileKey = profileKeyFor(keyB64, aliases(), stampOf);
      const received = profileKey === undefined ? undefined : store.get(profileKey);
      const username =
        received !== undefined
          ? received.username
          : (pendingContactOf(keyB64, aliases().get(keyB64) ?? keyB64)?.username ?? null);
      return username !== null && username.length > 0 ? username : null;
    },
    cardShareableOf(keyB64) {
      version();
      // A card names the person by their account key; a profile filed under
      // a retired key of theirs answers for them too (ADR-0073 R.2).
      const profileKey = profileKeyFor(keyB64, aliases(), stampOf);
      return profileCardShareable(profileKey === undefined ? undefined : store.get(profileKey));
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
          // ADR-0074 section 3.5a rule 2 - keyed on the PERSON: a leaf bound
          // to a proven predecessor files under the account's current key
          // (and this account's own other devices read as this account).
          senderHex = resolveAccountKeyHex(journalHex(base64Decode(senderKeyB64)));
        } catch {
          return;
        }
        const oneToOne = isOneToOne(entry.group);
        void (async () => {
          // ADR-0078 - a grant is kept only when its mailbox binding verifies
          // against the sender's account key resolved at receipt (C-68) or a
          // proven key of the same person; the rest are dropped here.
          const wasm = await loadWasm();
          const grants = grantsWithVerifiedBindings(
            wasm,
            profile.grants ?? [],
            personKeysOf(base64Decode(senderKeyB64)),
          );
          await session.updateReceivedGrants((g) =>
            foldProfileGrants(g, {
              senderHex,
              ownHex: session.accountKeyHex,
              scopeHex: journalHex(base64Decode(context.groupIdB64)),
              oneToOne,
              grants,
              nowMs: Date.now(),
            }),
          );
          bump();
        })().catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("[reach] profile grants not filed", err instanceof Error ? err.name : "");
        });
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
      // G-53 - a person left: the grants kept from this conversation that a
      // higher version of the same key replaced are deleted (the core
      // decides; a grant at its key's highest version stays). Not awaited.
      void pruneGrantsOfScope(journalHex(base64Decode(groupIdB64)));
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
      // "unnamed" here is a row that shows as `Member`, or one that shows only
      // what a search found: the person's own profile is still owed.
      const decision = decideProfileRequest(
        {
          joinedJustNow: false,
          memberKeysB64,
          isOwnKey: (k) => isOwnKeyB64(own, k),
          sourceOf: (k) => {
            const source = api.resolve(k).source;
            return source !== "rename" && awaitsOwnProfile(store.get(k)) ? "fingerprint" : source;
          },
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
    fileFoundName(signatureKeyB64, found) {
      const account = accountId();
      if (account === null) return;
      // Straight to the store: the check a stored username gets reads the
      // certificate a profile carried, and this row has none. The search
      // that showed the username verified it, so that verdict goes with it.
      void store.ingest(account, signatureKeyB64, { ...found, updatedAtMs: FOUND_NAME_STAMP }).then((stored) => {
        if (stored && found.username !== null) api.learnUsername(signatureKeyB64, found.username);
      });
    },
    ownProfileEnvelope,
    ownUsernameCertificate: () => ownCert(),
    async fetchOwnUsernameCertificate() {
      await ensureOwnCertificate();
      return ownCert();
    },
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

