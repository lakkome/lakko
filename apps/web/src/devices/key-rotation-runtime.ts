// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The key rotation wired to the app (ADR-0066 part 3, Stage A).
 *
 * `devices/key-rotation.ts` decides; this module supplies its ports: the
 * removal operation (`removal-runtime.ts`), the auth layer's half
 * (`auth.keyRotation`), the session's DeviceGroup handle
 * (`sync/key-rotation-port.ts`), the journals and channels the account owns,
 * and the backup. One rotation runs at a time per page; the Devices sheet
 * and the whole-session runtime (`OwedKeyRotation`) share the in-flight run.
 */
import { createSignal, type Accessor } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { AuthHttpError } from "../auth/client";
import { base64Decode } from "../auth/account-id";
import { deviceIdB64Url } from "../auth/session-device";
import { markBackupDirty } from "../backup/auto-backup";
import { putChannelOwner } from "../channels/api";
import { loadOwnerMoved, markOwnerMoved } from "../channels/owner-moved";
import { useChats } from "../chat/ChatContext";
import { DiscoveryHttpError } from "../discovery/client";
import { takeOverOwnedJournals } from "../ds/rotation-takeover";
import { channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import {
  channelOwnerOutcome,
  newRotation,
  readRotation,
  rotationResumesByItself,
  runRotation,
  writeRotation,
  type PublishVerdict,
  type RemoveStepResult,
  type RotationOutcome,
  type RotationOwed,
  type RotationPorts,
  type RotationStep,
} from "./key-rotation";
import { useDeviceRemoval } from "./removal-runtime";
import { keyRotationPort } from "./sync/key-rotation-port";

/** What starting (or continuing) a rotation came to. */
export type RotateResult =
  /** A key-reset publish is owed: nothing runs in that window. */
  | { readonly kind: "blocked" }
  /** No keys in memory. */
  | { readonly kind: "not-ready" }
  | {
      readonly kind: "ran";
      readonly outcome: RotationOutcome;
      /** What the revoke or the commit threw, for the proof block. */
      readonly error: unknown;
    };

export interface KeyRotationRuntime {
  /** Reactive: the owed rotation of the signed-in account, or `null`. */
  readonly record: Accessor<RotationOwed | null>;
  /** Reactive: the step that is running now, or `null`. */
  readonly activeStep: Accessor<RotationStep | null>;
  /** Start the rotation that removes `targetKey`, or continue the record. */
  rotate(targetKey: Uint8Array, grant: string | null, attestation?: Uint8Array | null): Promise<RotateResult>;
  /** Continue the owed record (the same device), with a new proof when the
   *  commit still needs one. */
  retry(grant: string | null): Promise<RotateResult>;
  /** "Not now": the device stays removed, the keys stay as they are. */
  notNow(): Promise<void>;
  /** Continue a record that finishes by itself. Never throws. */
  resume(): Promise<void>;
}

function storage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}

function b64UrlDecode(value: string): Uint8Array {
  const padded = value + "===".slice((value.length + 3) % 4);
  return base64Decode(padded.replace(/-/g, "+").replace(/_/g, "/"));
}

const [version, setVersion] = createSignal(0);
const [activeStep, setActiveStep] = createSignal<RotationStep | null>(null);
let inflight: Promise<RotateResult> | null = null;

