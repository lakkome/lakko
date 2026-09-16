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
import {
  DEVICE_IDENTITY_SEED_LENGTH,
  ensureDeviceIdentitySeed,
  putDeviceIdentitySeed,
} from "../auth/device-identity-store";
import { AuthClient } from "../auth/client";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import {
  buildPairingQrPayload,
  generateRendezvousPseudonym,
  parsePairingQrPayload,
} from "./pairing";
import { DsClient } from "../ds/client";
import { encodeJournalEntry } from "./journal-envelope";
import { ensureDeviceGroup, persistDeviceGroupSnapshot } from "./lifecycle";
import {
  publishWelcome,
  pollForWelcome,
  publishHistoryBundle,
  pollForHistoryBundle,
  type WelcomeOutcome,
} from "./welcome-courier";
import {
  buildHistoryBundleFromLog,
  applyHistoryBundleToLog,
} from "./history-bundle";
import {
  RECOVERY_EXPORTER_KEY_LENGTH,
  RECOVERY_EXPORTER_LABEL,
  buildRecoveryPayload,
  parseRecoveryPayload,
  pollForRecoveryPayload,
  publishRecoveryPayload,
} from "./recovery-payload";
import {
  STATE_EXPORTER_KEY_LENGTH,
  STATE_EXPORTER_LABEL,
  applyStateBundle,
  buildStateBundle,
  decodeStateBundle,
  encodeStateBundle,
  pollForStateBundle,
  publishStateBundle,
  type StateBundleV1,
} from "./state-bundle";
import { loadMlsSnapshot } from "../chat/mls-store";

// ---- Joining side ------------------------------------------------

/** ADR-0018 factor (a) — what the joiner recovered from the
 *  scanner-published recovery row (if any). Surfaced from
 *  [`JoinSession.accept`] / [`AnonymousJoinSession.accept`]. */
export interface JoinRecoveryOutcome {
  /** The 64-byte AccountIdentity (BIP39 phrase) seed transferred
   *  from the existing device (ADR-0017 IDENTITY half). The caller
   *  OWNS this buffer: verify it fail-closed against the published
   *  bundle, cache it, and zeroize on any refusal. */
  readonly phraseSeed: Uint8Array;
  /** Fresh session token from redeeming the device grant (ACCESS
   *  half), or `null` when the redeem failed (grant expired /
   *  already consumed / auth service unreachable) — the seed is
   *  still usable on a path that already holds its own session. */
  readonly sessionToken: string | null;
  /** Expiry of `sessionToken` (unix ms), `null` iff it is. */
  readonly sessionExpiresAtMs: number | null;
}

