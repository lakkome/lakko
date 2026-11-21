// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * People search, wired to the running app: the wasm core, the discovery and
 * DS clients, the profile store, the address book and the kept reach grants.
 *
 * Every decision lives in `people.ts`, `people-search.ts`, `introductions.ts`
 * and `send-request.ts`; this file only answers their questions from real
 * sources, so it holds no rule of its own.
 */
import { nameLookupAll, type NameLookupAnyHit } from "../discovery/handle-lookup";
import type { LookupResponse } from "../discovery/wire";
import { avatarFromWasm } from "../profile/avatar-model";
import type { FoundConversation } from "./people-search";
import type { ContactsClient } from "../contacts/client";
import { ContactsError, isAnswerUnreachable } from "../contacts/client";
import { contactsErrorText } from "../contacts/errors";
import type { ProfileApi } from "../profile/ProfileContext";
import { loadAccepted } from "../contacts/local-store";
import { base64Decode, base64Encode } from "../auth/account-id";
import type { IdentitySecrets } from "../auth/AuthContext";
import { DiscoveryClient } from "../discovery/client";
import { boundX25519 } from "../discovery/x25519-binding";
import { DsClient } from "../ds/client";
import { personKeysOf, verifiedTargetOfGrant } from "../reach/mailbox-write";
import { provenSuccessorWith } from "../contacts/proven-successor";
import { grantsInOrderForPerson } from "../reach/received-grants";
import { personKeysHex } from "../contacts/address-resolver";
import { reachSession, reachSignerForGrantWire } from "../reach/reach-service";
import { loadIntroductions } from "../reach/scoped-intake";
import { ScopedDiscoveryClient, usernameCertKey } from "../reach/scoped-discovery";
import type { Wasm } from "../wasm";
import { openMatchingIntroductions, recordsOf } from "./introductions";
import { searchNormalizer, toHex, type LocalCandidate, type Normalize } from "./people";
import type { KnownPerson, OpenedPublicRecord, PeopleSearchDeps, SearchCounts } from "./people-search";
import type { SendDeps } from "./send-request";

/** The core's search normaliser, with the short-input fallback. */
export function normalizerOf(wasm: Wasm): Normalize {
  return searchNormalizer((s) => wasm.normalizeNameForSearch(s));
}

/** The address book's keys (standard base64 account keys). */
function contactKeySet(): Set<string> {
  return new Set(loadAccepted().map((c) => c.signaturePubkeyB64));
}

/**
 * Every grant kept for a person, in the order they are tried. ADR-0074
 * section 3.5a rule 2 - the grants of the PERSON, under any key a proven
 * rotation left behind. All of them: a person may have rotated one away and
 * still answer to another.
 */
function keptGrantWiresOf(keyHex: string): readonly string[] {
  const session = reachSession();
  if (session === null) return [];
  try {
    return grantsInOrderForPerson(session.receivedGrants(), personKeysHex(keyHex)).map((g) => g.wireB64);
  } catch {
    return [];
  }
}

/**
 * Whether a grant this device holds for the person still reaches them: the
 * DS reach check, grant by grant, in the order a write would try them
 * (`docs/ux/who-can-find-me.md` section 6.4, step 1). `false` when none is
 * held or every one is refused. Throws what the check throws: a check that
 * could not be made is no refusal.
 */
export async function heldGrantReaches(identityPk: Uint8Array): Promise<boolean> {
  for (const wire of keptGrantWiresOf(toHex(identityPk))) {
    if (await reachCheckWith(identityPk, wire)) return true;
  }
  return false;
}

/** What this device knows about one person (by account key, base64). */
export function knownPerson(
  profiles: ProfileApi,
  keyB64: string,
  contactKeys: ReadonlySet<string> = contactKeySet(),
): KnownPerson {
  const isContact = contactKeys.has(keyB64);
  const resolved = profiles.resolve(keyB64);
  const named = resolved.source === "rename" || resolved.source === "profile";
  let keptGrantWires: readonly string[] = [];
  try {
    keptGrantWires = keptGrantWiresOf(toHex(base64Decode(keyB64)));
  } catch {
    keptGrantWires = [];
  }
  return {
    // Only a name this device holds counts as a name. A contact nothing
    // names still shows (their username or fingerprint), but as a stand-in:
    // a name they published beats it, so a name search finds them.
    name: named ? resolved.name : null,
    contactFallbackName: isContact && !named ? resolved.name : null,
    verifiedUsername: resolved.username === null ? null : resolved.username.replace(/^@+/, ""),
    isContact,
    keptGrantWires,
  };
}

