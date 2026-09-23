// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What `New call` starts (`docs/ux/call-links.md` sections 1 and 2).
 *
 * One kind of thing: a group video call, founded by whoever pressed the
 * button, who lands in it. There is no form, no media choice and no
 * participant picker in front of it, and none of the four questions the old
 * create-link form asked survives — a person already in the contact list is
 * called from their conversation or their contact row, which is why this
 * button does not try to be that route.
 *
 * Nothing here mints a capability. A call is founded by its owner
 * ([ADR-0032](../../../../docs/adr/0032-multi-joiner-call-links.md):
 * owner-founds, and the capability's embedded one-time KeyPackage goes
 * unused), so `startLinkCall` needs only the call's id and the owner's own
 * identity. The link is minted later, from inside the call, by whoever wants
 * one — see `in-call-invite.ts`.
 *
 * Kept out of the `.tsx` because vitest here runs in plain node with no Solid
 * transform: anything inside a component cannot be tested.
 */
import { paths } from "../routes/paths";
import { CALL_ID_LEN } from "./invite";

/** Length of an `account_id` (UUIDv7), in bytes. */
export const ACCOUNT_ID_LEN = 16;

/** Length of an Ed25519 public key, in bytes. */
export const SIGNATURE_KEY_LEN = 32;

/**
 * Where "return to the call" leads while the call is still connecting.
 *
 * A call started here has no `/c/<slug>` — nothing has been minted, and
 * section 3 is explicit that minting is deferred to the first `Invite`. So the
 * return path is the destination the person pressed the button on. Once the
 * call is live the in-call overlay covers every screen anyway
 * (`CallHostView`), so this is only ever followed during the seconds before
 * that, and it lands the person exactly where they were.
 *
 * Deliberately not `paths.call(...)`: that route seeds a mock call for the
 * design surface, so sending a live call there would show a fake one.
 */
export const NEW_CALL_RETURN_PATH: string = paths.calls;

/** The local account, as the owner of a call it is founding. */
export interface NewCallOwner {
  /** The 16-byte `account_id` of the signed-in account. */
  readonly accountId: Uint8Array;
  /** The MLS leaf signature key of the local identity — the same Ed25519
   *  public key a capability minted from this identity's seed carries as its
   *  `owner_signature_pubkey`, so a link minted later names this founder. */
  readonly signaturePublicKey: Uint8Array;
}

/**
 * The arguments `CallHostApi.startLinkCall` takes for a call this device
 * founds. Structurally the host's `StartLinkCallOptions`; restated here so
 * this module stays free of the Solid context it feeds.
 */
export interface NewCallStart {
  readonly callId: Uint8Array;
  readonly ownerAccountId: Uint8Array;
  readonly ownerSignatureKey: Uint8Array;
  readonly mediaVideo: boolean;
  readonly isOwner: boolean;
  readonly routePath: string;
}

/** Raised when the caller hands over an identity of the wrong shape. */
export class NewCallArgError extends Error {
  override name = "NewCallArgError";
}

/**
 * A fresh `call_id`: 16 random bytes.
 *
 * `random` is injectable so the decision can be tested without a Web Crypto
 * environment; production passes nothing and gets `crypto.getRandomValues`.
 */
export function newCallId(random?: (out: Uint8Array) => void): Uint8Array {
  const callId = new Uint8Array(CALL_ID_LEN);
  if (random !== undefined) {
    random(callId);
    return callId;
  }
  if (typeof globalThis.crypto?.getRandomValues !== "function") {
    throw new NewCallArgError("This browser has no secure random source.");
  }
  globalThis.crypto.getRandomValues(callId);
  return callId;
}

/**
 * The call `New call` starts: group, video, owned and hosted here.
 *
 * `mediaVideo` is `true` unconditionally — section 1: "a voice call is a video
 * call with the camera off, which the in-call controls already do, so offering
 * the choice up front only asks a person to predict something they can change
 * in one tap".
 */
export function newCallStart(owner: NewCallOwner, callId: Uint8Array): NewCallStart {
  if (callId.length !== CALL_ID_LEN) {
    throw new NewCallArgError(`callId must be ${CALL_ID_LEN} bytes, got ${callId.length}`);
  }
  if (owner.accountId.length !== ACCOUNT_ID_LEN) {
    throw new NewCallArgError(
      `accountId must be ${ACCOUNT_ID_LEN} bytes, got ${owner.accountId.length}`,
    );
  }
  if (owner.signaturePublicKey.length !== SIGNATURE_KEY_LEN) {
    throw new NewCallArgError(
      `signaturePublicKey must be ${SIGNATURE_KEY_LEN} bytes, got ${owner.signaturePublicKey.length}`,
    );
  }
  return {
    callId,
    ownerAccountId: owner.accountId,
    ownerSignatureKey: owner.signaturePublicKey,
    mediaVideo: true,
    isOwner: true,
    routePath: NEW_CALL_RETURN_PATH,
  };
}
