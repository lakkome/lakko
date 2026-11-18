// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pairing state bundle v3 (`state-bundle.ts`): what the SENDING device
 * reads from its encrypted stores into the bundle, and what the RECEIVING
 * device does with it. The decisions are in `carried-state.ts`.
 *
 * Sending: every shared group name of a known group and every received
 * profile, as envelopes composed with the ORIGINAL stamp (never a fresh one:
 * a later rename or profile change must still win), never this account's own
 * profile; every pending contact with what was written to it so far.
 *
 * Receiving: the names go to the session stores through the hand-off
 * (`offerCarriedNames`); a pending contact this device holds in no form is
 * added through the store's ordinary entry point, and its held messages are
 * filed as display-only copies (`contacts/carried-copies.ts`), never into
 * this device's held-send queue.
 */
import { loadGroupNameRows } from "../chat/group-name-idb";
import { heldSends, heldToPersisted } from "../chat/held-send-queue";
import { loadMessageLog, type PersistedHeldSend } from "../chat/message-log-store";
import { loadAccepted } from "../contacts/local-store";
import { storeCarriedCopies } from "../contacts/carried-copies";
import {
  pendingQueueKey,
  readPendingContacts,
  updatePendingContacts,
  withPendingContact,
  type PendingContact,
} from "../contacts/pending-contacts";
import { avatarToWasm, type AvatarWasm } from "../profile/avatar-model";
import { IdbProfileBackend } from "../profile/profile-idb";
import { forStateBundle, type ProfileRow } from "../profile/profile-store";
import { base64Decode, base64Encode } from "../auth/account-id";
import type { DeviceCipher } from "../auth/device-cipher";
import type { Wasm } from "../wasm";
import {
  carriedGroupNamesOf,
  carriedProfilesOf,
  copiesOf,
  offerCarriedNames,
  pendingContactOf,
  pendingContactsToAdd,
} from "./carried-state";
import { carriedSeedsOf } from "./state-bundle";
import { channelSubscriptionPort, liveChannelSubscriptionIds } from "../channels/channel-subscription-register";
import { channelsState, offerCarriedChannelSeeds } from "../state/channels-state";
import type {
  CarriedExtras,
  CarriedGroupName,
  CarriedHeldMessage,
  CarriedPendingContact,
  CarriedProfile,
  StateBundle,
} from "./state-bundle";

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** A held message in the bundle's form. */
function carriedHeld(row: PersistedHeldSend): CarriedHeldMessage {
  const target = row.reply === undefined ? null : Number(row.reply.target);
  return {
    text: row.text,
    attachments: row.attachments.map((a) => ({ ...a })),
    replyTo: target !== null && Number.isSafeInteger(target) ? target : null,
    heldAtMs: Math.floor(row.heldAtMs),
  };
}

/** What was written to `contact` on this device, in order: the page's queue
 *  when it holds the key, else the copy in the message log. */
function heldFor(contact: PendingContact, logHeld: readonly PersistedHeldSend[]): CarriedHeldMessage[] {
  const key = pendingQueueKey(contact);
  const live = heldSends.all().find(([k]) => k === key)?.[1];
  if (live !== undefined && live.length > 0) return live.map((h) => carriedHeld(heldToPersisted(key, h)));
  return logHeld.filter((r) => r.conversationHex === key).map(carriedHeld);
}

/**
 * The received profiles a row holds, each composed again as an envelope with
 * its stored stamp, keyed on the person key it is filed under: what a pairing
 * state bundle and a backup carry of the people this device knows. A profile
 * filed under one of `ownKeysB64` is not carried, and neither is one the core
 * refuses. Row order. Pure over the core.
 */
export function carriedProfilesFromRow(
  wasm: Pick<Wasm, "composeProfileEnvelope"> & Partial<AvatarWasm>,
  row: Pick<ProfileRow, "profiles"> | null,
  ownKeysB64: readonly string[],
): CarriedProfile[] {
  const own = new Set(ownKeysB64);
  const out: CarriedProfile[] = [];
  for (const [personKeyB64, stored] of row?.profiles ?? []) {
    if (own.has(personKeyB64)) continue;
    const p = forStateBundle(stored);
    if (p === null) continue;
    try {
      const avatar =
        p.avatar === undefined || p.avatar === null || wasm.AvatarRef === undefined
          ? undefined
          : avatarToWasm<never>(wasm as unknown as AvatarWasm, p.avatar);
      const envelope = wasm.composeProfileEnvelope(
        p.displayName ?? undefined,
        p.username ?? undefined,
        p.updatedAtMs,
        p.accountIdHex === null ? undefined : hexToBytes(p.accountIdHex),
        p.usernameCertificateB64 === undefined || p.usernameCertificateB64 === null
          ? undefined
          : base64Decode(p.usernameCertificateB64),
        undefined,
        avatar,
        // ADR-0092 section 7 — the person's own "my card may be shared".
        p.cardShareable !== false,
      );
      out.push({ personKeyB64, envelopeB64: base64Encode(envelope) });
    } catch {
      /* a stored profile the core refuses is not carried */
    }
  }
  return out;
}

/**
 * Sending device: the names and pending contacts of `accountIdB64` for the
 * bundle. `ownKeysB64` are this account's own keys (the account key, its
 * predecessor, the device key); no profile filed under one is carried.
 * Best-effort per part: a store that cannot be read contributes nothing.
 */