export function useKeyRotation(): KeyRotationRuntime {
  const auth = useAuth();
  const chats = useChats();
  const removal = useDeviceRemoval();

  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" && auth.identitySecrets() !== null ? s.account.accountId : null;
  };

  const record = (): RotationOwed | null => {
    version();
    const a = account();
    return a === null ? null : readRotation(storage(), a);
  };

  function portsFor(
    accountIdB64: string,
    attestation: Uint8Array | null | undefined,
  ): { ports: RotationPorts; error: () => unknown } {
    let error: unknown = null;
    const publish = async (
      step: "key-package" | "device-group-root" | "journals" | "channels" | "reach" | "backup",
      current: RotationOwed,
    ): Promise<PublishVerdict> => {
      switch (step) {
        case "key-package":
          // Every KeyPackage row of the account was deleted with the proven
          // upload; this device publishes again under the same device id.
          await auth.republishKeyPackage();
          return "done";
        case "device-group-root":
          await auth.keyRotation.transferDeviceGroupRoot(current.attestationB64Url ?? null);
          return "done";
        case "journals": {
          // The conversation journals this account owns: the takeover of an
          // ownership handover, to the root of the new seed. Each one is
          // persisted and retried by `JournalOwnershipProvider`.
          const oldSeed = await auth.keyRotation.predecessorSigningSeed();
          if (oldSeed === null) return "later";
          const wasm = await loadWasm();
          const old = wasm.Identity.fromSeed(oldSeed);
          try {
            return (await takeOverOwnedJournals(old.ed25519Seed)) === null ? "later" : "done";
          } finally {
            old.free();
            oldSeed.fill(0);
          }
        }
        case "channels": {
          const next = chats.accountIdentity();
          const oldSeed = await auth.keyRotation.predecessorSigningSeed();
          if (next === null || oldSeed === null) return "later";
          const wasm = await loadWasm();
          const old = wasm.Identity.fromSeed(oldSeed);
          try {
            const moved = loadOwnerMoved(accountIdB64);
            let allDone = true;
            // The channels this device publishes to; a channel it is only a
            // co-publisher of answers 401 to both keys and is left alone.
            for (const channelIdHex of channelsState.keysByChannel().keys()) {
              const id = channelIdHex.toLowerCase();
              if (moved.has(id)) continue;
              const status = await putChannelOwner(id, old, next);
              let newKeyOwns: boolean | null = null;
              if (status === 409 || status === 401) {
                // A handover naming the key that already owns the channel
                // answers 204: the new owner asking whether it landed.
                newKeyOwns = (await putChannelOwner(id, next, next)) === 204;
                // Neither key owns it: this account is a co-publisher there.
                if (!newKeyOwns && status === 401) continue;
              }
              if (channelOwnerOutcome({ status, newKeyOwns }) === "done") markOwnerMoved(accountIdB64, id);
              else allDone = false;
            }
            return allDone ? "done" : "later";
          } finally {
            old.free();
            oldSeed.fill(0);
          }
        }
        case "reach":
          // The new mailbox is claimed, and the reach session restarted for
          // the new key, when the account identity is replaced
          // (`ChatContext`, `ReachProvider`). Nothing more is sent here.
          return chats.accountIdentity() === null ? "later" : "done";
        case "backup":
          // Re-sealed under the new phrase and uploaded with a new head by
          // the automatic backup at its next tick.
          markBackupDirty(accountIdB64);
          return "done";
      }
    };
    const ports: RotationPorts = {
      remove: async (deviceIdB64, grant): Promise<RemoveStepResult> => {
        const before = keyRotationPort()?.epoch() ?? null;
        const result = await removal.remove(b64UrlDecode(deviceIdB64), grant, attestation);
        if (result.kind !== "ran") return { kind: "not-removed", failure: "other" };
        error = result.revokeError;
        const outcome = result.outcome;
        if (outcome.kind === "not-revoked") return { kind: "not-removed", failure: outcome.failure };
        if (outcome.kind === "finishing" && outcome.owed.some((s) => s !== "chats")) {
          // The DeviceGroup commit (or the directory delete before it) has
          // no answer yet: the envelope must not be sealed.
          return { kind: "finishing" };
        }
        return { kind: "removed", preRemovalEpoch: before };
      },
      prepare: async (current) => {
        const port = keyRotationPort();
        if (port === null) throw new Error("the DeviceGroup is not loaded");
        const prepared = await auth.keyRotation.prepare(current.deviceIdB64);
        // The one ordering seam (`sealKeyRotationAfterRemoval`): sealed only
        // when the removed device is out of the DeviceGroup.
        const sealed = await port.post({
          removedDeviceKey: b64UrlDecode(current.deviceIdB64),
          preRemovalEpoch: current.preRemovalEpoch === undefined ? null : BigInt(current.preRemovalEpoch),
          envelope: prepared.envelope,
        });
        return {
          newSignaturePubkeyB64: prepared.newSignaturePubkeyB64,
          preparedAtMs: prepared.preparedAtMs,
          envelopeEpoch: sealed.epoch,
        };
      },
      commit: async (current, grant) => {
        try {
          const done = await auth.keyRotation.commit(grant, [b64UrlDecode(current.deviceIdB64)]);
          return { status: 200 as const, ...done };
        } catch (err) {
          error = err;
          if (err instanceof AuthHttpError) return { status: err.status, errorCode: err.errorCode };
          return { status: null, errorCode: null };
        }
      },
      probeCommit: () => auth.keyRotation.probeCommit(),
      uploadIdentity: async (current, mode) => {
        try {
          await auth.keyRotation.uploadIdentity(
            mode,
            current.attestationB64Url ?? null,
            await auth.keyRotation.preparedProofB64(),
          );
          return { status: 200, errorCode: null };
        } catch (err) {
          if (err instanceof DiscoveryHttpError) return { status: err.status, errorCode: err.errorCode };
          return { status: null, errorCode: null };
        }
      },
      publish: async (step, current) => {
        try {
          return await publish(step, current);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(`key rotation: ${step} not done`, err instanceof Error ? err.name : "unknown");
          return "later";
        }
      },
      superseded: () => auth.keyRotation.reportSuperseded(),
      save: (next) => {
        writeRotation(storage(), accountIdB64, next);
        setVersion((v) => v + 1);
        auth.keyRotation.recordChanged();
      },
    };
    return { ports, error: () => error };
  }

  function run(
    accountIdB64: string,
    start: RotationOwed,
    grant: string | null,
    attestation: Uint8Array | null | undefined,
  ): Promise<RotateResult> {
    if (inflight !== null) return inflight;
    const { ports, error } = portsFor(accountIdB64, attestation);
    const task = runRotation(start, ports, { grant, onStep: setActiveStep })
      .then(async (outcome): Promise<RotateResult> => {
        if (outcome.kind === "complete") {
          // Step 12: the new words, now that nothing is owed.
          try {
            await auth.keyRotation.showNewPhrase();
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("key rotation: the new phrase could not be shown", err);
          }
        }
        return { kind: "ran", outcome, error: error() };
      })
      .finally(() => {
        inflight = null;
        setActiveStep(null);
        setVersion((v) => v + 1);
        auth.keyRotation.recordChanged();
      });
    inflight = task;
    return task;
  }

  return {
    record,
    activeStep,
    async rotate(targetKey, grant, attestation) {
      const a = account();
      if (a === null || chats.accountIdentity() === null) return { kind: "not-ready" };
      const deviceIdB64 = deviceIdB64Url(targetKey);
      const held = readRotation(storage(), a);
      // A rotation already past its commit finishes first; a record for
      // another device that never committed is replaced.
      if (held !== null && rotationResumesByItself(held)) return run(a, held, grant, attestation);
      if (removal.blocked()) return { kind: "blocked" };
      const start = held !== null && held.deviceIdB64 === deviceIdB64 ? held : newRotation(deviceIdB64, Date.now());
      return run(a, start, grant, attestation);
    },
    async retry(grant) {
      const a = account();
      if (a === null) return { kind: "not-ready" };
      const held = readRotation(storage(), a);
      if (held === null) return { kind: "ran", outcome: { kind: "complete" }, error: null };
      return run(a, held, grant, undefined);
    },
    async notNow() {
      const a = account();
      if (a === null) return;
      const held = readRotation(storage(), a);
      // Past the commit there is no "not now": the keys were changed.
      if (held === null || rotationResumesByItself(held) || held.done.includes("commit")) return;
      writeRotation(storage(), a, null);
      setVersion((v) => v + 1);
      auth.keyRotation.recordChanged();
      try {
        await auth.keyRotation.discardPrepared();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("key rotation: the prepared keys were not discarded", err);
      }
    },
    async resume() {
      const a = account();
      if (a === null || chats.accountIdentity() === null) return;
      const held = readRotation(storage(), a);
      if (held === null || !rotationResumesByItself(held)) return;
      try {
        await run(a, held, null, undefined);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("key rotation: resume failed", err);
      }
    },
  };
}
