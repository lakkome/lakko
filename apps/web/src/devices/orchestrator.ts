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
import { announceDeviceGroupChangedHere } from "./sync/growth";
import { resendServiceEnvelopes } from "../reach/service-envelope-outbox";
import { loadWasm } from "../wasm";
import { KeyedError } from "../l10n/keyed-error";
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
import {
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import {
  buildPairingQrPayload,
  generateRendezvousPseudonym,
  parsePairingQrPayload,
} from "./pairing";
import { DsClient } from "../ds/client";
import { encodeJournalEntry } from "./journal-envelope";
import {
  ensureDeviceGroup,
  persistDeviceGroupSnapshot,
  registerDeviceGroupEpochOrThrow,
} from "./lifecycle";
import { takeSigner } from "../ds/access";
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
  /** The 16-byte entropy of the 12-word recovery phrase, transferred
   *  from the existing device (recovery payload v2). The caller OWNS this
   *  buffer: rebuild the phrase from it, derive the 64-byte
   *  AccountIdentity seed, verify that seed fail-closed against the
   *  published bundle, and only then store the seed and the phrase;
   *  zeroize on any refusal. */
  readonly phraseEntropy: Uint8Array;
  /** Fresh session token from redeeming the device grant (ACCESS
   *  half), or `null` when the redeem failed (grant expired /
   *  already consumed / auth service unreachable) — the entropy is
   *  still usable on a path that already holds its own session. */
  readonly sessionToken: string | null;
  /** Expiry of `sessionToken` (unix ms), `null` iff it is. */
  readonly sessionExpiresAtMs: number | null;
  /** The 16-byte account id (ADR-0056). A fresh device learns it here,
   *  from the grant redeem, and nowhere earlier; on a path that already
   *  held a session it is that session's id. `null` when the redeem failed
   *  on a path that knew no id. */
  readonly accountIdBytes: Uint8Array | null;
}

/** The joined DeviceGroup belongs to another account than the grant
 *  (ADR-0056): refused before anything is stored. Thrown as a
 *  {@link KeyedError} so the screen renders the key in the current language
 *  (ADR-0061 section 6); this constant is the English rendering, kept for
 *  logs and tests. */
export const JOINED_WRONG_ACCOUNT_KEY = "device_link.error.wrong_account";
export const JOINED_WRONG_ACCOUNT =
  "The other device answered for a different account. Nothing was changed on this device.";

