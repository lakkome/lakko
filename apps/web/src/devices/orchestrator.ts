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
import { resendServiceEnvelopes } from "../reach/service-envelope-outbox";
import { loadWasm, type Wasm } from "../wasm";
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
import type { SessionPair } from "../auth/session-pair";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import {
  buildPairingQrPayload,
  generateRendezvousPseudonym,
  parsePairingQrPayload,
} from "./pairing";
import { buildPairingCommit } from "./pairing-commit";
import { recordJoinedDeviceGroupId, readJoinedDeviceGroupId } from "./joined-device-group-id";
import { getDeviceGroupSnapshotState } from "./device-group-snapshot-store";
import { DsClient } from "../ds/client";
import type { DeviceCipher } from "../auth/device-cipher";
import { persistDeviceGroupSnapshot, type EnsuredDeviceGroup } from "./lifecycle";
import { commitDeviceGroupChange } from "./device-group-commit-runtime";
import { t } from "../l10n";
import {
  pollForWelcome,
  joinFirstWelcome,
  publishHistoryBundle,
  pollForHistoryBundle,
  type WelcomeOutcome,
} from "./welcome-courier";
import {
  buildHistoryBundleFromLog,
  applyHistoryBundleToLog,
} from "./history-bundle";
import {
  composeRecoveryPlaintext,
  decodeRecoveryPlaintext,
  pollForRecoveryPayload,
  publishRecoveryRow,
  zeroOpenedRecoveryPayload,
  type OpenedRecoveryPayload,
  type RecoveredPredecessor,
} from "./recovery-payload";
import { applyCarriedExtras, collectCarriedExtras } from "./carried-state-io";
import {
  STATE_EXPORTER_KEY_LENGTH,
  STATE_EXPORTER_LABEL,
  accountKeyHexOfSeed,
  applyOwnPredecessors,
  applyStateBundle,
  boundedOwnPredecessors,
  buildStateBundle,
  collectOwnPredecessors,
  decodeStateBundle,
  encodeStateBundle,
  pollForStateBundle,
  publishStateBundle,
  type CarriedOwnPredecessor,
  type StateBundle,
} from "./state-bundle";
import { loadMlsSnapshot } from "../chat/mls-store";
import {
  adoptPairedDeviceCard,
  composePairingCardEnvelope,
  pairingCardEnvelopeNow,
  sendOwnCardAfterJoin,
  startPairingLocate,
  type PairingPlace,
} from "./device-card-runtime";
import type { StoredDeviceCard } from "./device-cards";

/**
 * docs/ux/second-device.md section 2.1, "The place in the payload card": the
 * QR payload waits for the city at most the bound from the screen opening
 * and is never rebuilt once shown. The code route seals the payload after the
 * code is typed, so `atSeal` rebuilds it with the same key package and
 * rendezvous when the answer arrived after the QR payload was built, and
 * otherwise returns the QR payload itself.
 */
async function buildPairingPayloads(
  loginHandle: Uint8Array,
  keyPackage: Uint8Array,
  rendezvous: Uint8Array,
  place: PairingPlace,
  /** G-56 — the self-removal proof of a device already in the DeviceGroup
   *  that pairs again; carried by both payloads. */
  selfRemovalProof: Uint8Array | null = null,
): Promise<{ qrPayload: Uint8Array; atSeal: () => Promise<Uint8Array> }> {
  // This device's own card rides the payload so the signed-in device can
  // name it; none is no failure.
  const qrPayload = await buildPairingQrPayload(
    loginHandle,
    keyPackage,
    rendezvous,
    await composePairingCardEnvelope(place),
    selfRemovalProof,
  );
  const builtWithPlace = place.current() !== null;
  return {
    qrPayload,
    atSeal: async () => {
      if (builtWithPlace || place.current() === null) return qrPayload;
      try {
        return await buildPairingQrPayload(
          loginHandle,
          keyPackage,
          rendezvous,
          await pairingCardEnvelopeNow(place),
          selfRemovalProof,
        );
      } catch {
        return qrPayload;
      }
    },
  };
}

// ---- Joining side ------------------------------------------------

/** ADR-0018 factor (a) — what the joiner recovered from the
 *  scanner-published recovery row (if any). Surfaced from
 *  [`JoinSession.accept`] / [`AnonymousJoinSession.accept`]. */
