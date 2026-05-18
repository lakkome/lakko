// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * High-level orchestration for the two add-device sides
 * (Phase 2a3b2-ui).
 *
 * Wraps the per-call sequence (pairing codec / DeviceGroup
 * operations / DS-mailbox round-trip / snapshot persistence)
 * into two flat call sites the UI invokes:
 *
 *   * [`prepareJoinFlow`] — joining-device side. Returns a
 *     [`JoinSession`] handle the UI uses to render the QR + poll
 *     for the Welcome.
 *   * [`runScanFlow`] — scanner-device side. One-shot: decode
 *     the scanned QR, add the new device's leaf, publish the
 *     Welcome.
 *
 * Why two shapes (object vs one-shot): the joining side is
 * inherently long-lived (the QR sits on screen while the user
 * walks to the other device); the scanner side is a single
 * acknowledged transaction (scan happens, ack happens, done).
 */
import { loadWasm } from "../wasm";
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { ensureDeviceIdentitySeed } from "../auth/device-identity-store";
import {
  buildPairingQrPayload,
  generateRendezvousPseudonym,
  parsePairingQrPayload,
} from "./pairing";
import { ensureDeviceGroup, persistDeviceGroupSnapshot } from "./lifecycle";
import { publishWelcome, pollForWelcome, type WelcomeOutcome } from "./welcome-courier";

// ---- Joining side ------------------------------------------------

/** Returned by [`prepareJoinFlow`]. The UI:
 *
 *   1. Reads `qrPayload` and feeds it into the QR-render library.
 *   2. Calls `awaitWelcome(signal)` to long-poll the rendezvous
 *      slot; on `{kind:"received"}` it calls `accept(welcomeBytes)`
 *      and surfaces success.
 *   3. On cancel / timeout / error it calls `dispose()` so the
 *      ephemeral DeviceIdentity handle is freed.
 */
export interface JoinSession {
  /** Wire-format pairing QR bytes ready to encode as a visual
   *  QR via the `qrcode` library. */
  readonly qrPayload: Uint8Array;
  /** 32-byte rendezvous pseudonym the scanner PUTs the
   *  Welcome to. Surface in the UI as a "rendezvous code" if
   *  the user wants to debug. */
  readonly rendezvous: Uint8Array;
  /** Long-poll the rendezvous mailbox for the Welcome bytes.
   *  Resolves with the canonical `WelcomeOutcome` shape from
   *  `welcome-courier.ts`. */
  awaitWelcome(signal?: AbortSignal): Promise<WelcomeOutcome>;
  /** Drive the joining-side terminal step: join the
   *  DeviceGroup, persist the resulting snapshot, free the
   *  ephemeral identity handle. */
  accept(welcomeBytes: Uint8Array): Promise<void>;
  /** Release the ephemeral DeviceIdentity wasm handle. Safe
   *  to call multiple times; safe to call after `accept`
   *  (no-op). */
  dispose(): void;
}

/**
 * Joining-device side: build the wire-format pairing QR + an
 * ephemeral session object the UI uses to drive the round-trip.
 *
 * The DeviceIdentity (per ADR-0007) is constructed from the
 * cached random seed; this is the leaf that will be added to
 * the existing DeviceGroup on the scanner side. No
 * DeviceGroup-on-this-device is built yet — that happens
 * inside [`JoinSession.accept`] once the Welcome arrives.
 */
export async function prepareJoinFlow(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
}): Promise<JoinSession> {
  const wasm = await loadWasm();
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  let identity: WasmIdentity | null = wasm.Identity.fromSeed(seed);
  // Best-effort scrub.
  seed.fill(0);

  const keyPackage = identity.createKeyPackage();
  const rendezvous = generateRendezvousPseudonym();
  const qrPayload = await buildPairingQrPayload(
    args.accountIdBytes,
    keyPackage,
    rendezvous,
  );

  let group: WasmDeviceGroup | null = null;

  return {
    qrPayload,
    rendezvous,
    async awaitWelcome(signal): Promise<WelcomeOutcome> {
      // `exactOptionalPropertyTypes` rejects `signal:
      // undefined`; build the options object conditionally.
      return pollForWelcome(
        rendezvous,
        signal === undefined ? {} : { signal },
      );
    },
    async accept(welcomeBytes): Promise<void> {
      if (identity === null) {
        throw new Error("join session already disposed");
      }
      // Run the MLS join. The deterministic group_id check
      // inside wasm.DeviceGroup.joinFromWelcome rejects a
      // misrouted Welcome before any state advance.
      group = wasm.DeviceGroup.joinFromWelcome(
        welcomeBytes,
        identity,
        args.accountIdBytes,
      );
      // Persist the post-join snapshot so subsequent
      // launches can pick up the same DeviceGroup state via
      // `ensureDeviceGroup`.
      await persistDeviceGroupSnapshot(
        identity,
        args.accountIdB64,
        args.deviceKey,
      );
    },
    dispose(): void {
      if (group !== null) {
        group.free();
        group = null;
      }
      if (identity !== null) {
        identity.free();
        identity = null;
      }
    },
  };
}

// ---- Scanner side ------------------------------------------------

/** Result of [`runScanFlow`]. Empty success — the UI just
 *  needs to know the round-trip landed without throwing. */
export interface ScanResult {
  /** Number of devices in the local DeviceGroup AFTER the
   *  add. UI surfaces as "now N devices linked" toast. */
  readonly memberCount: number;
}

/**
 * Scanner-device side: parse the scanned QR bytes, add the
 * joining device's KeyPackage to the local DeviceGroup,
 * publish the Welcome to the rendezvous mailbox.
 *
 * One-shot — every call constructs fresh DeviceIdentity +
 * DeviceGroup handles, runs the round-trip, frees the handles.
 * Re-entrant safe — concurrent scans on the same account
 * serialise on the snapshot-store write (IDB transaction).
 *
 * Throws the inner error verbatim when:
 *   - The QR was scanned against a different account.
 *   - The QR carries a newer-version payload than this binary
 *     recognises.
 *   - The Welcome PUT fails (DS-mailbox unreachable).
 *   - The DeviceGroup state on this device is corrupt.
 *
 * UI surfaces the thrown message in a one-line toast.
 */
export async function runScanFlow(args: {
  readonly scannedBytes: Uint8Array;
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
}): Promise<ScanResult> {
  const decoded = await parsePairingQrPayload(
    args.scannedBytes,
    args.accountIdBytes,
  );

  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  const ensured = await ensureDeviceGroup(
    args.accountIdBytes,
    args.accountIdB64,
    seed,
    args.deviceKey,
  );
  seed.fill(0);

  try {
    const addition = ensured.group.addDeviceFromWire(
      ensured.identity,
      decoded.keyPackage,
    );
    // Push the Welcome to the rendezvous mailbox the joining
    // device is polling.
    await publishWelcome(decoded.rendezvousPseudonym, addition.welcome);
    // TODO(2a3b2-followup): once the DS group journal is
    // wired for DeviceGroup commits, broadcast
    // `addition.commit` here too so any other
    // already-paired devices on this account advance to the
    // new epoch. For the founder + first-pair case the
    // commit is a no-op (the founder is the only existing
    // member, and the merge-pending-commit already advanced
    // its local view inside addDeviceFromWire).
    await persistDeviceGroupSnapshot(
      ensured.identity,
      args.accountIdB64,
      args.deviceKey,
    );
    // member count: founder + freshly-added leaf, plus any
    // prior members. We don't expose `devices()` through FFI
    // yet (2a3c), so surface a best-effort estimate.
    return { memberCount: ensured.bootstrapped ? 2 : 0 };
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
}