/**
 * The people this device already knows: its contacts, the people in its
 * groups (the rosters' account keys) and every profile it holds with an
 * account id (group and channel members alike).
 */
export function localCandidates(
  profiles: ProfileApi,
  groupPeopleKeysB64: readonly string[],
): LocalCandidate[] {
  const keys = new Map<string, string | null>();
  const contacts = loadAccepted();
  const contactKeys = new Set(contacts.map((c) => c.signaturePubkeyB64));
  for (const c of contacts) keys.set(c.signaturePubkeyB64, c.accountIdHex ?? null);
  for (const k of groupPeopleKeysB64) if (!keys.has(k)) keys.set(k, null);
  for (const b of profiles.accountIdBindings()) {
    if (!keys.has(b.personKeyB64) || keys.get(b.personKeyB64) === null) {
      keys.set(b.personKeyB64, b.accountIdHex);
    }
  }
  const out: LocalCandidate[] = [];
  for (const [keyB64, contactAccountIdHex] of keys) {
    let keyHex: string;
    try {
      keyHex = toHex(base64Decode(keyB64));
    } catch {
      continue;
    }
    const k = knownPerson(profiles, keyB64, contactKeys);
    out.push({
      keyB64,
      keyHex,
      name: k.name ?? k.contactFallbackName,
      ...(k.name === null && k.contactFallbackName !== null ? { nameIsFallback: true } : {}),
      verifiedUsername: k.verifiedUsername,
      isContact: k.isContact,
      // A claimed account id only locates the identity bundle; the send path
      // checks the bundle's key against this key before anything goes out.
      accountIdHex: contactAccountIdHex ?? profiles.accountIdHexOf(keyB64),
      keptGrantWires: k.keptGrantWires,
    });
  }
  return out;
}

/** The canonical username the typed text could be, or `null`. */
export function canonicalUsernameOf(wasm: Wasm, typed: string): string | null {
  const bare = wasm.bareName(typed);
  if (!wasm.looksLikeAName(bare)) return null;
  try {
    return wasm.validateUsername(bare);
  } catch {
    return null;
  }
}

/** `openPublicRecord`, reduced to the fields the search reads and freed.
 *  `null` when the record does not open (or opens with a grant). */
export function openPublicRecordWith(
  wasm: Wasm,
  record: Uint8Array,
  oprfOutput: Uint8Array,
): OpenedPublicRecord | null {
  let payload: ReturnType<Wasm["openPublicRecord"]>;
  try {
    payload = wasm.openPublicRecord(record, oprfOutput);
  } catch {
    return null;
  }
  if (payload === undefined) return null;
  try {
    return {
      identityPk: payload.identityPk,
      accountId: payload.accountId,
      displayName: payload.displayName ?? null,
      username: payload.username ?? null,
      usernameCertificate: payload.usernameCertificate ?? null,
      // ADR-0078 - where an unsigned request to this person goes.
      mailboxBinding: payload.mailboxBinding,
    };
  } finally {
    payload.free();
  }
}

/** The DS reach check of a person's account mailbox with a kept grant wire.
 *  ADR-0078: the mailbox is the one the grant's binding names, verified
 *  against the person's key (and its proven chain); a binding that does not
 *  verify is a refusal, and no request is made. */
export async function reachCheckWith(identityPk: Uint8Array, grantWireB64: string): Promise<boolean> {
  const target = await verifiedTargetOfGrant(grantWireB64, personKeysOf(identityPk));
  if (target === null) return false;
  const signer = await reachSignerForGrantWire(grantWireB64);
  try {
    return await new DsClient().reachCheck(target.address, signer);
  } finally {
    try {
      signer.free();
    } catch {
      /* already freed */
    }
  }
}

/**
 * ADR-0074 section 3.5a - whether the key `accountId` publishes now is the
 * proven successor of `foundKey`: the pin store's proven chain, or the
 * supersession proof the published bundle carries, verified under the found
 * key. A failed fetch proves nothing.
 */
async function provenPredecessorOf(
  discovery: DiscoveryClient,
  foundKey: Uint8Array,
  accountId: Uint8Array,
): Promise<boolean> {
  try {
    const bundle = await discovery.identityFetch(accountId);
    if (bundle === null) return false;
    return await provenSuccessorWith(foundKey, base64Decode(bundle.signature_pubkey), bundle);
  } catch {
    return false;
  }
}

/** The lookup keys of usernames a record stated, by username. A key depends
 *  on the name alone, so a person found again costs no token. Memory only;
 *  the oldest goes first. Chosen, not measured. */
