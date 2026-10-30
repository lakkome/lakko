// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 (amended 2026-10-29) - the grant a network search found a
 * non-contact through, kept when this account adds them to a group from the
 * Add members screen (`docs/ux/conversation-info.md` section 2.4).
 *
 * Kept as a grant this device holds for the person (`found` in
 * `received-grants.ts`), durably, through the same store every received grant
 * goes through, and only when its mailbox binding verifies against the
 * person's account key (or a proven key of their chain). From then on the
 * ordinary held-grant paths use it: the committer delegations are signed with
 * it, re-sent with every re-request of the add, and a sweep this device runs
 * writes the Welcome with it.
 *
 * A person found in Everyone hands out no grant, only the mailbox binding of
 * their public record: {@link openCommitterEnvelope} builds the committer
 * envelope with NO delegation that carries it.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { journalHex } from "../ds/access";
import { resolveAccountKeyHex } from "../contacts/address-resolver";
import { loadWasm } from "../wasm";
import { personKeysOf, verifiedTargetOfBinding } from "./mailbox-write";
import { reachClassName, type ReachClassName } from "./reach-model";
import { reachSession } from "./reach-service";
import { fileDelegations, fileFoundGrant } from "./received-grants";

/** The classes a search finds a person through: an introduction's `fof` key,
 *  and the `group` / `member` / `channel` keys a scope they share hands out.
 *  A `contact`, `link` or `reply` grant never arrives this way. */
export function keepsFoundGrantClass(cls: ReachClassName | null): boolean {
  return cls === "fof" || cls === "group" || cls === "member" || cls === "channel";
}

/**
 * Keep `grantWireB64` (the 134-byte grant wire, standard base64) as found for
 * the person `personKey` names. Returns whether it is now held. Never throws:
 * a grant that does not decode, is of another class, or whose binding does not
 * verify is not kept (logged without identifiers).
 */
export async function keepFoundGrant(personKey: Uint8Array, grantWireB64: string): Promise<boolean> {
  const session = reachSession();
  if (session === null) return false;
  try {
    const wasm = await loadWasm();
    const grant = wasm.decodeReachGrant(base64Decode(grantWireB64));
    let version: number;
    let verified = false;
    try {
      if (!keepsFoundGrantClass(reachClassName(grant.class as number))) {
        // eslint-disable-next-line no-console
        console.warn("ADR-0060: a found grant was not kept; its class is not one a search finds");
        return false;
      }
      version = grant.version;
      for (const key of personKeysOf(personKey)) {
        try {
          grant.mailboxPkVerified(key);
          verified = true;
          break;
        } catch {
          /* not this key */
        }
      }
    } finally {
      grant.free();
    }
    if (!verified) {
      // eslint-disable-next-line no-console
      console.warn("ADR-0078: a found grant was not kept; its mailbox binding did not verify");
      return false;
    }
    // ADR-0074 section 3.5a rule 2 - filed for the PERSON, under the key they
    // are addressed by now.
    const personHex = resolveAccountKeyHex(journalHex(personKey));
    await session.updateReceivedGrants((g) =>
      fileFoundGrant(g, personHex, { wireB64: grantWireB64, version, receivedAtMs: Date.now() }),
    );
    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ADR-0060: a found grant was not kept", err instanceof Error ? err.name : "");
    return false;
  }
}

/**
 * ADR-0060 section 6 - the committer-delegation envelope with NO delegation
 * for a person found in Everyone, to post in the group `scopeHex` after the
 * add proposal: it carries `openBindingB64` (their 97-byte mailbox binding,
 * standard base64), verified here under the added account key (and its
 * proven chain) and by the core's envelope check. The envelope is also
 * recorded as this device's own, for this group and person (a device never
 * receives its own group messages), so a sweep run here delivers the Welcome
 * the same way a committer that received it does.
 *
 * With `openBindingB64` `null`, the envelope recorded earlier for this person
 * and group is returned again (a re-request of the add re-sends it), or
 * `null` when there is none. `null` too when the binding does not verify
 * (nothing recorded). Throws only for a store failure.
 */
export async function openCommitterEnvelope(
  scopeHex: string,
  addedAccountKey: Uint8Array,
  openBindingB64: string | null,
): Promise<Uint8Array | null> {
  const session = reachSession();
  if (session === null) return null;
  const addedHex = journalHex(addedAccountKey);
  let bytes: Uint8Array;
  if (openBindingB64 === null) {
    const recorded = session.receivedGrants().delegations[`${session.accountKeyHex}|${addedHex}`];
    if (recorded === undefined || recorded.scopeHex !== scopeHex) return null;
    bytes = base64Decode(recorded.envelopeB64);
  } else {
    let binding: Uint8Array;
    try {
      binding = base64Decode(openBindingB64);
    } catch {
      return null;
    }
    const target = await verifiedTargetOfBinding(binding, personKeysOf(addedAccountKey));
    if (target === null) {
      // eslint-disable-next-line no-console
      console.warn("ADR-0078: no committer envelope posted; the found mailbox binding did not verify");
      return null;
    }
    const wasm = await loadWasm();
    let env;
    try {
      env = new wasm.CommitterDelegationEnvelope(addedAccountKey, binding, []);
    } catch {
      return null;
    }
    try {
      // The core's own check: the binding verifies under the added key.
      wasm.checkCommitterDelegations(env).forEach((d) => d.free());
      bytes = wasm.composeCommitterDelegationEnvelope(env);
    } catch {
      return null;
    } finally {
      env.free();
    }
  }
  const envelopeB64 = base64Encode(bytes);
  await session.updateReceivedGrants((g) =>
    fileDelegations(g, session.accountKeyHex, addedHex, { envelopeB64, receivedAtMs: Date.now(), scopeHex }),
  );
  return bytes;
}
