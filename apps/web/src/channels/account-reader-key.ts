// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account's one subscriber key on the publishing device (2026-11-13,
 * follow-up decision 1).
 *
 * The channel-subscription register carries one subscriber key per account
 * and channel, also for an owner or a co-publisher. Its other devices read the
 * channel with it, so it must be a real reader key at the channels service:
 * the device that holds the PublisherGroup registers it there as a subscriber
 * (once; also for a channel whose register carried a key the service did not
 * know yet), hands it the current seed, and every rotation it makes reseals
 * the new seed to it (`rotation-cron.ts` `withOwnReaderKey`), on a private
 * channel too, whatever the admission mark says.
 *
 * On a private channel the service wants an invite proof and a sealed
 * admission ticket for a new row. This device mints a one-use invite key of
 * its own for it and keeps it registered (it never expires), so the ticket
 * stays valid and the let-in pass of the device holding the mark admits the
 * row like any invited reader's, rather than removing it.
 */
import { loadWasm } from "../wasm";
import { channelsActions, channelsState } from "../state/channels-state";
import { putInviteKey, subscribeWithAccess } from "./api";
import { channelVisibilityOf } from "./channel-access";
import { newestAdmissionSeed } from "./channel-access-record";
import { liveRegisterSecret } from "./channel-subscription-register";
import { bytesToHex, channelIdFromHex, type WasmIdentity } from "./composer";
import { admitSubscriber } from "./subscriber-admission";
import { b64ToBytes } from "../profile/avatar-model";
import { holdAllows, PublisherCommitRetry, publisherCommitFailureLabel, type PublisherCommitHold } from "./publisher-commit-outbox";

/** One attempt per channel per this long after a failure (chosen). */
/** The holds after a failure (2026-11-20): a refusal (`401
 *  owner-proof-required`) waits for the next start, anything else 30 s,
 *  doubling to 10 minutes (`publisher-commit-outbox`). */
const holds = new PublisherCommitRetry();

/** Forget the holds (a new session). */
export function resetAccountReaderKeyHolds(): void {
  holds.reset();
}

/** Whether the registration of `idHex` was refused (a refused owner proof). */
export function accountReaderKeyRefused(idHex: string): boolean {
  return holds.hold(idHex)?.refused === true;
}

/** Drop the hold of `idHex` (its owner key was repaired). */
export function clearAccountReaderKeyHold(idHex: string): void {
  holds.noteSuccess(idHex);
}

/** The hex public key of the account's live register key, or `null`. */
export async function accountReaderKeyHex(idHex: string): Promise<string | null> {
  const secret = liveRegisterSecret(idHex);
  if (secret === null) return null;
  const wasm = await loadWasm();
  let kp: InstanceType<typeof wasm.ChannelSubscriberKeypair> | null = null;
  try {
    kp = new wasm.ChannelSubscriberKeypair(secret);
    return bytesToHex(kp.public());
  } catch {
    return null;
  } finally {
    kp?.free();
  }
}

/** What has to happen for the account's reader key on this device. Pure. */
export function readerKeyStep(input: {
  readonly publishing: boolean;
  readonly keyHex: string | null;
  readonly registeredHex: string | null;
  readonly hold: PublisherCommitHold | undefined;
  readonly nowMs: number;
}): "register" | "nothing" {
  if (!input.publishing || input.keyHex === null) return "nothing";
  if (input.registeredHex === input.keyHex) return "nothing";
  if (input.hold !== undefined && !holdAllows(input.hold, input.nowMs)) return "nothing";
  return "register";
}

/**
 * Register the account's reader key for a channel this device publishes in,
 * once, and hand it the current seed. Best effort: a failure is retried by a
 * later publisher pass (after its hold). Never throws.
 */
export async function ensureAccountReaderKey(identity: WasmIdentity, idHex: string): Promise<void> {
  const keyHex = await accountReaderKeyHex(idHex);
  const step = readerKeyStep({
    publishing: channelsState.keysByChannel().has(idHex),
    keyHex,
    registeredHex: channelsActions.accessOf(idHex).readerKeyRegisteredHex ?? null,
    hold: holds.hold(idHex),
    nowMs: Date.now(),
  });
  if (step !== "register" || keyHex === null) return;
  try {
    await registerReaderKey(identity, idHex, keyHex);
    holds.noteSuccess(idHex);
  } catch (err) {
    const hold = holds.noteFailure(idHex, err);
    // eslint-disable-next-line no-console
    console.warn(
      `[channels] the account's reader key was not registered (${publisherCommitFailureLabel(err)}); ${
        hold.refused ? "not tried again until the next start" : "tried again after a wait"
      }`,
    );
    return;
  }
  // Written with the channel log's next flush. Lost (a crash before it),
  // the next pass registers again: a row already there takes no invite use.
  channelsActions.updateAccess(idHex, (r) => ({ ...r, readerKeyRegisteredHex: keyHex }));
  // The current seed now, not at the next rotation. A failure is the
  // admission pass's and the next rotation's to cover.
  await admitSubscriber(idHex, keyHex, identity).catch(() => undefined);
}

async function registerReaderKey(identity: WasmIdentity, idHex: string, keyHex: string): Promise<void> {
  if (channelVisibilityOf(idHex) !== "private") {
    await subscribeWithAccess(idHex, keyHex, {});
    return;
  }
  const adm = newestAdmissionSeed(channelsActions.accessOf(idHex));
  if (adm === null) throw new Error("no admission seed on this device");
  const wasm = await loadWasm();
  const inviteSeed = wasm.generateChannelInviteSeed();
  const signer = wasm.ChannelInviteSigner.fromInviteSeed(inviteSeed);
  const channelId = channelIdFromHex(idHex);
  const subscriber = hexBytes(keyHex);
  const admSeed = b64ToBytes(adm.seedB64);
  try {
    // One use, never expiring: the row's ticket names it for good.
    await putInviteKey(idHex, bytesToHex(signer.verifyingKey), 0, 1, identity);
    const ticket = signer.sealTicket(wasm.channelAdmissionPublicKey(admSeed), adm.version, channelId, subscriber);
    await subscribeWithAccess(idHex, keyHex, {
      sealedTicketB64Url: b64Url(ticket),
      inviteProof: () => ({
        headerName: wasm.channelInviteProofHeaderName(),
        value: signer.signProof(channelId, subscriber),
      }),
    });
  } finally {
    admSeed.fill(0);
    inviteSeed.fill(0);
    signer.free();
  }
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function b64Url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
