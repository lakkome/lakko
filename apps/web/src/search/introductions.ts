// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Contacts of contacts: opening the introduction records this device keeps
 * (ADR-0058 section 4).
 *
 * A contact relays the records of THEIR contacts as a bundle; a record is
 * sealed under the OPRF output of each search term of the introduced
 * person's name and username, so only someone who searched for one of those
 * terms can open it. The searcher compares the record's tags with
 * `fofTag(oprf)` for the OPRF outputs it holds (the query term's, the
 * username's) and opens only a record whose tag matches.
 *
 * A record that matched a tag and then failed to authenticate is a forgery,
 * not a miss: it is skipped, never shown. What a record claims is the
 * introduced person's claim - the caller still filters by the whole query,
 * verifies the username, and runs the reach check before showing it.
 *
 * The wasm functions arrive as an argument, so a test can drive the real
 * core.
 */
import type {
  IntroductionPayload,
  IntroductionsBundle,
  ReachGrant,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode } from "../auth/account-id";

/** The core functions this module uses. */
export interface IntroductionCore {
  introductionTags(record: Uint8Array): Uint8Array;
  fofTag(oprfOutput: Uint8Array): Uint8Array;
  openIntroduction(record: Uint8Array, oprfOutput: Uint8Array): IntroductionPayload | undefined;
  decodeIntroductionsBundle(plaintext: Uint8Array): IntroductionsBundle;
  encodeReachGrant(grant: ReachGrant): Uint8Array;
}

/** The kept introductions row (`reach/scoped-intake.ts`), as far as it is
 *  read here. */
export interface KeptIntroductions {
  readonly offers: Readonly<Record<string, { readonly recordB64: string }>>;
  readonly bundles: Readonly<Record<string, { readonly bundleB64: string }>>;
}

/** Which OPRF output a record was opened with. */
export type ProbeVia = "term" | "username";

export interface IntroductionProbe {
  readonly oprf: Uint8Array;
  readonly via: ProbeVia;
}

/** What an opened record revealed, copied out of the wasm objects. */
export interface OpenedIntroduction {
  readonly identityPk: Uint8Array;
  readonly accountId: Uint8Array;
  readonly displayName: string | null;
  readonly username: string | null;
  readonly usernameCertificate: Uint8Array | null;
  /** The contacts-of-contacts grant wire. Secret. */
  readonly fofWire: Uint8Array;
  readonly via: ProbeVia;
}

/** Tag length (`fofTagLen()`). */
const TAG_LEN = 32;

/** Every record wire the row holds: each relayed bundle's records (padding
 *  included - a dummy never matches) and each contact's own offer. A bundle
 *  that fails to decode contributes nothing. */
export function recordsOf(row: KeptIntroductions, core: IntroductionCore): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (const { bundleB64 } of Object.values(row.bundles)) {
    let bundle: IntroductionsBundle;
    try {
      bundle = core.decodeIntroductionsBundle(base64Decode(bundleB64));
    } catch {
      continue;
    }
    try {
      const count = bundle.recordCount();
      for (let i = 0; i < count; i++) {
        const record = bundle.record(i);
        if (record !== undefined) out.push(record);
      }
    } finally {
      bundle.free();
    }
  }
  for (const { recordB64 } of Object.values(row.offers)) {
    try {
      out.push(base64Decode(recordB64));
    } catch {
      /* a malformed row names nobody */
    }
  }
  return out;
}

function tagsContain(tags: Uint8Array, tag: Uint8Array): boolean {
  if (tag.length !== TAG_LEN) return false;
  for (let off = 0; off + TAG_LEN <= tags.length; off += TAG_LEN) {
    let equal = true;
    for (let i = 0; i < TAG_LEN; i++) {
      if (tags[off + i] !== tag[i]) {
        equal = false;
        break;
      }
    }
    if (equal) return true;
  }
  return false;
}

/**
 * Open every record one of the probes' tags matches. A record matched by two
 * probes is opened once (the first probe wins). Never throws.
 */
export function openMatchingIntroductions(
  records: readonly Uint8Array[],
  probes: readonly IntroductionProbe[],
  core: IntroductionCore,
): OpenedIntroduction[] {
  const probeTags: { tag: Uint8Array; probe: IntroductionProbe }[] = [];
  for (const probe of probes) {
    try {
      probeTags.push({ tag: core.fofTag(probe.oprf), probe });
    } catch {
      /* a malformed OPRF output opens nothing */
    }
  }
  const out: OpenedIntroduction[] = [];
  for (const record of records) {
    let tags: Uint8Array;
    try {
      tags = core.introductionTags(record);
    } catch {
      continue;
    }
    const hit = probeTags.find((p) => tagsContain(tags, p.tag));
    if (hit === undefined) continue;
    let payload: IntroductionPayload | undefined;
    try {
      payload = core.openIntroduction(record, hit.probe.oprf);
    } catch {
      // A tag matched and the record did not authenticate: a forgery.
      continue;
    }
    if (payload === undefined) continue;
    try {
      const grant = payload.fof;
      let fofWire: Uint8Array;
      try {
        fofWire = core.encodeReachGrant(grant);
      } finally {
        grant.free();
      }
      out.push({
        identityPk: payload.identityPk,
        accountId: payload.accountId,
        displayName: payload.displayName ?? null,
        username: payload.username ?? null,
        usernameCertificate: payload.usernameCertificate ?? null,
        fofWire,
        via: hit.probe.via,
      });
    } catch {
      /* a payload that cannot be read shows nothing */
    } finally {
      payload.free();
    }
  }
  return out;
}
