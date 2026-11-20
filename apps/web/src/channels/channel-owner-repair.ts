// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 2026-11-20 — an owner device whose own channel refuses its owner proof
 * (`401 owner-proof-required`) moves the channel's owner key to the key its
 * CURRENT identity derives, signed by an own predecessor's key (ADR-0074
 * section 3.5), instead of waiting for a handover that may never come.
 *
 * Seen live on an iPhone: its reseals were refused for hours. The service
 * holds one owner key per channel, set at creation and moved only by a
 * handover. They part from what a device signs with when the account rotated
 * and the handover for that channel never landed: a kept device hands over
 * once and owes nothing, a device hands over only the channels it publishes,
 * and this client signed with the lineage key until the same day. Running
 * the handover from the predecessor heals each case from any owner device.
 */
import type { Wasm } from "../wasm";
import { putChannelOwner } from "./api";

type Identity = InstanceType<Wasm["Identity"]>;

/** What a channel's repair came to. */
export type OwnerRepairOutcome = "owned" | "not-owner" | "retry";

/** `PUT /v1/channels/{id}/owner` signed by `signing` naming `naming`'s key:
 *  the HTTP status, or `null` when no answer arrived. */
export type PutOwner = (channelIdHex: string, signing: Identity, naming: Identity) => Promise<number | null>;

function sameKey(a: Identity, b: Identity): boolean {
  const x = a.signaturePublicKey;
  const y = b.signaturePublicKey;
  return x.length === y.length && x.every((v, i) => v === y[i]);
}

/**
 * Hand `channelIdHex` from each of `predecessors` (newest first) to
 * `current`: `"owned"` at the first handover that lands, or when the current
 * key turns out to own the channel already; `"retry"` when some handover got
 * no answer and none landed; `"not-owner"` otherwise (no key of this account
 * owns the channel).
 */
export async function repairChannelOwner(
  channelIdHex: string,
  predecessors: readonly Identity[],
  current: Identity,
  put: PutOwner = putChannelOwner,
): Promise<OwnerRepairOutcome> {
  let sawRetry = false;
  for (const old of predecessors) {
    if (sameKey(old, current)) continue;
    const status = await put(channelIdHex, old, current);
    if (status === 204) return "owned";
    if (status === 401 || status === 409) {
      // A handover naming the key that already owns the channel answers 204.
      const probe = await put(channelIdHex, current, current);
      if (probe === 204) return "owned";
      if (probe !== 401) sawRetry = true;
      continue;
    }
    sawRetry = true;
  }
  return sawRetry ? "retry" : "not-owner";
}

/** The channels this session already tried; one try each (a "retry" may
 *  run again). */
const tried = new Set<string>();

export function resetChannelOwnerRepairs(): void {
  tried.clear();
}

/**
 * Repair `channelIdHex` once per session when `refused` says a retry holds it
 * as refused. Calls `onOwned` when the owner key is the current one's now.
 */
export async function repairChannelOwnerIfRefused(input: {
  readonly channelIdHex: string;
  readonly refused: boolean;
  readonly predecessors: readonly Identity[];
  readonly current: Identity | null;
  readonly onOwned: () => void;
  readonly put?: PutOwner;
}): Promise<OwnerRepairOutcome | null> {
  const id = input.channelIdHex.toLowerCase();
  if (!input.refused || tried.has(id) || input.current === null) return null;
  tried.add(id);
  if (input.predecessors.length === 0) {
    // eslint-disable-next-line no-console
    console.warn("[channels] an owner proof is refused and the account has no earlier keys; nothing to hand over");
    return "not-owner";
  }
  const outcome = await repairChannelOwner(id, input.predecessors, input.current, input.put).catch(
    () => "retry" as const,
  );
  if (outcome === "owned") {
    input.onOwned();
    // eslint-disable-next-line no-console
    console.info("ADR-0074: a refused channel's owner key is now the current key's; held work runs again");
  } else if (outcome === "retry") {
    tried.delete(id);
  } else {
    // eslint-disable-next-line no-console
    console.warn("[channels] no key of this account owns a refused channel; nothing to hand over");
  }
  return outcome;
}
