// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Thin TS wrappers over the wasm pairing-payload codec (Phase 2a3b2 web
 * infrastructure; payload v2 since ADR-0056).
 *
 * Two halves:
 *
 *   * [`buildPairingQrPayload`] — fresh-device side. Build the wire bytes
 *     that are shown as a QR code OR sealed inside the device link
 *     (`device-link.ts`; the same bytes on both carriers).
 *   * [`parsePairingQrPayload`] — confirming side. Take those bytes and
 *     produce the typed `{keyPackage, rendezvousPseudonym}` pair the rest of
 *     the add-device flow consumes.
 *
 * ADR-0056: the payload names the account it is meant for by the **login
 * handle** of the username the person typed on the fresh device
 * (`loginHandle(username)`, 32 bytes), never by the account id — a fresh
 * device has no way to learn the id before a session. The confirming device
 * passes its OWN handle (from its own username) and the core refuses a
 * payload meant for another name, so a device is never enrolled into the
 * wrong account. An account with no username (a guest) has no handle and
 * cannot take part in this flow on either side.
 */
import { loadWasm } from "../wasm";
import { LOGIN_HANDLE_LEN, LoginHandleUsernameError, loginHandleFor } from "../auth/login-handle";
import { storedCardOf, type DeviceCardLike, type StoredDeviceCard } from "./device-cards";

/** Length of the rendezvous pseudonym in bytes. Pinned at 32 —
 *  matches `lakko_mls::pairing::RENDEZVOUS_PSEUDONYM_LEN` on
 *  the Rust side, the routing-pseudonym width in
 *  `wire/v1.proto`, and every other pseudonym in Lakko. */
export const RENDEZVOUS_PSEUDONYM_LEN = 32;

/** The payload was made for another account's name (ADR-0056). */
export const PAIRING_WRONG_ACCOUNT =
  "This code is for a different account. Check that the new device uses the same username as this one.";
/** The payload's version is not one this build reads. */
export const PAIRING_UNSUPPORTED =
  "This code comes from a different version of Lakko. Update Lakko on both devices and try again.";
/** A signed-in account with no username cannot add a device (no handle). */
export const PAIRING_NEEDS_USERNAME = "Set a username first to add another device.";

/** The subset of the wasm core the codec needs (injectable in tests). */
export interface PairingCore {
  encodePairingQr(
    loginHandle: Uint8Array,
    keyPackage: Uint8Array,
    rendezvousPseudonym: Uint8Array,
    deviceCardEnvelope?: Uint8Array | null,
  ): Uint8Array;
  decodePairingQr(
    bytes: Uint8Array,
    expectedLoginHandle: Uint8Array,
  ): {
    readonly keyPackage: Uint8Array;
    readonly rendezvousPseudonym: Uint8Array;
    readonly selfRemovalProof?: Uint8Array | undefined;
    /** docs/ux/second-device.md section 2.1: the new device's own card,
     *  `undefined` when absent or undecodable. Display only. */
    readonly deviceCard?: (DeviceCardLike & { free(): void }) | undefined;
  };
}

/**
 * Generate a fresh random 32-byte rendezvous pseudonym for an
 * add-device pairing session.
 *
 * Called on the joining-device side once per pairing. The
 * pseudonym names the DS-mailbox slot the scanner PUTs the
 * Welcome to. Single-use — never reused across pairings.
 */
export function generateRendezvousPseudonym(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(RENDEZVOUS_PSEUDONYM_LEN));
}

/**
 * The login handle of this account's own username, for the confirming side
 * and for a signed-in device that shows a code. A name-less account (a
 * guest) throws {@link PAIRING_NEEDS_USERNAME}; a name the username rule
 * refuses throws the rule's copy.
 */
export async function ownPairingHandle(username: string): Promise<Uint8Array> {
  if (username.trim().length === 0) throw new Error(PAIRING_NEEDS_USERNAME);
  return (await loginHandleFor(username)).bytes;
}

/** `true` when `err` is the no-username refusal of {@link ownPairingHandle}
 *  or a name the rule refuses. */
export function isMissingUsername(err: unknown): boolean {
  return (
    (err instanceof Error && err.message === PAIRING_NEEDS_USERNAME) ||
    err instanceof LoginHandleUsernameError
  );
}

/** [`buildPairingQrPayload`] over an explicit core. */
export function buildPairingQrPayloadWith(
  core: PairingCore,
  loginHandle: Uint8Array,
  keyPackage: Uint8Array,
  rendezvousPseudonym: Uint8Array,
  deviceCardEnvelope: Uint8Array | null = null,
): Uint8Array {
  if (loginHandle.length !== LOGIN_HANDLE_LEN) {
    throw new Error(`loginHandle must be ${LOGIN_HANDLE_LEN} bytes (got ${loginHandle.length})`);
  }
  if (rendezvousPseudonym.length !== RENDEZVOUS_PSEUDONYM_LEN) {
    throw new Error(
      `rendezvousPseudonym must be ${RENDEZVOUS_PSEUDONYM_LEN} bytes ` +
        `(got ${rendezvousPseudonym.length})`,
    );
  }
  return deviceCardEnvelope === null
    ? core.encodePairingQr(loginHandle, keyPackage, rendezvousPseudonym)
    : core.encodePairingQr(loginHandle, keyPackage, rendezvousPseudonym, deviceCardEnvelope);
}

