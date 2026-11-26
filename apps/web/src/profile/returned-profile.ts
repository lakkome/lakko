// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 8 - the profile comes back after a key reset.
 *
 * A peer that returns a conversation or a channel to a reset person hands
 * back, verbatim, the newest profile envelope it holds for them (profile v6
 * is signed by the person's account identity key, so nobody can make one up
 * in their name). The reset device keeps one only when it verifies under its
 * own PREVIOUS account key (`auth/reset-continuity.ts`); the newest stamp
 * wins; it adopts the name and the photo references and republishes them
 * under its new key, everywhere, as after a restore. The `@username` never
 * left (it belongs to the account and was typed at sign-in), so it is not
 * taken from the envelope.
 *
 * Two carriers reach this module:
 *   - a channel return's `profile` field (`channels/channel-return-runtime.ts`);
 *   - a profile envelope posted into a returned conversation by the peer
 *     right after its Welcome (`chat/continuity-runtime.ts` port
 *     `carryProfile`), which the receive path cannot file under the sender
 *     (the signature is the person's own, not the sender's) and offers here.
 */
import { base64Decode } from "../auth/account-id";
import { readResetContinuity } from "../auth/reset-continuity";
import { loadWasm } from "../wasm";
import { avatarFromWasm, type AvatarData } from "./avatar-model";
import { announceOwnStateRestored, ownProfileChanged, ownProfileHeld, receiveOwnProfile } from "./own-profile-sync";
import { profileVerifiesUnder, routeReceivedProfile } from "./profile-signer";

/** A returned profile that verified. */
export interface VerifiedReturnedProfile {
  readonly displayName: string | null;
  readonly updatedAtMs: bigint;
  readonly avatar: AvatarData | null;
}

/**
 * Whether a verified returned profile is adopted over what is held: strictly
 * newer than the held stamp only (the same rule a device-to-device own
 * profile follows). Pure.
 */
export function adoptsReturnedProfile(candidate: VerifiedReturnedProfile, heldStamp: bigint): boolean {
  return candidate.updatedAtMs > heldStamp && candidate.updatedAtMs > 0n;
}

/** Verify `envelope` under `previousKeyB64`; `null` when it does not. */
export async function verifyReturnedProfile(
  envelope: Uint8Array,
  previousKeyB64: string,
): Promise<VerifiedReturnedProfile | null> {
  const wasm = await loadWasm();
  let profile;
  try {
    profile = wasm.verifyProfileEnvelope(envelope, base64Decode(previousKeyB64));
  } catch {
    return null;
  }
  try {
    const name = profile.displayName;
    return {
      displayName: name === undefined || name === null || name.length === 0 ? null : name,
      updatedAtMs: profile.updatedAtMs,
      avatar: avatarFromWasm(profile.avatar),
    };
  } finally {
    profile.free();
  }
}

/**
 * Offer one returned profile envelope for the signed-in account. Adopted
 * only when this device recorded a previous key for the reset and the
 * envelope verifies under it and is newer; then sent to the account's other
 * devices and republished in every conversation. Never throws.
 */
export async function offerReturnedOwnProfile(accountIdB64: string, envelope: Uint8Array): Promise<boolean> {
  try {
    const previousKeyB64 = readResetContinuity(globalThis.localStorage, accountIdB64)?.previousKeyB64 ?? null;
    if (previousKeyB64 === null) return false;
    const verified = await verifyReturnedProfile(envelope, previousKeyB64);
    const held = ownProfileHeld();
    if (verified === null || held === null || !adoptsReturnedProfile(verified, held.stamp)) return false;
    if (!receiveOwnProfile(verified.displayName, verified.updatedAtMs, verified.avatar)) return false;
    ownProfileChanged();
    announceOwnStateRestored();
    return true;
  } catch {
    return false;
  }
}

/**
 * A profile received in a conversation, routed by its signer
 * (`profile-signer.ts`): filed under the sender through `ingest` when the
 * sender signed it, offered as this account's own when its previous key
 * did, dropped otherwise. Never throws.
 */
export async function fileReceivedProfile(args: {
  readonly accountIdB64: string;
  readonly senderKeyB64: string;
  readonly profile: { readonly envelope?: Uint8Array };
  readonly ingest: () => void;
}): Promise<void> {
  const env = args.profile.envelope;
  if (env === undefined) return;
  try {
    const wasm = await loadWasm();
    const verify = (e: Uint8Array, k: Uint8Array): { free(): void } => wasm.verifyProfileEnvelope(e, k);
    const previousOwnKeyB64 = readResetContinuity(globalThis.localStorage, args.accountIdB64)?.previousKeyB64 ?? null;
    const route = routeReceivedProfile({
      verifiesUnderSender: profileVerifiesUnder(verify, env, args.senderKeyB64, base64Decode),
      verifiesUnderPreviousOwnKey: profileVerifiesUnder(verify, env, previousOwnKeyB64, base64Decode),
    });
    if (route === "file") args.ingest();
    else if (route === "returned-own") await offerReturnedOwnProfile(args.accountIdB64, env);
  } catch {
    /* dropped */
  }
}

/**
 * The newest envelope held under any of a person's keys (a reset person's
 * retired keys, newest stamp wins; a tie keeps the first key's). `null`
 * when none is held. Pure.
 */
export function newestHeldEnvelope(
  keysB64: readonly string[],
  heldEnvelopeOf: (keyB64: string) => { readonly envelope: Uint8Array; readonly updatedAtMs: bigint } | null,
): Uint8Array | null {
  let best: { readonly envelope: Uint8Array; readonly updatedAtMs: bigint } | null = null;
  for (const k of keysB64) {
    const held = heldEnvelopeOf(k);
    if (held !== null && (best === null || held.updatedAtMs > best.updatedAtMs)) best = held;
  }
  return best?.envelope ?? null;
}

function carriedKey(accountIdB64: string): string {
  return `lakko:continuity-profile-carried:${accountIdB64}`;
}

/** Whether the profile was already carried into `successorGroupIdB64`. */
export function profileCarriedInto(accountIdB64: string, successorGroupIdB64: string): boolean {
  try {
    const raw = globalThis.localStorage?.getItem(carriedKey(accountIdB64)) ?? null;
    return raw !== null && (JSON.parse(raw) as unknown[]).includes(successorGroupIdB64);
  } catch {
    return false;
  }
}

/** Record that it was. */
export function markProfileCarried(accountIdB64: string, successorGroupIdB64: string): void {
  try {
    const raw = globalThis.localStorage?.getItem(carriedKey(accountIdB64)) ?? null;
    const held = raw === null ? [] : (JSON.parse(raw) as string[]);
    if (!held.includes(successorGroupIdB64)) held.push(successorGroupIdB64);
    globalThis.localStorage?.setItem(carriedKey(accountIdB64), JSON.stringify(held.slice(-512)));
  } catch {
    /* carried again: the receiver keeps only a strictly newer stamp */
  }
}