/** Result of [`JoinSession.accept`]. */
export interface JoinAcceptOutcome {
  /** Recovery payload outcome — `null` when no recovery row showed
   *  up (older scanner build, best-effort scanner half failed, or
   *  the caller didn't opt in via `opts.recovery`). The join itself
   *  succeeded either way. */
  readonly recovery: JoinRecoveryOutcome | null;
}

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
   *  ephemeral identity handle.
   *
   *  Optional `opts.welcomeCursor` (the DS journal id of the
   *  consumed Welcome row) opts into the Phase 2a3d8-ui
   *  history catch-up bundle path — `accept` polls the
   *  rendezvous mailbox past that cursor, opens the bundle
   *  with the post-join DeviceGroup exporter secret, and
   *  applies the rows into the local message log. Omit to
   *  skip the catch-up step (older callers stay
   *  backward-compatible).
   *
   *  Optional `opts.recovery` (ADR-0018 factor (a)) also polls
   *  past the cursor for the scanner-published recovery row,
   *  opens it with the per-pairing exporter key, redeems the
   *  embedded grant, and surfaces the result on the returned
   *  outcome. Absence of the row is tolerated (`recovery: null`)
   *  — the join completes exactly as without the flag. */
  accept(
    welcomeBytes: Uint8Array,
    opts?: { readonly welcomeCursor: number; readonly recovery?: boolean },
  ): Promise<JoinAcceptOutcome>;
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
    async accept(welcomeBytes, opts): Promise<JoinAcceptOutcome> {
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
      let recovery: JoinRecoveryOutcome | null = null;
      if (opts !== undefined && group !== null) {
        // Phase 2a3d8-ui — poll past the Welcome cursor for the
        // scanner-side history bundle. Best-effort: a timeout or
        // a malformed bundle leaves the joiner with an empty
        // log + the normal DS-poll catch-up takes over going
        // forward.
        //
        // ADR-0018 factor (a) — when opted in, ALSO poll for the
        // recovery row. Both polls start at the Welcome cursor
        // and filter disjoint row types (the recovery row carries
        // a plaintext magic the history poll skips and vice
        // versa), so they run concurrently.
        const liveGroup = group;
        const liveIdentity = identity;
        const historyTask = (async (): Promise<void> => {
          try {
            const sealed = await pollForHistoryBundle(
              rendezvous,
              opts.welcomeCursor,
            );
            if (sealed.kind === "received") {
              const secret = liveGroup.historyBundleSecret(liveIdentity);
              const applied = await applyHistoryBundleToLog({
                accountIdB64: args.accountIdB64,
                deviceKey: args.deviceKey,
                secret,
                sealedBytes: sealed.sealedBytes,
              });
              // eslint-disable-next-line no-console
              console.info(`history catch-up: applied ${applied} rows`);
            }
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("history bundle apply failed", err);
          }
        })();
        // ADR-0050 section 2.9 — the state bundle (contacts, pins, known
        // group ids), best-effort like the history bundle.
        const stateTask = (async (): Promise<void> => {
          try {
            const bundle = await receiveStateBundle(
              liveGroup,
              liveIdentity,
              rendezvous,
              opts.welcomeCursor,
            );
            if (bundle !== null) {
              const applied = applyStateBundle(bundle);
              // eslint-disable-next-line no-console
              console.info(`state bundle: ${applied.contacts} contacts, ${applied.pins} pins`);
            }
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("state bundle apply failed", err);
          }
        })();
        const recoveryTask =
          opts.recovery === true
            ? runJoinerRecovery({
                group: liveGroup,
                identity: liveIdentity,
                rendezvous,
                welcomeCursor: opts.welcomeCursor,
                accountIdBytes: args.accountIdBytes,
              })
            : Promise.resolve<JoinRecoveryOutcome | null>(null);
        const [, , recovered] = await Promise.all([historyTask, stateTask, recoveryTask]);
        recovery = recovered;
      }
      return { recovery };
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

/** Joiner side of the ADR-0050 state bundle: poll the tagged row, open it
 *  under the per-pairing exporter key, decode. `null` when the scanner sent
 *  none within the window or the bytes did not decode. */
async function receiveStateBundle(
  group: WasmDeviceGroup,
  identity: WasmIdentity,
  rendezvous: Uint8Array,
  welcomeCursor: number,
): Promise<StateBundleV1 | null> {
  const outcome = await pollForStateBundle(rendezvous, welcomeCursor);
  if (outcome.kind !== "received") return null;
  const wasm = await loadWasm();
  let key: Uint8Array | null = null;
  try {
    key = group.exportSecret(
      identity,
      STATE_EXPORTER_LABEL,
      rendezvous,
      STATE_EXPORTER_KEY_LENGTH,
    );
    return decodeStateBundle(wasm.openWithKey(key, outcome.sealed));
  } finally {
    key?.fill(0);
  }
}

// ---- Joiner-side recovery (ADR-0018 factor (a)) -------------------

/**
 * Poll the rendezvous mailbox for the scanner-published recovery
 * row, open it with the per-pairing MLS exporter key (Path B —
 * both devices sit at the same DeviceGroup epoch post-join, so
 * both derive the same key), parse `grant ‖ seed`, and redeem the
 * grant for a session-token triple.
 *
 * Tolerant by design:
 *   - no row within the soft window → `null` (older scanner build
 *     or the best-effort scanner half failed; the join itself is
 *     unaffected and the device falls back to phrase entry);
 *   - row present but the redeem fails → the outcome still carries
 *     the seed with `sessionToken: null` (a caller that already
 *     holds its own session can use the seed alone).
 *
 * Throws nothing on its own failure paths — a malformed / wrong-key
 * row logs + returns `null` (fail-closed on identity: nothing is
 * cached here; the caller verifies the seed against the published
 * bundle before trusting it).
 */
async function runJoinerRecovery(args: {
  readonly group: WasmDeviceGroup;
  readonly identity: WasmIdentity;
  readonly rendezvous: Uint8Array;
  readonly welcomeCursor: number;
  readonly accountIdBytes: Uint8Array;
}): Promise<JoinRecoveryOutcome | null> {
  let outcome;
  try {
    outcome = await pollForRecoveryPayload(args.rendezvous, args.welcomeCursor);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("recovery payload poll failed", err);
    return null;
  }
  if (outcome.kind !== "received") return null;

  const wasm = await loadWasm();
  let key: Uint8Array | null = null;
  let plaintext: Uint8Array | null = null;
  try {
    key = args.group.exportSecret(
      args.identity,
      RECOVERY_EXPORTER_LABEL,
      args.rendezvous,
      RECOVERY_EXPORTER_KEY_LENGTH,
    );
    plaintext = wasm.openWithKey(key, outcome.sealedBlob);
    const parsed = parseRecoveryPayload(plaintext);

    // Redeem the grant for ACCESS. A failure here (expired /
    // consumed grant, auth service down) downgrades the outcome to
    // seed-only rather than discarding the IDENTITY half.
    let sessionToken: string | null = null;
    let sessionExpiresAtMs: number | null = null;
    try {
      const redeemed = await new AuthClient().deviceAuthorizeRedeem(
        args.accountIdBytes,
        base64UrlEncodeNoPad(parsed.grantToken),
      );
      sessionToken = redeemed.sessionToken;
      sessionExpiresAtMs = redeemed.sessionExpiresAtMs;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device grant redeem failed", err);
    }
    parsed.grantToken.fill(0);
    return { phraseSeed: parsed.phraseSeed, sessionToken, sessionExpiresAtMs };
  } catch (err) {
    // Wrong key / tampered blob / malformed payload — drop it.
    // eslint-disable-next-line no-console
    console.warn("recovery payload open failed", err);
    return null;
  } finally {
    key?.fill(0);
    plaintext?.fill(0);
  }
}

// ---- Anonymous joining side (ADR-0018 factor (a), path (b)) -------

/**
 * Returned by [`prepareAnonymousJoinFlow`]. Mirrors [`JoinSession`]
 * for the NO-PASSWORD restore path: the fresh device has no device
 * root key yet (nothing to wrap persistence under), so `accept`
 * skips snapshot / history persistence and ALWAYS consumes the
 * recovery row; the caller bootstraps the device key from the
 * recovered seed and then calls [`persistTo`] to land the
 * DeviceIdentity seed + DeviceGroup snapshot on disk.
 */
export interface AnonymousJoinSession {
  /** Wire-format pairing QR bytes. */
  readonly qrPayload: Uint8Array;
  /** 32-byte rendezvous pseudonym for this pairing session. */
  readonly rendezvous: Uint8Array;
  /** Long-poll the rendezvous mailbox for the Welcome bytes. */
  awaitWelcome(signal?: AbortSignal): Promise<WelcomeOutcome>;
  /** Join the DeviceGroup from the Welcome + consume the recovery
   *  row. NOTHING is persisted yet — there is no device key. */
  accept(
    welcomeBytes: Uint8Array,
    opts: { readonly welcomeCursor: number },
  ): Promise<JoinAcceptOutcome>;
  /** After the caller bootstrapped the device root key from the
   *  recovered seed: persist the in-memory DeviceIdentity seed +
   *  the post-join DeviceGroup snapshot under it, then zeroize the
   *  in-memory seed copy. Call once, after a successful `accept`. */
  persistTo(deviceKey: CryptoKey): Promise<void>;
  /** Release the wasm handles + zeroize the in-memory
   *  DeviceIdentity seed. Idempotent. */
  dispose(): void;
}

/**
 * NO-PASSWORD variant of [`prepareJoinFlow`] (ADR-0018 factor (a),
 * the full recovery cascade entry). The DS rendezvous mailbox is
 * pseudonym-keyed and auth-free, so the whole pairing round-trip
 * runs anonymously; only the grant redeem inside `accept` touches
 * the auth service (PoW-gated, anonymous by design).
 *
 * Because no device root key exists yet, the DeviceIdentity seed is
 * generated IN MEMORY here and persisted later via
 * [`AnonymousJoinSession.persistTo`] — right after the caller
 * derives + stores the device key from the recovered phrase seed.
 */
export async function prepareAnonymousJoinFlow(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
}): Promise<AnonymousJoinSession> {
  const wasm = await loadWasm();
  // Fresh random per-device seed, held in memory until `persistTo`.
  let deviceIdentitySeed: Uint8Array | null = crypto.getRandomValues(
    new Uint8Array(DEVICE_IDENTITY_SEED_LENGTH),
  );
  let identity: WasmIdentity | null = wasm.Identity.fromSeed(
    deviceIdentitySeed,
  );

  const keyPackage = identity.createKeyPackage();
  const rendezvous = generateRendezvousPseudonym();
  const qrPayload = await buildPairingQrPayload(
    args.accountIdBytes,
    keyPackage,
    rendezvous,
  );

  let group: WasmDeviceGroup | null = null;
  // ADR-0050 — fetched at accept, when the DeviceGroup handle that derives
  // the exporter keys is alive; applied at `persistTo`, when the device key
  // that wraps the message log exists. Before this the history bundle the
  // scanner published was never read on this route, and a confirmed device
  // opened every chat empty.
  let pendingHistory: { secret: Uint8Array; sealedBytes: Uint8Array } | null = null;
  let pendingState: StateBundleV1 | null = null;

  return {
    qrPayload,
    rendezvous,
    async awaitWelcome(signal): Promise<WelcomeOutcome> {
      return pollForWelcome(
        rendezvous,
        signal === undefined ? {} : { signal },
      );
    },
    async accept(welcomeBytes, opts): Promise<JoinAcceptOutcome> {
      if (identity === null) {
        throw new Error("join session already disposed");
      }
      group = wasm.DeviceGroup.joinFromWelcome(
        welcomeBytes,
        identity,
        args.accountIdBytes,
      );
      const liveGroup = group;
      const liveIdentity = identity;
      const historyTask = (async (): Promise<void> => {
        try {
          const sealed = await pollForHistoryBundle(rendezvous, opts.welcomeCursor);
          if (sealed.kind === "received") {
            pendingHistory = {
              secret: liveGroup.historyBundleSecret(liveIdentity),
              sealedBytes: sealed.sealedBytes,
            };
          }
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("history bundle fetch failed", err);
        }
      })();
      const stateTask = (async (): Promise<void> => {
        try {
          pendingState = await receiveStateBundle(
            liveGroup,
            liveIdentity,
            rendezvous,
            opts.welcomeCursor,
          );
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("state bundle fetch failed", err);
        }
      })();
      // The recovery row is what lands the session; the two bundles ride
      // alongside and are applied once there is a device key to write under.
      const [recovery] = await Promise.all([
        runJoinerRecovery({
          group,
          identity,
          rendezvous,
          welcomeCursor: opts.welcomeCursor,
          accountIdBytes: args.accountIdBytes,
        }),
        historyTask,
        stateTask,
      ]);
      return { recovery };
    },
    async persistTo(deviceKey): Promise<void> {
      if (identity === null || deviceIdentitySeed === null) {
        throw new Error("join session already disposed");
      }
      await putDeviceIdentitySeed(
        args.accountIdB64,
        deviceIdentitySeed,
        deviceKey,
      );
      await persistDeviceGroupSnapshot(
        identity,
        args.accountIdB64,
        deviceKey,
      );
      deviceIdentitySeed.fill(0);
      deviceIdentitySeed = null;
      if (pendingHistory !== null) {
        const history = pendingHistory;
        pendingHistory = null;
        try {
          const applied = await applyHistoryBundleToLog({
            accountIdB64: args.accountIdB64,
            deviceKey,
            secret: history.secret,
            sealedBytes: history.sealedBytes,
          });
          // eslint-disable-next-line no-console
          console.info(`history catch-up: applied ${applied} rows`);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("history bundle apply failed", err);
        } finally {
          history.secret.fill(0);
        }
      }
      if (pendingState !== null) {
        const applied = applyStateBundle(pendingState);
        pendingState = null;
        // eslint-disable-next-line no-console
        console.info(`state bundle: ${applied.contacts} contacts, ${applied.pins} pins`);
      }
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
      if (deviceIdentitySeed !== null) {
        deviceIdentitySeed.fill(0);
        deviceIdentitySeed = null;
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
  /** ADR-0050 section 2.8 — the added device's 32-byte signature key: its
   *  DeviceGroup leaf, its conversation leaf and its discovery `device_id`
   *  are one key. The caller hands it to `BackgroundReceiveApi.admitDevice`,
   *  which admits the device to every chat this device is in. */
  readonly newDeviceKey: Uint8Array;
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
  /** ADR-0018 factor (a) — when present, the scanner also mints a
   *  device-authorization grant and publishes the sealed
   *  `grant ‖ phrase_seed` recovery row after the Welcome, so the
   *  fresh device lands with ACCESS + IDENTITY and never types a
   *  password or phrase. Strictly best-effort: any failure logs +
   *  continues (the fresh device falls back to phrase entry).
   *
   *  `accountIdentitySeed` is CONSUMED — pass a `.slice()` of the
   *  live session seed; this function zeroizes it after the
   *  publish attempt. */
  readonly recovery?: {
    readonly accountIdentitySeed: Uint8Array;
    /** Mint a fresh grant via `POST /device/authorize/start`
     *  (bearer + PoW) and return its URL-safe-base64 raw token —
     *  typically `(t) => new AuthClient().deviceAuthorizeStart(t)`
     *  closed over the live session token. */
    readonly deviceAuthorizeStart: () => Promise<string>;
  };
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

    // G-7 — persist the advanced DeviceGroup snapshot BEFORE any
    // publish. `addDeviceFromWire` already merged the pending
    // commit, so the in-memory view is one epoch ahead; writing it
    // to disk first closes the window where a crash between the
    // publishes and the persist leaves the on-disk envelope an
    // epoch behind (which would make the next launch try to add the
    // same device twice).
    await persistDeviceGroupSnapshot(
      ensured.identity,
      args.accountIdB64,
      args.deviceKey,
    );

    // Push the Welcome to the rendezvous mailbox the joining
    // device is polling.
    await publishWelcome(decoded.rendezvousPseudonym, addition.welcome);

    // G-5 — broadcast the Commit to the DeviceGroup DS journal so
    // any OTHER already-paired devices on this account advance to
    // the new epoch; their DeviceGroup poll loop runs
    // `processIncoming` on this row (see `DeviceGroupSyncContext`).
    // MLS requires in-order epoch processing, so this is on the
    // critical path (not best-effort) — a missed commit would
    // permanently strand a third device. For the founder +
    // first-pair case there is no other member, so the row is a
    // harmless no-op (the joiner ratchets forward from the Welcome,
    // not this journal entry). Mirrors the `removeDeviceAt`
    // broadcast in `remove.ts`.
    const ds = new DsClient();
    // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped; an
    // add-device commit carries no attestation.
    await ds.publishGroup(
      ensured.group.groupId(),
      encodeJournalEntry(addition.commit, null),
    );

    // Phase 2a3d8-ui — best-effort history catch-up bundle.
    // Read the local message log, derive the post-add
    // exporter secret, seal the bundle, publish as the
    // *second* mailbox row (Welcome was the first). A
    // failure anywhere along the chain is non-fatal — the
    // joining device still gets the Welcome + ratchets
    // forward; it just lands with an empty timeline.
    try {
      const secret = ensured.group.historyBundleSecret(ensured.identity);
      const sealed = await buildHistoryBundleFromLog({
        accountIdB64: args.accountIdB64,
        deviceKey: args.deviceKey,
        secret,
      });
      if (sealed !== null) {
        await publishHistoryBundle(decoded.rendezvousPseudonym, sealed);
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("history bundle publish failed", err);
    }

    // ADR-0018 factor (a) — best-effort recovery payload. Mint a
    // single-use device grant (bearer + PoW), derive the
    // per-pairing exporter key at the POST-ADD epoch (the joiner
    // ratchets to the same epoch from the Welcome, so it derives
    // identical bytes), seal `grant(32) ‖ phrase_seed(64)`, and
    // publish the tagged row to the SAME rendezvous mailbox. A
    // failure anywhere along this chain must NOT break the pairing
    // itself — log + continue; the fresh device then falls back to
    // phrase entry exactly as before this feature existed.
    if (args.recovery !== undefined) {
      let grant: Uint8Array | null = null;
      let key: Uint8Array | null = null;
      let payload: Uint8Array | null = null;
      try {
        const wasm = await loadWasm();
        const grantB64 = await args.recovery.deviceAuthorizeStart();
        grant = base64UrlDecodeNoPad(grantB64);
        key = ensured.group.exportSecret(
          ensured.identity,
          RECOVERY_EXPORTER_LABEL,
          decoded.rendezvousPseudonym,
          RECOVERY_EXPORTER_KEY_LENGTH,
        );
        payload = buildRecoveryPayload(
          grant,
          args.recovery.accountIdentitySeed,
        );
        const sealed = wasm.sealWithKey(key, payload);
        await publishRecoveryPayload(decoded.rendezvousPseudonym, sealed);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("recovery payload publish failed", err);
      } finally {
        // Zeroize every local copy of secret material, success or
        // not. The caller passed a dedicated copy of the seed
        // (documented contract), so consuming it here is safe.
        grant?.fill(0);
        key?.fill(0);
        payload?.fill(0);
        args.recovery.accountIdentitySeed.fill(0);
      }
    }

    // ADR-0050 section 2.9 — the state bundle: contacts, pins and the ids
    // of every group this device holds, sealed under the per-pairing
    // exporter like the recovery row. Best-effort; a failure leaves the new
    // device with the chats it is admitted to and no contact names.
    {
      let key: Uint8Array | null = null;
      try {
        const wasm = await loadWasm();
        const snapshot = await loadMlsSnapshot(args.accountIdB64, args.deviceKey);
        const bundle = buildStateBundle(snapshot?.knownGroupIds ?? []);
        key = ensured.group.exportSecret(
          ensured.identity,
          STATE_EXPORTER_LABEL,
          decoded.rendezvousPseudonym,
          STATE_EXPORTER_KEY_LENGTH,
        );
        const sealed = wasm.sealWithKey(key, encodeStateBundle(bundle));
        await publishStateBundle(decoded.rendezvousPseudonym, sealed);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("state bundle publish failed", err);
      } finally {
        key?.fill(0);
      }
    }

    // member count: founder + freshly-added leaf, plus any
    // prior members. We don't expose `devices()` through FFI
    // yet (2a3c), so surface a best-effort estimate.
    const wasm = await loadWasm();
    return {
      memberCount: ensured.bootstrapped ? 2 : 0,
      newDeviceKey: wasm.keyPackageSignatureKey(decoded.keyPackage),
    };
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
}