export async function collectCarriedExtras(args: {
  readonly wasm: Wasm;
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly knownGroupIdsB64: readonly string[];
  readonly ownKeysB64: readonly string[];
}): Promise<CarriedExtras> {
  const { wasm } = args;
  const known = new Set(args.knownGroupIdsB64);

  const groupNames: CarriedGroupName[] = [];
  try {
    const rows = await loadGroupNameRows(args.accountIdB64, args.deviceKey);
    for (const [groupIdB64, value] of rows ?? []) {
      if (!known.has(groupIdB64)) continue;
      try {
        const envelope = wasm.composeGroupNameEnvelope(value.name, value.updatedAtMs);
        groupNames.push({ groupIdB64, envelopeB64: base64Encode(envelope) });
      } catch {
        /* a stored name the core refuses is not carried */
      }
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("state bundle: group names not read", err instanceof Error ? err.name : "");
  }

  let profiles: CarriedProfile[] = [];
  try {
    const row = await new IdbProfileBackend(async () => args.deviceKey, () => new Map()).load(args.accountIdB64);
    profiles = carriedProfilesFromRow(wasm, row, args.ownKeysB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("state bundle: profiles not read", err instanceof Error ? err.name : "");
  }

  const pendingContacts: CarriedPendingContact[] = [];
  try {
    const list = await readPendingContacts(args.accountIdB64);
    let logHeld: readonly PersistedHeldSend[] = [];
    if (list.length > 0) {
      try {
        logHeld = (await loadMessageLog(args.accountIdB64, args.deviceKey))?.held ?? [];
      } catch {
        logHeld = [];
      }
    }
    for (const c of list) {
      pendingContacts.push({
        accountIdHex: c.accountIdHex,
        signaturePubkeyB64: c.signaturePubkeyB64,
        x25519PubkeyB64: c.x25519PubkeyB64,
        displayName: c.displayName,
        username: c.username,
        addedAtMs: c.addedAtMs,
        linkIdB64: base64Encode(hexToBytes(c.linkIdHex)),
        heldMessages: heldFor(c, logHeld),
      });
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("state bundle: pending contacts not read", err instanceof Error ? err.name : "");
  }

  // v4: the epoch seeds of every channel the account's register holds live,
  // from this session's channel state (only when it is this account's).
  const channelSeeds = new Map<string, ReadonlyMap<number, Uint8Array>>();
  const registers = channelSubscriptionPort();
  if (registers !== null && registers.accountIdB64 === args.accountIdB64 && channelsState.hydrated()) {
    const live = liveChannelSubscriptionIds();
    for (const [idHex, inner] of channelsState.subscribedSeedsByChannel()) {
      if (live.has(idHex) && inner.size > 0) channelSeeds.set(idHex, inner);
    }
  }

  return { groupNames, profiles, pendingContacts, channelSeeds };
}

/**
 * Receiving device, after `applyStateBundle` (so the accepted contacts are
 * already here): offer the names to the session stores and add the pending
 * contacts this device holds in no form, with their display-only copies.
 * Never throws; returns what it did, for the log line.
 */
export async function applyCarriedExtras(
  bundle: StateBundle,
  accountIdB64: string,
  wasm: Pick<Wasm, "decodeGroupNameEnvelope" | "decodeProfileEnvelope">,
): Promise<{ groupNames: number; profiles: number; pendingContacts: number; channelSeeds: number }> {
  // v4: the carried channel seeds wait for this account's channel state
  // (never replacing a seed held for an epoch).
  let channelSeeds = 0;
  try {
    const carried = carriedSeedsOf(bundle);
    channelSeeds = carried.size;
    if (carried.size > 0) offerCarriedChannelSeeds(accountIdB64, carried);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("state bundle: channel seeds not applied", err instanceof Error ? err.name : "");
  }
  let groupNames = 0;
  let profiles = 0;
  try {
    const names = carriedGroupNamesOf(bundle, wasm);
    // The own keys are not known here; the profile provider leaves them out
    // when it takes the profiles (`ProfileContext.tsx`).
    const people = carriedProfilesOf(bundle, wasm, () => false);
    groupNames = names.size;
    profiles = people.size;
    offerCarriedNames(accountIdB64, names, people);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("state bundle: names not applied", err instanceof Error ? err.name : "");
  }

  let pendingContacts = 0;
  if (bundle.pendingContacts.length > 0) {
    const accepted = loadAccepted(accountIdB64);
    const added: CarriedPendingContact[] = [];
    const next = await updatePendingContacts(accountIdB64, (held) => {
      const toAdd = pendingContactsToAdd(bundle.pendingContacts, accepted, held);
      if (toAdd.length === 0) return null;
      let list: PendingContact[] = [...held];
      for (const c of toAdd) {
        list = withPendingContact(list, pendingContactOf(c));
        added.push(c);
      }
      return list;
    });
    if (next !== null) {
      pendingContacts = added.length;
      for (const c of added) {
        await storeCarriedCopies(accountIdB64, pendingQueueKey(c), copiesOf(c));
      }
    }
  }
  return { groupNames, profiles, pendingContacts, channelSeeds };
}