/**
 * Build the wire-format pairing payload.
 *
 * `loginHandle` is the 32-byte `loginHandle(username)` of the account the
 * fresh device wants to join. `keyPackage` is the TLS-encoded `KeyPackage`
 * from the joining device's DeviceIdentity (ADR-0007). `rendezvousPseudonym`
 * is the 32-byte slot from [`generateRendezvousPseudonym`].
 * `deviceCardEnvelope` (docs/ux/second-device.md section 2.1) is this
 * device's own device-card envelope, so the signed-in device can name it;
 * `null` builds a payload without one.
 *
 * Throws on wrong-length inputs — caller bugs surface synchronously, not
 * deep inside the QR-render call.
 */
export async function buildPairingQrPayload(
  loginHandle: Uint8Array,
  keyPackage: Uint8Array,
  rendezvousPseudonym: Uint8Array,
  deviceCardEnvelope: Uint8Array | null = null,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return buildPairingQrPayloadWith(wasm, loginHandle, keyPackage, rendezvousPseudonym, deviceCardEnvelope);
}

/** Result of [`parsePairingQrPayload`]. */
export interface DecodedPairingQr {
  /** TLS-encoded `KeyPackage` from the joining device's
   *  DeviceIdentity. Feed to `DeviceGroup.addDeviceFromWire`. */
  readonly keyPackage: Uint8Array;
  /** 32-byte rendezvous pseudonym. The DS-mailbox slot the
   *  scanner PUTs the resulting Welcome to. */
  readonly rendezvousPseudonym: Uint8Array;
  /** ADR-0081 amendment — on a DeviceGroup rejoin knock, the rejoining
   *  device's self-removal proof (137 bytes, not verified here), else
   *  `null`. */
  readonly selfRemovalProof: Uint8Array | null;
  /** docs/ux/second-device.md section 2.1: what the new device says about
   *  itself, or `null`. Nothing checks it; it names the device on the
   *  confirm step and its row until its own card arrives. */
  readonly deviceCard: StoredDeviceCard | null;
}

/** The screen copy for a codec refusal: the core's messages are stable
 *  (`lakko_mls::pairing::decode`), so they are mapped here once. Anything
 *  else is returned unchanged. */
export function pairingDecodeError(err: unknown): Error {
  const message = err instanceof Error ? err.message : String(err);
  if (message.includes("login_handle mismatch")) return new Error(PAIRING_WRONG_ACCOUNT);
  if (message.includes("version is not supported")) return new Error(PAIRING_UNSUPPORTED);
  return err instanceof Error ? err : new Error(message);
}

/** [`parsePairingQrPayload`] over an explicit core. */
export function parsePairingQrPayloadWith(
  core: PairingCore,
  wireBytes: Uint8Array,
  expectedLoginHandle: Uint8Array,
): DecodedPairingQr {
  if (expectedLoginHandle.length !== LOGIN_HANDLE_LEN) {
    throw new Error(
      `expectedLoginHandle must be ${LOGIN_HANDLE_LEN} bytes (got ${expectedLoginHandle.length})`,
    );
  }
  let decoded;
  try {
    decoded = core.decodePairingQr(wireBytes, expectedLoginHandle);
  } catch (err) {
    throw pairingDecodeError(err);
  }
  return {
    keyPackage: decoded.keyPackage,
    rendezvousPseudonym: decoded.rendezvousPseudonym,
    selfRemovalProof: decoded.selfRemovalProof ?? null,
    deviceCard: cardOfDecoded(decoded.deviceCard),
  };
}

/** The decoded payload's card as this client keeps it; the wasm handle is
 *  freed. A card this client cannot read gives `null`, never an error. */
function cardOfDecoded(card: (DeviceCardLike & { free(): void }) | undefined): StoredDeviceCard | null {
  if (card === undefined) return null;
  try {
    return storedCardOf(card);
  } catch {
    return null;
  } finally {
    try {
      card.free();
    } catch {
      // Already freed.
    }
  }
}

/**
 * ADR-0081 amendment — a DeviceGroup rejoin knock: the pairing payload plus
 * this device's self-removal proof (signed with its old leaf key, which is
 * its device identity's key) for `keyPackage`.
 */
export async function buildRejoinKnockPayload(
  loginHandle: Uint8Array,
  keyPackage: Uint8Array,
  rendezvousPseudonym: Uint8Array,
  selfRemovalProof: Uint8Array,
): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.encodeRejoinKnock(loginHandle, keyPackage, rendezvousPseudonym, selfRemovalProof);
}

/**
 * Parse a pairing payload and verify it is meant for this account's name.
 * `expectedLoginHandle` is the confirming device's OWN handle. Throws:
 *
 *   - {@link PAIRING_WRONG_ACCOUNT} when the payload names another account;
 *   - {@link PAIRING_UNSUPPORTED} for any payload version other than 2;
 *   - the core's error for malformed bytes or a wrong-length handle.
 */
export async function parsePairingQrPayload(
  wireBytes: Uint8Array,
  expectedLoginHandle: Uint8Array,
): Promise<DecodedPairingQr> {
  const wasm = await loadWasm();
  return parsePairingQrPayloadWith(wasm, wireBytes, expectedLoginHandle);
}