export interface JoinRecoveryOutcome {
  /** The 16-byte entropy of the 12-word recovery phrase, transferred
   *  from the existing device (recovery payload v3). The caller OWNS this
   *  buffer: rebuild the phrase from it, derive the 64-byte
   *  AccountIdentity seed, verify that seed fail-closed against the
   *  published bundle, and only then store the seed and the phrase;
   *  zeroize on any refusal. */
  readonly phraseEntropy: Uint8Array;
  /** The account's earlier keys (own predecessors, oldest first) the
   *  confirming device holds (recovery payload v3, ADR-0074 "Joining after a
   *  rotation"). The caller OWNS the seeds: it stores them BEFORE the
   *  account seed and the landing, and zeroes them on every path. */
  readonly predecessors: readonly RecoveredPredecessor[];
  /** The fresh session pair from redeeming the device grant (ACCESS half,
   *  ADR-0073), or `null` when the redeem failed (grant expired / already
   *  consumed / auth service unreachable) — the entropy is still usable on
   *  a path that already holds its own session. */
  readonly session: SessionPair | null;
  /** The 16-byte account id (ADR-0056). A fresh device learns it here,
   *  from the grant redeem, and nowhere earlier; on a path that already
   *  held a session it is that session's id. `null` when the redeem failed
   *  on a path that knew no id. */
  readonly accountIdBytes: Uint8Array | null;
}

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
  /** The payload the code route seals after the code is typed: the QR
   *  payload, or the same payload carrying the city when its answer
   *  arrived only after the QR payload was built. Never throws. */
  pairingPayloadAtSeal(): Promise<Uint8Array>;
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
  readonly deviceKey: DeviceCipher;
  /** ADR-0056 — the 32-byte login handle of this account's username; the
   *  payload names the account by it, never by the id. */
  readonly loginHandle: Uint8Array;
  /** ADR-0074 — called after a state bundle stored the account's earlier
   *  keys on this (signed-in) device, before the rest of the bundle is
   *  applied. The session re-mints its conversation binding there
   *  (`AuthApi.rebindAfterStateBundle`). Must not throw. */
  readonly onOwnPredecessorsStored?: () => Promise<unknown>;
  /** The city of this pairing, started when the screen opened
   *  (`startPairingLocate`); one is started here when absent. */
  readonly place?: PairingPlace;
  /** G-56 — the pairing runs from the earlier-keys parked screen: when this
   *  device is already in the DeviceGroup it joined by pairing, the payload
   *  carries its self-removal proof ({@link heldLeafSelfRemovalProof}) so
   *  the confirming device replaces its leaf instead of refusing a second
   *  one with the same key. Otherwise the ordinary join. */
  readonly rejoinsHeldLeaf?: boolean;
  /** The DeviceGroup's latest epoch, by an empty DS knock (the ADR-0081
   *  rejoin probe); `null` when the journal names none. Injectable for tests. */
  readonly latestEpoch?: (groupId: Uint8Array) => Promise<bigint | null>;
}): Promise<JoinSession> {
  const wasm = await loadWasm();
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  let identity: WasmIdentity | null = wasm.Identity.fromSeed(seed);

  const keyPackage = identity.createKeyPackage();
  let selfRemovalProof: Uint8Array | null = null;
  try {
    if (args.rejoinsHeldLeaf === true) {
      selfRemovalProof = await heldLeafSelfRemovalProof({
        wasm,
        accountIdBytes: args.accountIdBytes,
        accountIdB64: args.accountIdB64,
        deviceKey: args.deviceKey,
        deviceIdentitySeed: seed,
        joiningIdentity: identity,
        keyPackage,
        latestEpoch: args.latestEpoch ?? probeLatestDeviceGroupEpoch,
      });
    }
  } finally {
    // Best-effort scrub.
    seed.fill(0);
  }
  const rendezvous = generateRendezvousPseudonym();
  const { qrPayload, atSeal } = await buildPairingPayloads(
    args.loginHandle,
    keyPackage,
    rendezvous,
    args.place ?? startPairingLocate(),
    selfRemovalProof,
  );

  let group: WasmDeviceGroup | null = null;

  return {
    qrPayload,
    pairingPayloadAtSeal: atSeal,
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
      // misrouted Welcome before any state advance. A row that does not
      // join (anyone who learned the rendezvous id can write one) is passed
      // over and the next row read.
      const joiner = identity;
      const joinedAt =
        opts === undefined
          ? {
              joined: wasm.DeviceGroup.joinFromWelcome(welcomeBytes, joiner, args.accountIdBytes),
              welcomeCursor: 0,
            }
          : await joinFirstWelcome(
              rendezvous,
              { welcome: welcomeBytes, welcomeCursor: opts.welcomeCursor },
              (bytes) => wasm.DeviceGroup.joinFromWelcome(bytes, joiner, args.accountIdBytes),
            );
      group = joinedAt.joined;
      const welcomeCursor = joinedAt.welcomeCursor;
      // Persist the post-join snapshot so subsequent
      // launches can pick up the same DeviceGroup state via
      // `ensureDeviceGroup`.
      await persistDeviceGroupSnapshot(
        identity,
        args.accountIdB64,
        args.deviceKey,
      );
      // G-56 — the DeviceGroup id this device joined, for a later pairing
      // from the earlier-keys parked screen that has to name it.
      await recordJoinedDeviceGroupId(args.accountIdB64, args.deviceKey, group.groupId());
      // docs/ux/second-device.md section 3: a device that has just joined
      // sends its own card at once.
      void sendOwnCardAfterJoin(args.accountIdB64);
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
            const secret = liveGroup.historyBundleSecret(liveIdentity);
            const sealed = await pollForHistoryBundle(rendezvous, welcomeCursor, {
              accept: historyBundleOpens(wasm, secret),
            });
            if (sealed.kind === "received") {
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
              welcomeCursor,
            );
            if (bundle !== null) {
              // ADR-0074: the own predecessors first, under the device key.
              // On the restore route this runs before the session's
              // identities exist, so the conversation identity is minted
              // under the same account key the sending device's leaves use.
              await storeOwnPredecessors(bundle, args.accountIdB64, args.deviceKey);
              // A device that was already signed in minted its conversation
              // binding before these keys arrived: the session mints it again
              // (only while it holds no conversation) and publishes again.
              if (bundle.ownPredecessors.length > 0 && args.onOwnPredecessorsStored !== undefined) {
                await args.onOwnPredecessorsStored();
              }
              const applied = applyStateBundle(bundle, args.accountIdB64);
              // v3: group names, profiles, pending contacts.
              const extras = await applyCarriedExtras(bundle, args.accountIdB64, await loadWasm());
              // eslint-disable-next-line no-console
              console.info(
                `state bundle: ${applied.contacts} contacts, ${applied.pins} pins, ` +
                  `${extras.groupNames} group names, ${extras.profiles} profiles, ` +
                  `${extras.pendingContacts} pending contacts, ${extras.channelSeeds} channels with seeds`,
              );
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
                welcomeCursor,
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

/** The DeviceGroup's latest epoch, by an empty knock (the ADR-0081 rejoin
 *  probe, one `ds-knock` token); `null` when the journal names none. */
async function probeLatestDeviceGroupEpoch(groupId: Uint8Array): Promise<bigint | null> {
  const probe = await new DsClient().knock(groupId, []);
  return probe.epoch === null ? null : BigInt(probe.epoch);
}

/**
 * G-56 — the self-removal proof a device already in the account's DeviceGroup
 * signs when it pairs again from the earlier-keys parked screen, or `null`
 * when the ordinary join applies: this device recorded no joined DeviceGroup
 * id, holds no DeviceGroup snapshot it can read, or its leaf is no longer in
 * that group's roster.
 *
 * The device keeps its device identity (no new seed), so the payload's
 * KeyPackage carries the leaf key already in the group, which an Add would
 * refuse (`DuplicateSignatureKey`). With its old leaf key (the same key) it
 * signs that the leaf may be replaced by `keyPackage` in the commit leaving
 * the DeviceGroup's latest epoch, learned by an empty knock as the ADR-0081
 * rejoin knock does (this device's own epoch when the probe gets no answer).
 * Never throws.
 */
export async function heldLeafSelfRemovalProof(args: {
  readonly wasm: Wasm;
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  /** The 32-byte device identity seed (not consumed). */
  readonly deviceIdentitySeed: Uint8Array;
  /** The joining identity (the same seed, an empty provider): the signer. */
  readonly joiningIdentity: WasmIdentity;
  readonly keyPackage: Uint8Array;
  readonly latestEpoch: (groupId: Uint8Array) => Promise<bigint | null>;
}): Promise<Uint8Array | null> {
  const { wasm } = args;
  const groupId = await readJoinedDeviceGroupId(args.accountIdB64, args.deviceKey);
  if (groupId === null) return null;
  let snapshot: Uint8Array | null = null;
  try {
    snapshot = (await getDeviceGroupSnapshotState(args.accountIdB64, args.deviceKey))?.snapshot ?? null;
  } catch {
    snapshot = null;
  }
  if (snapshot === null || snapshot.length === 0) return null;
  let heldEpoch: bigint;
  const held = wasm.Identity.fromSeed(args.deviceIdentitySeed);
  try {
    held.restoreStorage(snapshot);
    const group = wasm.loadDeviceGroupById(held, args.accountIdBytes, groupId);
    if (group === undefined) return null;
    try {
      const ownKey = held.signaturePublicKey;
      const members = group.devices() as { signaturePublicKey: Uint8Array }[];
      if (!members.some((m) => bytesEqual(m.signaturePublicKey, ownKey))) return null;
      heldEpoch = group.epoch();
    } finally {
      group.free();
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("pairing: the held DeviceGroup state could not be read; the ordinary join applies", refusalCode(err));
    return null;
  } finally {
    held.free();
  }
  let probed: bigint | null = null;
  try {
    probed = await args.latestEpoch(groupId);
  } catch {
    // eslint-disable-next-line no-console
    console.warn("pairing: the DeviceGroup epoch probe failed; this device's own epoch is used");
    probed = null;
  }
  try {
    const proof = wasm.signDeviceGroupSelfRemoval(
      args.joiningIdentity,
      groupId,
      probed ?? heldEpoch,
      args.keyPackage,
    );
    // eslint-disable-next-line no-console
    console.info("pairing: this device is in the DeviceGroup already; it pairs again with a self-removal proof");
    return proof;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("pairing: the self-removal proof was not signed; the ordinary join applies", refusalCode(err));
    return null;
  }
}

/** ADR-0074 — store a bundle's own predecessors under the device key. Best
 *  effort and never throws: a failure leaves the device as one that knows
 *  only the current key (logged without any key material). */
async function storeOwnPredecessors(
  bundle: StateBundle,
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<void> {
  if (bundle.ownPredecessors.length === 0) return;
  try {
    const wasm = await loadWasm();
    const added = await applyOwnPredecessors(bundle, accountIdB64, deviceKey, (seed) =>
      accountKeyHexOfSeed(wasm, seed),
    );
    // eslint-disable-next-line no-console
    console.info(`state bundle: ${added} earlier account keys stored`);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(
      "state bundle: the earlier account keys were not stored",
      err instanceof Error ? err.name : "unknown",
    );
  }
}

/** Whether a rendezvous row is the history bundle: it opens under the
 *  pairing's history secret. A row that does not is passed over. */
function historyBundleOpens(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  secret: Uint8Array,
): (sealedBytes: Uint8Array) => boolean {
  return (sealedBytes) => {
    try {
      wasm.openHistoryBundleStructured(secret, sealedBytes).free();
      return true;
    } catch {
      return false;
    }
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
): Promise<StateBundle | null> {
  const wasm = await loadWasm();
  let key: Uint8Array | null = null;
  try {
    key = group.exportSecret(
      identity,
      STATE_EXPORTER_LABEL,
      rendezvous,
      STATE_EXPORTER_KEY_LENGTH,
    );
    const liveKey = key;
    const open = (sealed: Uint8Array): StateBundle | null => {
      try {
        return decodeStateBundle(wasm.openWithKey(liveKey, sealed));
      } catch {
        return null;
      }
    };
    // Anyone who learned the rendezvous id can write a tagged row: one that
    // does not open and decode is passed over.
    const outcome = await pollForStateBundle(rendezvous, welcomeCursor, {
      accept: (sealed) => open(sealed) !== null,
    });
    if (outcome.kind !== "received") return null;
    return open(outcome.sealed);
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
    readonly session: SessionPair;
  }>;
}): Promise<JoinRecoveryOutcome | null> {
  const wasm = await loadWasm();
  let outcome;
  try {
    outcome = await pollForRecoveryPayload(args.rendezvous, args.welcomeCursor, {
      // Anyone who learned the rendezvous id can write a tagged row: only a
      // row that opens under this pairing's key is taken.
      accept: (row) => recoveryRowOpens(wasm, args, row),
    });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("recovery payload poll failed", err);
    return null;
  }
  if (outcome.kind !== "received") return null;

  // The core reads the row: `undefined` is not a recovery row, a throw is a
  // recovery row of another version (v2) or an empty one. Either way no keys
  // are handed over.
  let sealed: Uint8Array | undefined;
  try {
    sealed = wasm.unwrapRecoveryRow(outcome.row);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("recovery row refused", refusalCode(err));
    return null;
  }
  if (sealed === undefined) return null;
  let key: Uint8Array | null = null;
  let plaintext: Uint8Array | null = null;
  let opened: OpenedRecoveryPayload | null = null;
  try {
    key = args.exporter.exportSecret(
      args.identity,
      wasm.recoveryExporterLabel(),
      args.rendezvous,
      wasm.recoveryExporterKeyLen(),
    );
    plaintext = wasm.openWithKey(key, sealed);
    opened = decodeRecoveryPlaintext(wasm, plaintext);
    // eslint-disable-next-line no-console
    console.info(`recovery row: ${opened.predecessors.length} earlier account keys`);

    // Redeem the grant for ACCESS. A failure here (expired /
    // consumed grant, auth service down) downgrades the outcome to
    // seed-only rather than discarding the IDENTITY half.
    let session: SessionPair | null = null;
    let accountIdBytes: Uint8Array | null = args.expectedAccountIdBytes;
    try {
      const redeem =
        args.redeem ?? ((grant: string) => new AuthClient().deviceAuthorizeRedeem(grant));
      const redeemed = await redeem(base64UrlEncodeNoPad(opened.grant));
      if (
        args.expectedAccountIdBytes !== null &&
        !bytesEqual(redeemed.accountId, args.expectedAccountIdBytes)
      ) {
        // eslint-disable-next-line no-console
        console.warn("device grant redeem answered for another account; session dropped");
      } else {
        session = redeemed.session;
        accountIdBytes = redeemed.accountId;
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device grant redeem failed", err);
    }
    opened.grant.fill(0);
    const handed = opened;
    opened = null;
    return {
      phraseEntropy: handed.phraseEntropy,
      predecessors: handed.predecessors,
      session,
      accountIdBytes,
    };
  } catch (err) {
    // Wrong key / tampered blob / malformed payload — drop it.
    // eslint-disable-next-line no-console
    console.warn("recovery payload open failed", refusalCode(err));
    return null;
  } finally {
    key?.fill(0);
    plaintext?.fill(0);
    if (opened !== null) zeroOpenedRecoveryPayload(opened);
  }
}

/** Whether a tagged recovery row opens under this pairing's key and
 *  decodes; everything it opened is zeroed again. */
function recoveryRowOpens(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  args: { readonly exporter: PairingExporter; readonly identity: WasmIdentity; readonly rendezvous: Uint8Array },
  row: Uint8Array,
): boolean {
  let key: Uint8Array | null = null;
  let plaintext: Uint8Array | null = null;
  try {
    const sealed = wasm.unwrapRecoveryRow(row);
    if (sealed === undefined) return false;
    key = args.exporter.exportSecret(
      args.identity,
      wasm.recoveryExporterLabel(),
      args.rendezvous,
      wasm.recoveryExporterKeyLen(),
    );
    plaintext = wasm.openWithKey(key, sealed);
    zeroOpenedRecoveryPayload(decodeRecoveryPlaintext(wasm, plaintext));
    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("recovery row passed over", refusalCode(err));
    return false;
  } finally {
    key?.fill(0);
    plaintext?.fill(0);
  }
}

/** The code of a core refusal (`"<code>: <reason>"`), never the payload. */
function refusalCode(err: unknown): string {
  return err instanceof Error ? (err.message.split(":")[0] ?? "unknown") : "unknown";
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
  /** See [`JoinSession.pairingPayloadAtSeal`]. */
  pairingPayloadAtSeal(): Promise<Uint8Array>;
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
  persistTo(deviceKey: DeviceCipher): Promise<void>;
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
 * grant, and learns the account id from the redeem response. The group is
 * bound to the account by the recovery row itself: only a member of the
 * group the Welcome joined can seal a row this device opens under that
 * group's exporter, so the group and the phrase the row carries come from
 * one source. Since ADR-0078 section 6 the DeviceGroup id is derived from
 * the account seed, not the account id, so there is no id to compare the
 * redeemed account against; a row whose phrase is not the redeemed
 * account's is refused by the restore's check against the published
 * identity before anything is persisted. `known` (a path that already holds
 * a session) keeps the redeem honest: an answer for another account drops
 * the redeemed session.
 */
export async function prepareAnonymousJoinFlow(args: {
  /** 32-byte login handle of the typed username (ADR-0056). */
  readonly loginHandle: Uint8Array;
  /** The account, when this device already knows it (a signed-in device
   *  parked without its identity). Absent on the no-password entry. */
  readonly known?: { readonly accountIdBytes: Uint8Array; readonly accountIdB64: string };
  /** The city of this pairing, started when the screen opened
   *  (`startPairingLocate`); one is started here when absent. */
  readonly place?: PairingPlace;
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
  const { qrPayload, atSeal } = await buildPairingPayloads(
    args.loginHandle,
    keyPackage,
    rendezvous,
    args.place ?? startPairingLocate(),
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
  let pendingState: StateBundle | null = null;

  return {
    qrPayload,
    pairingPayloadAtSeal: atSeal,
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
      // No account id yet: join the DeviceGroup Welcome as a plain group and
      // bind it below. ADR-0081 amendment — through the DeviceGroup entry
      // point: `Group.joinFromWelcome` refuses a Welcome with no role table.
      // A row that does not join (anyone who learned the rendezvous id can
      // write one) is passed over and the next row read.
      const joinedAt = await joinFirstWelcome(
        rendezvous,
        { welcome: welcomeBytes, welcomeCursor: opts.welcomeCursor },
        (bytes) => wasm.DeviceGroup.joinWelcomeByName(bytes, liveIdentity),
      );
      const joined = joinedAt.joined;
      const welcomeCursor = joinedAt.welcomeCursor;
      let recovery: JoinRecoveryOutcome | null;
      try {
        recovery = await runJoinerRecovery({
          exporter: joined,
          identity: liveIdentity,
          rendezvous,
          welcomeCursor,
          expectedAccountIdBytes: args.known?.accountIdBytes ?? null,
        });
        // No row, or no account learned: the restore cannot land (the
        // caller refuses either case); nothing is bound or persisted.
        if (recovery === null || recovery.accountIdBytes === null) {
          return { recovery };
        }
        const accountIdBytes = recovery.accountIdBytes;
        const loaded = wasm.loadDeviceGroupById(liveIdentity, accountIdBytes, joined.groupId);
        if (loaded === undefined) {
          recovery.phraseEntropy.fill(0);
          throw new Error("internal: the joined device group did not load");
        }
        group = loaded;
        account = {
          bytes: accountIdBytes,
          b64: args.known?.accountIdB64 ?? base64Encode(accountIdBytes),
        };
        // docs/ux/second-device.md section 3: the card goes as soon as the
        // session this route lands binds the card runtime.
        void sendOwnCardAfterJoin(account.b64);
      } finally {
        joined.free();
      }
      const liveGroup = group;
      const historyTask = (async (): Promise<void> => {
        try {
          const secret = liveGroup.historyBundleSecret(liveIdentity);
          const sealed = await pollForHistoryBundle(rendezvous, welcomeCursor, {
            accept: historyBundleOpens(wasm, secret),
          });
          if (sealed.kind === "received") {
            pendingHistory = { secret, sealedBytes: sealed.sealedBytes };
          } else {
            secret.fill(0);
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
            welcomeCursor,
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
      // G-56 — the DeviceGroup id this device joined (by name), as on the
      // pairing join of a known account.
      if (group !== null) await recordJoinedDeviceGroupId(account.b64, deviceKey, group.groupId());
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
        // ADR-0074: before `finishCanonicalSession` installs the session's
        // identities (the caller's next step), so the conversation identity
        // is minted under the account key the sending device's leaves use.
        await storeOwnPredecessors(pendingState, account.b64, deviceKey);
        const state = pendingState;
        pendingState = null;
        const applied = applyStateBundle(state, account.b64);
        // v3: group names, profiles, pending contacts. The names wait for the
        // session stores of this account, which bind when the session lands.
        const extras = await applyCarriedExtras(state, account.b64, await loadWasm());
        // eslint-disable-next-line no-console
        console.info(
          `state bundle: ${applied.contacts} contacts, ${applied.pins} pins, ` +
            `${extras.groupNames} group names, ${extras.profiles} profiles, ` +
            `${extras.pendingContacts} pending contacts, ${extras.channelSeeds} channels with seeds`,
        );
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
  /** docs/ux/second-device.md section 2.1: the card the new device put in
   *  its payload (display only), or `null`. Already kept for its leaf. */
  readonly newDeviceCard: StoredDeviceCard | null;
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
  readonly deviceKey: DeviceCipher;
  /** This account's own keys (standard base64: the account key, its
   *  predecessor, this device's key). The state bundle never carries a
   *  profile filed under one of them. */
  readonly ownKeysB64?: readonly string[];
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
  const wasm = await loadWasm();

  // ADR-0081 decision 2 — the add goes through the DeviceGroup commit outbox:
  // the entry beside the durable snapshot, one `POST .../commit` (the next
  // epoch's registration and the commit row together), the next epoch made
  // current, the Welcome to the rendezvous the joining device polls, the
  // entry deleted. A failure after the entry was written is finished by the
  // outbox later; the joining device keeps polling meanwhile.
  const result = await commitDeviceGroupChange(
    {
      accountIdBytes: args.accountIdBytes,
      accountIdB64: args.accountIdB64,
      deviceKey: args.deviceKey,
    },
    // G-56 — the Add, or, for a device already in the DeviceGroup that pairs
    // again with its self-removal proof, the one-commit replacement of its
    // leaf (`pairing-commit.ts`); an uncovered proof throws before anything
    // is built.
    (ensured) => buildPairingCommit(wasm, ensured, decoded),
    async (ensured, outcome) => {
      if (outcome.kind === "lost") {
        // Another device of the account changed the DeviceGroup at the same
        // epoch: nothing of this add is on the journal. The person scans
        // again; the joining device is still waiting.
        throw new Error(t("devices.add.error.generic"));
      }
      return finishScan(args, decoded, ensured);
    },
  );
  if (result === null) throw new Error(t("devices.add.error.generic"));
  return result.value;
}

/**
 * The part of {@link runScanFlow} that runs once the add is on the journal
 * and current: the service envelopes, then the best-effort history,
 * recovery and state bundles, sealed under the post-add exporter on the
 * throwaway handle the commit was built on.
 */
async function finishScan(
  args: Parameters<typeof runScanFlow>[0],
  decoded: Awaited<ReturnType<typeof parsePairingQrPayload>>,
  ensured: EnsuredDeviceGroup,
): Promise<ScanResult> {
  // ADR-0060 section 7 - the setting and the key versions, for the device
  // just admitted, now that the commit is on the journal. Sent by the sync
  // runtime through its own handle, reloaded from the snapshot made current
  // above: sealing them here, on this second handle of the same DeviceGroup,
  // reused the sync handle's sender generations (seen live 2026-09-30,
  // SecretReuseError on the new device).
  resendServiceEnvelopes();

  // Phase 2a3d8-ui — best-effort history catch-up bundle.
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

  // ADR-0018 factor (a) — best-effort recovery payload, sealed under the
  // per-pairing exporter key at the POST-ADD epoch. A failure anywhere along
  // this chain must NOT break the pairing itself.
  let recoveryPublished = false;
  if (args.recovery !== undefined) {
    let grant: Uint8Array | null = null;
    let key: Uint8Array | null = null;
    let payload: Uint8Array | null = null;
    let predecessors: RecoveredPredecessor[] = [];
    try {
      const wasm = await loadWasm();
      // ADR-0074 "Joining after a rotation": the account's earlier keys ride
      // in the row, so the joining device holds the conversation key before
      // it binds anything. A store that cannot be read is not "none": no row
      // is published (the joining device sees the existing error).
      predecessors = await ownPredecessorsForRow(
        wasm,
        args.accountIdB64,
        args.deviceKey,
        args.recovery.phraseEntropy,
      );
      const grantB64 = await args.recovery.deviceAuthorizeStart();
      grant = base64UrlDecodeNoPad(grantB64);
      key = ensured.group.exportSecret(
        ensured.identity,
        wasm.recoveryExporterLabel(),
        decoded.rendezvousPseudonym,
        wasm.recoveryExporterKeyLen(),
      );
      payload = composeRecoveryPlaintext(wasm, grant, args.recovery.phraseEntropy, predecessors);
      const sealed = wasm.sealWithKey(key, payload);
      await publishRecoveryRow(decoded.rendezvousPseudonym, wasm.wrapRecoveryRow(sealed));
      recoveryPublished = true;
      // eslint-disable-next-line no-console
      console.info(`recovery row published with ${predecessors.length} earlier account keys`);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("recovery payload publish failed", refusalCode(err));
    } finally {
      grant?.fill(0);
      key?.fill(0);
      payload?.fill(0);
      for (const p of predecessors) p.seed.fill(0);
      args.recovery.phraseEntropy.fill(0);
    }
  }

  // ADR-0050 section 2.9 — the state bundle: contacts, pins and the ids of
  // every group this device holds, sealed under the per-pairing exporter like
  // the recovery row. Best-effort.
  {
    let key: Uint8Array | null = null;
    try {
      const wasm = await loadWasm();
      const snapshot = await loadMlsSnapshot(args.accountIdB64, args.deviceKey);
      let ownPredecessors: CarriedOwnPredecessor[] = [];
      try {
        ownPredecessors = await collectOwnPredecessors(args.accountIdB64, args.deviceKey, (seed) =>
          accountKeyHexOfSeed(wasm, seed),
        );
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "state bundle: the earlier account keys could not be read",
          err instanceof Error ? err.name : "unknown",
        );
      }
      const knownGroupIds = snapshot?.knownGroupIds ?? [];
      // v3: the names and pending contacts this device holds. Best-effort
      // per part, inside `collectCarriedExtras`.
      const extras = await collectCarriedExtras({
        wasm,
        accountIdB64: args.accountIdB64,
        deviceKey: args.deviceKey,
        knownGroupIdsB64: knownGroupIds,
        ownKeysB64: args.ownKeysB64 ?? [],
      });
      const bundle = buildStateBundle(knownGroupIds, args.accountIdB64, ownPredecessors, extras);
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

  const wasm = await loadWasm();
  const newDeviceKey = wasm.keyPackageSignatureKey(decoded.keyPackage);
  // docs/ux/second-device.md section 2.1: the new device's row has its name
  // at once, from the payload's card, until its own card arrives.
  if (decoded.deviceCard !== null) adoptPairedDeviceCard(hexOf(newDeviceKey), decoded.deviceCard);
  return {
    memberCount: ensured.group.devices().length,
    newDeviceKey,
    recoveryPublished,
    newDeviceCard: decoded.deviceCard,
  };
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

/**
 * The own predecessors the recovery row carries (oldest first, at most the
 * row's maximum: the oldest and the newest ones, like the state bundle), read
 * with the reader the state bundle uses. A seed equal to the current account
 * seed (the one `phraseEntropy` derives) or to another carried seed is left
 * out: the row would be refused. THROWS when the predecessor store cannot be
 * read: the caller publishes no row then.
 */
export async function ownPredecessorsForRow(
  wasm: Wasm,
  accountIdB64: string,
  deviceKey: DeviceCipher,
  phraseEntropy: Uint8Array,
): Promise<RecoveredPredecessor[]> {
  const carried = boundedOwnPredecessors(
    await collectOwnPredecessors(accountIdB64, deviceKey, (seed) => accountKeyHexOfSeed(wasm, seed)),
  ).slice(0, wasm.recoveryPredecessorsMax());
  const mnemonic = wasm.RecoveryMnemonic.fromEntropy(phraseEntropy.slice());
  const current = mnemonic.toSeed();
  mnemonic.free();
  const out: RecoveredPredecessor[] = [];
  try {
    for (const p of carried) {
      const seed = base64Decode(p.seedB64);
      if (bytesEqual(seed, current) || out.some((o) => bytesEqual(o.seed, seed))) {
        seed.fill(0);
        continue;
      }
      out.push({ seed, adoptedAtMs: p.adoptedAtMs });
    }
  } finally {
    current.fill(0);
  }
  return out;
}