/** The exporter half of a joined group (`Group` or `DeviceGroup`). */
interface PairingExporter {
  exportSecret(identity: WasmIdentity, label: string, context: Uint8Array, length: number): Uint8Array;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
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
  /** ADR-0056 — the 32-byte login handle of this account's username; the
   *  payload names the account by it, never by the id. */
  readonly loginHandle: Uint8Array;
}): Promise<JoinSession> {
  const wasm = await loadWasm();
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  let identity: WasmIdentity | null = wasm.Identity.fromSeed(seed);
  // Best-effort scrub.
  seed.fill(0);

  const keyPackage = identity.createKeyPackage();
  const rendezvous = generateRendezvousPseudonym();
  const qrPayload = await buildPairingQrPayload(
    args.loginHandle,
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
              const applied = applyStateBundle(bundle, args.accountIdB64);
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
                exporter: liveGroup,
                identity: liveIdentity,
                rendezvous,
                welcomeCursor: opts.welcomeCursor,
                expectedAccountIdBytes: args.accountIdBytes,
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
 * both derive the same key), parse `grant ‖ phrase_entropy`, and redeem the
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
 *
 * ADR-0056: the redeem carries the grant alone and answers with the account
 * id. When the caller already knows its account (`expectedAccountIdBytes`),
 * a redeem answering for another account is treated as a failed redeem: the
 * session is dropped and the known id is kept.
 */
export async function runJoinerRecovery(args: {
  readonly exporter: PairingExporter;
  readonly identity: WasmIdentity;
  readonly rendezvous: Uint8Array;
  readonly welcomeCursor: number;
  readonly expectedAccountIdBytes: Uint8Array | null;
  /** Injectable for tests. */
  readonly redeem?: (grantTokenB64: string) => Promise<{
    readonly accountId: Uint8Array;
    readonly sessionToken: string;
    readonly sessionExpiresAtMs: number;
  }>;
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
    key = args.exporter.exportSecret(
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
    let accountIdBytes: Uint8Array | null = args.expectedAccountIdBytes;
    try {
      const redeem =
        args.redeem ?? ((grant: string) => new AuthClient().deviceAuthorizeRedeem(grant));
      const redeemed = await redeem(base64UrlEncodeNoPad(parsed.grantToken));
      if (
        args.expectedAccountIdBytes !== null &&
        !bytesEqual(redeemed.accountId, args.expectedAccountIdBytes)
      ) {
        // eslint-disable-next-line no-console
        console.warn("device grant redeem answered for another account; session dropped");
      } else {
        sessionToken = redeemed.sessionToken;
        sessionExpiresAtMs = redeemed.sessionExpiresAtMs;
        accountIdBytes = redeemed.accountId;
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device grant redeem failed", err);
    }
    parsed.grantToken.fill(0);
    return {
      phraseEntropy: parsed.phraseEntropy,
      sessionToken,
      sessionExpiresAtMs,
      accountIdBytes,
    };
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
 * the auth service (anonymous by design, limited server-side).
 *
 * Because no device root key exists yet, the DeviceIdentity seed is
 * generated IN MEMORY here and persisted later via
 * [`AnonymousJoinSession.persistTo`] — right after the caller
 * derives + stores the device key from the recovered phrase seed.
 *
 * ADR-0056: the payload names the account by the login handle of the
 * username the person typed; nothing before the session carries the
 * account id, and the fresh device may not know it at all. So `accept`
 * joins the MLS group first (`Group.joinFromWelcome`, which needs no
 * account), opens the recovery row under the group's exporter, redeems the
 * grant, and learns the account id from the redeem response. Only then is
 * the Welcome bound to the account: the joined group's id must equal
 * `deviceGroupIdFor(account id)` (the check `DeviceGroup.joinFromWelcome`
 * performed up front before), else the whole outcome is refused and nothing
 * is persisted. `known` (a path that already holds a session) keeps the
 * redeem honest: an answer for another account drops the redeemed session.
 */
export async function prepareAnonymousJoinFlow(args: {
  /** 32-byte login handle of the typed username (ADR-0056). */
  readonly loginHandle: Uint8Array;
  /** The account, when this device already knows it (a signed-in device
   *  parked without its identity). Absent on the no-password entry. */
  readonly known?: { readonly accountIdBytes: Uint8Array; readonly accountIdB64: string };
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
    args.loginHandle,
    keyPackage,
    rendezvous,
  );

  let group: WasmDeviceGroup | null = null;
  // The account the joined group was bound to, set by a successful accept.
  let account: { readonly bytes: Uint8Array; readonly b64: string } | null = null;
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
      const liveIdentity = identity;
      // No account id yet: join the plain MLS group, bind it below.
      const joined = wasm.Group.joinFromWelcome(welcomeBytes, liveIdentity);
      let recovery: JoinRecoveryOutcome | null;
      try {
        recovery = await runJoinerRecovery({
          exporter: joined,
          identity: liveIdentity,
          rendezvous,
          welcomeCursor: opts.welcomeCursor,
          expectedAccountIdBytes: args.known?.accountIdBytes ?? null,
        });
        // No row, or no account learned: the restore cannot land (the
        // caller refuses either case); nothing is bound or persisted.
        if (recovery === null || recovery.accountIdBytes === null) {
          return { recovery };
        }
        const accountIdBytes = recovery.accountIdBytes;
        if (!bytesEqual(wasm.deviceGroupIdFor(accountIdBytes), joined.groupId)) {
          recovery.phraseEntropy.fill(0);
          throw new KeyedError(JOINED_WRONG_ACCOUNT_KEY);
        }
        const loaded = wasm.loadDeviceGroup(liveIdentity, accountIdBytes);
        if (loaded === undefined) {
          recovery.phraseEntropy.fill(0);
          throw new Error("internal: the joined device group did not load");
        }
        group = loaded;
        account = {
          bytes: accountIdBytes,
          b64: args.known?.accountIdB64 ?? base64Encode(accountIdBytes),
        };
      } finally {
        joined.free();
      }
      const liveGroup = group;
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
      await Promise.all([historyTask, stateTask]);
      return { recovery };
    },
    async persistTo(deviceKey): Promise<void> {
      if (identity === null || deviceIdentitySeed === null) {
        throw new Error("join session already disposed");
      }
      if (account === null) {
        throw new Error("internal: persistTo before a bound accept");
      }
      await putDeviceIdentitySeed(
        account.b64,
        deviceIdentitySeed,
        deviceKey,
      );
      await persistDeviceGroupSnapshot(
        identity,
        account.b64,
        deviceKey,
      );
      deviceIdentitySeed.fill(0);
      deviceIdentitySeed = null;
      if (pendingHistory !== null) {
        const history = pendingHistory;
        pendingHistory = null;
        try {
          const applied = await applyHistoryBundleToLog({
            accountIdB64: account.b64,
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
        const applied = applyStateBundle(pendingState, account.b64);
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
  /** Whether the recovery row (session grant + recovery-phrase entropy)
   *  was published for the new device. `false` when this device does not
   *  hold the phrase or the best-effort publish failed: the new device is
   *  then added to the account's devices but still has to restore its keys
   *  another way, and the screen says so. */
  readonly recoveryPublished: boolean;
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
 *   - The payload names another account (ADR-0056: its login handle is not
 *     this account's own) — `PAIRING_WRONG_ACCOUNT` copy.
 *   - The payload's version is not one this build reads —
 *     `PAIRING_UNSUPPORTED` copy.
 *   - The Welcome PUT fails (DS-mailbox unreachable).
 *   - The DeviceGroup state on this device is corrupt.
 *
 * UI surfaces the thrown message in a one-line toast.
 */
export async function runScanFlow(args: {
  readonly scannedBytes: Uint8Array;
  /** ADR-0056 — this account's OWN login handle (from its own username,
   *  `ownPairingHandle`); the payload is refused unless it names it. */
  readonly loginHandle: Uint8Array;
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
  /** ADR-0018 factor (a) — when present, the scanner also mints a
   *  device-authorization grant and publishes the sealed
   *  `grant ‖ phrase_entropy` recovery row (v2) after the Welcome, so
   *  the fresh device lands with ACCESS + IDENTITY, holds the recovery
   *  phrase like this device does, and never types a password or
   *  phrase. Strictly best-effort: any failure logs + continues (the
   *  fresh device falls back to its other routes). Absent when this
   *  device does not hold the phrase (`recovery-sender.ts`): no row is
   *  published then, and `ScanResult.recoveryPublished` is `false`.
   *
   *  `phraseEntropy` (16 bytes) is CONSUMED: this function zeroizes it
   *  after the publish attempt. */
  readonly recovery?: {
    readonly phraseEntropy: Uint8Array;
    /** Mint a fresh grant via `POST /device/authorize/start`
     *  (bearer) and return its URL-safe-base64 raw token —
     *  typically `(t) => new AuthClient().deviceAuthorizeStart(t)`
     *  closed over the live session token. */
    readonly deviceAuthorizeStart: () => Promise<string>;
  };
}): Promise<ScanResult> {
  const decoded = await parsePairingQrPayload(
    args.scannedBytes,
    args.loginHandle,
  );

  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  const ensured = await ensureDeviceGroup(
    args.accountIdBytes,
    args.accountIdB64,
    seed,
    args.deviceKey,
  );
  seed.fill(0);

  // ADR-0053 — the DeviceGroup journal key of the epoch the add leaves, taken
  // BEFORE the create call. Freed with the ephemeral group below.
  const signerN = takeSigner(() => ensured.group.dsAccessSigner(ensured.identity));
  try {
    // ADR-0058 section 5.1 — who was a member before this add, for the
    // growth re-send the sync runtime runs once the add is on disk.
    let priorLeaves: number[] = [];
    try {
      priorLeaves = ensured.group
        .devices()
        .map((m: { leafIndex: number }) => m.leafIndex)
        .sort((a: number, b: number) => a - b);
    } catch {
      priorLeaves = [];
    }
    const addition = ensured.group.addDeviceFromWire(
      ensured.identity,
      decoded.keyPackage,
    );

    // ADR-0053 contract point 3 — register the new epoch's key, proven under
    // the old one, BEFORE the advanced snapshot is persisted or anything is
    // published. A refusal throws out of here with nothing written: the
    // ephemeral group is freed and the durable DeviceGroup stays at N.
    const ds = new DsClient();
    await registerDeviceGroupEpochOrThrow(ds, ensured.group, ensured.identity, signerN);

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

    // The sync runtime holds a long-lived handle that is now an epoch
    // behind. Tell it BEFORE the commit is published, so it stops before it
    // can see the commit and write its stale state over this one, reloads,
    // and - if this device is the lowest prior leaf - re-sends the account's
    // synced state to the new device (ADR-0058 section 5.1).
    if (priorLeaves.length > 0) {
      try {
        announceDeviceGroupChangedHere({
          growth: { priorLeaves, ownLeaf: ensured.group.ownLeafIndex() },
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("DeviceGroup growth announce failed", err);
      }
    }

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
    // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped; an
    // add-device commit carries no attestation. ADR-0053: proven under the
    // key of the epoch the commit leaves, which the other devices still hold.
    await ds.publishGroup(
      ensured.group.groupId(),
      encodeJournalEntry(addition.commit, null),
      { signer: signerN },
    );

    // ADR-0060 section 7 - the setting and the key versions, for the device
    // just admitted, now that the commit is on the journal. Sent by the sync
    // runtime through its own handle, reloaded from the snapshot persisted
    // above: sealing them here, on this second handle of the same
    // DeviceGroup, reused the sync handle's sender generations (seen live
    // 2026-09-30, SecretReuseError on the new device).
    resendServiceEnvelopes();

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
    // single-use device grant (bearer), derive the
    // per-pairing exporter key at the POST-ADD epoch (the joiner
    // ratchets to the same epoch from the Welcome, so it derives
    // identical bytes), seal `grant(32) ‖ phrase_entropy(16)`, and
    // publish the tagged row to the SAME rendezvous mailbox. A
    // failure anywhere along this chain must NOT break the pairing
    // itself — log + continue; the fresh device then falls back to
    // its other routes (a passkey, the phrase).
    let recoveryPublished = false;
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
          args.recovery.phraseEntropy,
        );
        const sealed = wasm.sealWithKey(key, payload);
        await publishRecoveryPayload(decoded.rendezvousPseudonym, sealed);
        recoveryPublished = true;
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("recovery payload publish failed", err);
      } finally {
        // Zeroize every local copy of secret material, success or
        // not. The caller handed over the entropy for this call alone
        // (documented contract), so consuming it here is safe.
        grant?.fill(0);
        key?.fill(0);
        payload?.fill(0);
        args.recovery.phraseEntropy.fill(0);
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
        const bundle = buildStateBundle(snapshot?.knownGroupIds ?? [], args.accountIdB64);
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
      recoveryPublished,
    };
  } finally {
    signerN?.free();
    ensured.group.free();
    ensured.identity.free();
  }
}