const MAX_KEPT_USERNAME_LOOKUP_KEYS = 64;
const usernameLookupKeys = new Map<string, Uint8Array>();

/** One line per finished network search: counts only. */
function logSearchCounts(c: SearchCounts): void {
  // eslint-disable-next-line no-console
  console.info(
    `[search] finished records_held=${c.recordsHeld} records_opened=${c.recordsOpened} ` +
      `kept_after_match=${c.keptAfterMatch} refused_by_reach_check=${c.refusedByReachCheck} ` +
      `people=${c.people}`,
  );
}

/** The answers of one search's lookups, by lookup key (base64), so the
 *  conversation question reads what the person question already fetched. */
const handleLookups = new Map<string, LookupResponse | null>();
const termLookups = new Map<string, NameLookupAnyHit[]>();

/** `openSearchRecord` for a public group or channel; `null` otherwise. */
export function openConversationRecordWith(
  wasm: Wasm,
  record: Uint8Array,
  output: Uint8Array,
): FoundConversation | null {
  let opened;
  try {
    opened = wasm.openSearchRecord(record, output);
  } catch {
    return null;
  }
  if (opened === undefined) return null;
  try {
    const c = opened.conversation;
    if (c === undefined) return null;
    try {
      const kind = c.kind === "channel" ? "channel" : "group";
      return {
        kind,
        idB64: base64Encode(c.id),
        handle: c.handle,
        name: c.name,
        description: c.description,
        sizeBand: c.sizeBand,
        key: c.key,
        knockVersion: c.knockVersion,
        avatar: avatarFromWasm(c.avatar),
      };
    } finally {
      c.free();
    }
  } finally {
    opened.free();
  }
}

/** The network search, answered from the running app. */
export function peopleSearchDeps(args: {
  readonly wasm: Wasm;
  readonly profiles: ProfileApi;
  readonly sessionToken: () => string | null;
}): PeopleSearchDeps {
  const { wasm, profiles } = args;
  const discovery = new DiscoveryClient(undefined, { sessionToken: args.sessionToken });
  const scoped = new ScopedDiscoveryClient(args.sessionToken);
  let recordsHeld = 0;
  return {
    canonicalUsername: (typed) => canonicalUsernameOf(wasm, typed),
    queryTerm: (query) => wasm.nameQueryTerm(query) ?? null,
    nameMatches: (name, query) => wasm.nameMatchesQuery(name, query),
    // ADR-0087 section 1: a channel's `@name` is a handle in the one
    // namespace; the conversations question below finds it (by `@name` and by
    // name) from the same OPRF rounds, so no separate channel question.
    usernameOprf: (username) => scoped.usernameOprf(username),
    usernameLookupKey: async (username) => {
      const held = usernameLookupKeys.get(username);
      if (held !== undefined) return held;
      const { lookupKey } = await scoped.usernameOprf(username);
      usernameLookupKeys.set(username, lookupKey);
      const oldest = usernameLookupKeys.keys().next().value;
      if (usernameLookupKeys.size > MAX_KEPT_USERNAME_LOOKUP_KEYS && oldest !== undefined) {
        usernameLookupKeys.delete(oldest);
      }
      return lookupKey;
    },
    accountOfUsernameLookupKey: async (lookupKey) => {
      const res = await discovery.lookupRaw({ oprf_hash: base64Encode(lookupKey) });
      handleLookups.set(base64Encode(lookupKey), res);
      if (res === null) return null;
      // ADR-0087 section 1: one namespace; a group or channel answers here
      // with its record and no account (read by the conversation question).
      if ((res as { kind?: string }).kind === "handle" || res.account_id === undefined) return null;
      let publicRecord: Uint8Array | null = null;
      if (res.public_record_b64 !== undefined) {
        try {
          publicRecord = base64Decode(res.public_record_b64);
        } catch {
          publicRecord = null;
        }
      }
      return { accountId: base64Decode(res.account_id), publicRecord };
    },
    nameTermOprf: (term) => scoped.nameTermOprf(term),
    accountsOfNameTermLookupKey: async (lookupKey) => {
      const all = await nameLookupAll(lookupKey);
      termLookups.set(base64Encode(lookupKey), all);
      return all.flatMap((h) =>
        h.kind === "person" && h.accountId !== null ? [{ accountId: h.accountId, publicRecord: h.publicRecord }] : [],
      );
    },
    conversationRecordOfLookupKey: async (lookupKey) => {
      const key = base64Encode(lookupKey);
      const res = handleLookups.has(key)
        ? handleLookups.get(key)
        : await discovery.lookupRaw({ oprf_hash: key });
      if (res === null || res === undefined) return null;
      if ((res as { kind?: string }).kind !== "handle" || res.public_record_b64 === undefined) return null;
      try {
        return base64Decode(res.public_record_b64);
      } catch {
        return null;
      }
    },
    conversationRecordsOfTermLookupKey: async (lookupKey) => {
      const key = base64Encode(lookupKey);
      const all = termLookups.get(key) ?? (await nameLookupAll(lookupKey));
      return all.flatMap((h) => (h.kind === "handle" && h.publicRecord !== null ? [h.publicRecord] : []));
    },
    openConversationRecord: (record, output) => openConversationRecordWith(wasm, record, output),
    openPublicRecord: (record, oprfOutput) => openPublicRecordWith(wasm, record, oprfOutput),
    identityKeyOf: async (accountId) => {
      const bundle = await discovery.identityFetch(accountId);
      return bundle === null ? null : base64Decode(bundle.signature_pubkey);
    },
    provenPredecessor: (foundKey, accountId) => provenPredecessorOf(discovery, foundKey, accountId),
    known: (keyB64) => knownPerson(profiles, keyB64),
    openIntroductions: async (probes) => {
      const session = reachSession();
      if (session === null) return [];
      const row = await loadIntroductions(session.accountIdB64);
      const records = recordsOf(row, wasm);
      recordsHeld = records.length;
      return openMatchingIntroductions(records, probes, wasm);
    },
    introductionRecordsHeld: () => recordsHeld,
    logCounts: logSearchCounts,
    verifyUsername: async (certificate, usernameLookupKey, accountId) => {
      try {
        const cert = wasm.verifyUsernameBinding(certificate, await usernameCertKey(), usernameLookupKey, accountId);
        cert.free();
        return true;
      } catch {
        return false;
      }
    },
    reachCheck: reachCheckWith,
    encodeGrant: (wire) => base64Encode(wire),
    ownKeyHex: reachSession()?.accountKeyHex ?? null,
  };
}

