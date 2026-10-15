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
import type { ContactsClient } from "../contacts/client";
import { ContactsError } from "../contacts/client";
import { contactsErrorText } from "../contacts/errors";
import type { ProfileApi } from "../profile/ProfileContext";
import { loadAccepted } from "../contacts/local-store";
import { base64Decode, base64Encode } from "../auth/account-id";
import type { IdentitySecrets } from "../auth/AuthContext";
import { DiscoveryClient } from "../discovery/client";
import { DsClient } from "../ds/client";
import { bytesToHex } from "../channels/composer";
import { personKeysOf, verifiedTargetOfGrant } from "../reach/mailbox-write";
import { provenSuccessorWith } from "../contacts/proven-successor";
import { bestGrantForPerson } from "../reach/received-grants";
import { personKeysHex } from "../contacts/address-resolver";
import { reachSession, reachSignerForGrantWire } from "../reach/reach-service";
import { loadIntroductions } from "../reach/scoped-intake";
import { ScopedDiscoveryClient, usernameCertKey } from "../reach/scoped-discovery";
import type { Wasm } from "../wasm";
import { openMatchingIntroductions, recordsOf } from "./introductions";
import { searchNormalizer, toHex, type LocalCandidate, type Normalize } from "./people";
import type { KnownPerson, OpenedPublicRecord, PeopleSearchDeps } from "./people-search";
import type { SendDeps } from "./send-request";

/** The core's search normaliser, with the short-input fallback. */
export function normalizerOf(wasm: Wasm): Normalize {
  return searchNormalizer((s) => wasm.normalizeNameForSearch(s));
}

/** The address book's keys (standard base64 account keys). */
function contactKeySet(): Set<string> {
  return new Set(loadAccepted().map((c) => c.signaturePubkeyB64));
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
  const session = reachSession();
  let keptGrantWireB64: string | null = null;
  if (session !== null) {
    try {
      keptGrantWireB64 =
        // ADR-0074 section 3.5a rule 2 - the grants of the PERSON, under
        // any key a proven rotation left behind.
        bestGrantForPerson(session.receivedGrants(), personKeysHex(toHex(base64Decode(keyB64))))
          ?.wireB64 ?? null;
    } catch {
      keptGrantWireB64 = null;
    }
  }
  return {
    // Only a name this device holds counts as a name. A contact nothing
    // names still shows (their username or fingerprint), but as a stand-in:
    // a name they published beats it, so a name search finds them.
    name: named ? resolved.name : null,
    contactFallbackName: isContact && !named ? resolved.name : null,
    verifiedUsername: resolved.username === null ? null : resolved.username.replace(/^@+/, ""),
    isContact,
    keptGrantWireB64,
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
      keptGrantWireB64: k.keptGrantWireB64,
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
async function reachCheckWith(identityPk: Uint8Array, grantWireB64: string): Promise<boolean> {
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

/** The network search, answered from the running app. */
export function peopleSearchDeps(args: {
  readonly wasm: Wasm;
  readonly profiles: ProfileApi;
  readonly sessionToken: () => string | null;
}): PeopleSearchDeps {
  const { wasm, profiles } = args;
  const discovery = new DiscoveryClient(undefined, { sessionToken: args.sessionToken });
  const scoped = new ScopedDiscoveryClient(args.sessionToken);
  return {
    canonicalUsername: (typed) => canonicalUsernameOf(wasm, typed),
    queryTerm: (query) => wasm.nameQueryTerm(query) ?? null,
    nameMatches: (name, query) => wasm.nameMatchesQuery(name, query),
    lookupChannel: async (handle) => {
      const found = await discovery.lookupChannel(handle);
      return found.channel === null ? null : bytesToHex(found.channel);
    },
    usernameOprf: (username) => scoped.usernameOprf(username),
    accountOfUsernameLookupKey: async (lookupKey) => {
      const res = await discovery.lookupRaw({ oprf_hash: base64Encode(lookupKey) });
      if (res === null) return null;
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
    accountsOfNameTermLookupKey: (lookupKey) => scoped.nameLookup(lookupKey),
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
      return openMatchingIntroductions(recordsOf(row, wasm), probes, wasm);
    },
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
    identityFetch: async (accountId) => {
      const bundle = await discovery.identityFetch(accountId);
      if (bundle === null) return null;
      return {
        signaturePub: base64Decode(bundle.signature_pubkey),
        x25519Pub: base64Decode(bundle.x25519_pubkey),
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
    isRefusal: (err) => err instanceof ContactsError && err.variant.kind === "username-unknown",
    errorMessage: (err) => contactsErrorText(err, "contacts.error.send", args.peerLabel),
  };
}
