// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every publishing account's reader key, known to the channel's other
 * publishers (`docs/ux/group-and-channel-privacy.md` section 15.1; live walk
 * 2026-11-24, "Night walk": web owner, iOS publisher).
 *
 * The device that holds the PublisherGroup registers its account's reader
 * key at the channels service as a subscriber row (`account-reader-key.ts`).
 * The owner saw a co-publisher's such row as "From an invitation" with
 * `Remove` and counted it, and the publisher saw the owner's the same way.
 * Each publishing device now sends its account's reader key on the
 * PublisherGroup (`lakko-contacts::publisher_reader_key`) once it is
 * registered, and again when the group grows; every publishing device keeps
 * the newest per current publisher account (sender resolved at receipt,
 * C-68) and leaves those keys out of the subscriber list and count.
 */
import { base64Encode } from "../auth/account-id";
import { DsClient } from "../ds/client";
import { channelsActions } from "../state/channels-state";
import { loadWasm, type Wasm } from "../wasm";
import { channelPublishers } from "./channel-publishers";
import { channelIdFromHex, type WasmIdentity } from "./composer";
import { persistThenPublish } from "./publisher-app-messages";
import { withPublisherGroup } from "./publisher-group-registry";

/** The receiver's rule, pure: kept only for this channel, from a sender that
 *  is a current publisher account. The account key and the reader key to
 *  keep, or `null`. */
export function publisherReaderKeyVerdict(input: {
  readonly channelIdHex: string;
  readonly envelopeChannelHex: string;
  readonly readerKeyHex: string;
  readonly senderB64: string | undefined;
  readonly publisherAccountsB64: readonly string[];
}): { readonly accountKeyB64: string; readonly readerKeyHex: string } | null {
  if (input.envelopeChannelHex.toLowerCase() !== input.channelIdHex.toLowerCase()) return null;
  if (input.senderB64 === undefined || !input.publisherAccountsB64.includes(input.senderB64)) return null;
  if (!/^[0-9a-f]{64}$/i.test(input.readerKeyHex)) return null;
  return { accountKeyB64: input.senderB64, readerKeyHex: input.readerKeyHex.toLowerCase() };
}

/** The reader keys to leave out, pure: those of accounts that publish now.
 *  Before the publisher list is known (empty), every key kept counts. */
export function currentPublisherReaderKeys(
  stored: Readonly<Record<string, string>> | undefined,
  publisherAccountsB64: readonly string[],
): readonly string[] {
  if (stored === undefined) return [];
  const all = publisherAccountsB64.length === 0;
  return [
    ...new Set(
      Object.entries(stored)
        .filter(([account]) => all || publisherAccountsB64.includes(account))
        .map(([, key]) => key.toLowerCase()),
    ),
  ];
}

/** Reactive: the other publishers' reader keys of `idHex` this device holds,
 *  as `currentPublisherReaderKeys` keeps them. */
export function otherPublisherReaderKeys(idHex: string): readonly string[] {
  return currentPublisherReaderKeys(
    channelsActions.accessOf(idHex).publisherReaderKeys,
    channelPublishers(idHex).map((p) => p.accountKeyB64),
  );
}

/** When to send this device's reader key, pure: once registered, again when
 *  the key or the number of PublisherGroup leaves changed since the last
 *  send (growth brings a device that never read the earlier envelope). */
export function readerKeySendDue(input: {
  readonly registeredHex: string | null;
  readonly leaves: number;
  readonly lastSent: string | undefined;
}): string | null {
  if (input.registeredHex === null) return null;
  const sig = `${input.registeredHex.toLowerCase()}|${input.leaves}`;
  return sig === input.lastSent ? null : sig;
}

const sent = new Map<string, string>();

/** Forget what was sent (a new session). */
export function resetPublisherReaderKeySends(): void {
  sent.clear();
}

/** Send this account's reader key on the PublisherGroup when due. Persisted
 *  before published. Never throws. */
export async function sendPublisherReaderKey(identity: WasmIdentity, idHex: string): Promise<void> {
  const registeredHex = channelsActions.accessOf(idHex).readerKeyRegisteredHex ?? null;
  if (registeredHex === null) return;
  try {
    const wasm = await loadWasm();
    await withPublisherGroup(identity, idHex, async (group, device) => {
      if (!group.isActive()) return;
      let leaves = 0;
      try {
        const members = group.members() as Array<{ free?: () => void }>;
        leaves = members.length;
        for (const m of members) m.free?.();
      } catch {
        return;
      }
      const due = readerKeySendDue({ registeredHex, leaves, lastSent: sent.get(idHex) });
      if (due === null) return;
      const envelope = wasm.composePublisherReaderKeyEnvelope(channelIdFromHex(idHex), hexToBytes(registeredHex));
      const message = group.createApplicationMessage(device, envelope);
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => new DsClient().publishGroup(group.groupId(), message).then(() => undefined),
      );
      sent.set(idHex, due);
    });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] reader key not sent to the publishers; tried again next pass", err instanceof Error ? err.name : "");
  }
}

/**
 * Read one PublisherGroup application message as a publisher reader key;
 * `true` when it was one (kept or dropped).
 */
export async function takePublisherReaderKey(
  wasm: Wasm,
  idHex: string,
  plaintext: Uint8Array,
  sender: Uint8Array | undefined,
  publisherAccountsB64: readonly string[],
): Promise<boolean> {
  if (plaintext.length !== wasm.publisherReaderKeyEnvelopeLen() || plaintext[0] !== 0x6c) return false;
  let decoded: ReturnType<Wasm["decodePublisherReaderKeyEnvelope"]>;
  try {
    decoded = wasm.decodePublisherReaderKeyEnvelope(plaintext);
  } catch {
    return false;
  }
  let kept: ReturnType<typeof publisherReaderKeyVerdict>;
  try {
    kept = publisherReaderKeyVerdict({
      channelIdHex: idHex,
      envelopeChannelHex: hexOf(decoded.channelId),
      readerKeyHex: hexOf(decoded.readerKey),
      senderB64: sender === undefined ? undefined : base64Encode(sender),
      publisherAccountsB64,
    });
  } finally {
    decoded.free();
  }
  if (kept === null) return true;
  const k = kept;
  if (channelsActions.accessOf(idHex).publisherReaderKeys?.[k.accountKeyB64] === k.readerKeyHex) return true;
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    publisherReaderKeys: { ...(r.publisherReaderKeys ?? {}), [k.accountKeyB64]: k.readerKeyHex },
  }));
  return true;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