/** The request senders, answered from the running app. */
export function sendDeps(args: {
  readonly wasm: Wasm;
  readonly client: ContactsClient;
  readonly identity: () => IdentitySecrets | null;
  readonly peerLabel: string;
}): SendDeps {
  const { wasm, client } = args;
  const discovery = new DiscoveryClient();
  /** A wasm grant for one write, freed after. */
  const withGrant = async <T>(
    grantWireB64: string | null,
    use: (grant: ReturnType<Wasm["decodeReachGrant"]> | null) => Promise<T>,
  ): Promise<T> => {
    if (grantWireB64 === null) return use(null);
    const grant = wasm.decodeReachGrant(base64Decode(grantWireB64));
    try {
      return await use(grant);
    } finally {
      grant.free();
    }
  };
  return {
    reachCheck: reachCheckWith,
    heldGrantWires: (identityPk) => keptGrantWiresOf(toHex(identityPk)),
    identityFetch: async (accountId) => {
      const bundle = await discovery.identityFetch(accountId);
      if (bundle === null) return null;
      // Security review 2026-11-21: a key whose binding does not verify is
      // never sealed to; the person reads as not reachable.
      const x25519Pub = await boundX25519(bundle);
      if (x25519Pub === null) return null;
      return {
        signaturePub: base64Decode(bundle.signature_pubkey),
        x25519Pub,
      };
    },
    provenPredecessor: (foundKey, accountId) => provenPredecessorOf(discovery, foundKey, accountId),
    sendByUsername: async (username, intro, grantWireB64) => {
      await withGrant(grantWireB64, (writeGrant) =>
        client.sendContactRequest(args.identity(), username, intro, { writeGrant }),
      );
    },
    sendToBundle: async (accountId, signaturePub, x25519Pub, intro, grantWireB64, openBindingB64) => {
      await withGrant(grantWireB64, (writeGrant) =>
        client.sendContactRequestToBundle(args.identity(), accountId, signaturePub, x25519Pub, intro, {
          writeGrant,
          openBinding:
            openBindingB64 === null || openBindingB64 === undefined ? null : base64Decode(openBindingB64),
        }),
      );
    },
    // G-53 - no kept grant reaching the person (every one refused, or none
    // the plan offers) is a refusal too: the request then goes inside a
    // shared group (`docs/ux/who-can-find-me.md` section 6.4).
    isRefusal: (err) =>
      (err instanceof ContactsError && err.variant.kind === "username-unknown") || isAnswerUnreachable(err),
    errorMessage: (err) => contactsErrorText(err, "contacts.error.send", args.peerLabel),
  };
}
