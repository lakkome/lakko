// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The removal operation wired to the app (ADR-0074 section 2.3).
 *
 * `devices/removal.ts` decides; this module supplies its ports from the
 * session (the bearer, the account key that signs the statement), the
 * discovery client, the DeviceGroup and the chat fan-out, and keeps the one
 * reactive view of the owed records that both the Devices screen and the
 * whole-session runtime (`OwedDeviceRemovals`) read.
 *
 * One removal runs at a time per device (the screen and the runtime share
 * the in-flight map), so a landing that resumes a record never races the
 * button that started it.
 */
import { createSignal, type Accessor } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { AuthClient, AuthHttpError } from "../auth/client";
import { base64Decode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { deviceIdB64Url } from "../auth/session-device";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import { useChats } from "../chat/ChatContext";
import { DiscoveryClient, DiscoveryHttpError } from "../discovery/client";
import { loadWasm } from "../wasm";
import { removeDeviceByKey } from "./remove";
import {
  newRemoval,
  readRemoval,
  readRemovals,
  removalAllowed,
  resumesByItself,
  runRemoval,
  withAttestation,
  writeRemoval,
  type RemovalOutcome,
  type RemovalOwed,
  type RemovalPorts,
} from "./removal";

/** How long a passkey removal attestation is reused for a retry. The server
 *  signs it for one hour (ADR-0015); a margin is left. */
export const ATTESTATION_REUSE_MS = 55 * 60_000;

/** What starting (or continuing) a removal came to. */
export type RemoveDeviceResult =
  /** A key-reset publish is owed: no removal runs in that window. */
  | { readonly kind: "blocked" }
  /** This session cannot remove devices now (no keys in memory). */
  | { readonly kind: "not-ready" }
  | { readonly kind: "ran"; readonly outcome: RemovalOutcome; readonly revokeError: unknown };

export interface DeviceRemovalApi {
  /** Reactive: the owed removals of the signed-in account. */
  readonly owed: Accessor<readonly RemovalOwed[]>;
  /** Reactive: a key-reset publish is owed, so no removal may run. */
  readonly blocked: Accessor<boolean>;
  /** Start the removal of `targetKey`, or continue its record. `grant` is
   *  the fresh-auth grant (needed until the revoke has its answer);
   *  `attestation` the ADR-0015 passkey attestation, when one was made. */
  remove(
    targetKey: Uint8Array,
    grant: string | null,
    attestation?: Uint8Array | null,
  ): Promise<RemoveDeviceResult>;
  /** Continue every record that finishes by itself. Never throws. */
  resume(): Promise<void>;
}

function storage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}

function b64UrlDecode(value: string): Uint8Array {
  const padded = value + "===".slice((value.length + 3) % 4);
  return base64Decode(padded.replace(/-/g, "+").replace(/_/g, "/"));
}

function b64UrlEncode(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// One view for every caller in the page.
const [version, setVersion] = createSignal(0);
const inflight = new Map<string, Promise<RemoveDeviceResult>>();

/** Tell every reader the records changed. */
export function removalsChanged(): void {
  setVersion((v) => v + 1);
}

export function useDeviceRemoval(): DeviceRemovalApi {
  const auth = useAuth();
  const chats = useChats();
  const receive = useBackgroundReceive();

  const account = (): { accountIdB64: string; accountIdBytes: Uint8Array } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return null;
    return { accountIdB64: s.account.accountId, accountIdBytes: secrets.accountId };
  };

  const blocked = (): boolean => !removalAllowed({ keyResetPublishOwed: auth.publishOwed() });

  const owed = (): readonly RemovalOwed[] => {
    version();
    const a = account();
    return a === null ? [] : readRemovals(storage(), a.accountIdB64);
  };

  /** The 138-byte `device-revoke` statement for `deviceId`, signed now. */
  async function signStatement(accountIdBytes: Uint8Array, deviceId: Uint8Array): Promise<Uint8Array> {
    const identity = chats.accountIdentity();
    if (identity === null) throw new Error("no account key in this session");
    const wasm = await loadWasm();
    return wasm.signDeviceRevokeStatement(identity.ed25519Seed, accountIdBytes, deviceId);
  }

  function portsFor(a: { accountIdB64: string; accountIdBytes: Uint8Array }): {
    ports: RemovalPorts;
    revokeError: () => unknown;
  } {
    let revokeError: unknown = null;
    const ports: RemovalPorts = {
      revoke: async (deviceIdB64, grant) => {
        const token = auth.identitySecrets()?.sessionToken ?? null;
        if (token === null) return { status: null, errorCode: null };
        const deviceId = b64UrlDecode(deviceIdB64);
        // Signed right before sending: the statement is valid for 120 s.
        const statement = await signStatement(a.accountIdBytes, deviceId);
        try {
          await new AuthClient().revokeDevice(token, grant, deviceId, statement);
          return { status: 204, errorCode: null };
        } catch (err) {
          revokeError = err;
          if (err instanceof AuthHttpError) return { status: err.status, errorCode: err.errorCode };
          return { status: null, errorCode: null };
        }
      },
      deleteDirectoryRow: async (deviceIdB64) => {
        const token = auth.identitySecrets()?.sessionToken ?? null;
        if (token === null) return null;
        const deviceId = b64UrlDecode(deviceIdB64);
        // A new statement for every attempt: a retry is usually later than
        // the first one's two minutes.
        const statement = await signStatement(a.accountIdBytes, deviceId);
        const discovery = new DiscoveryClient();
        discovery.setSessionToken(token);
        try {
          await discovery.deleteKeyPackage(deviceId, statement);
          return 204;
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "device removal: directory row not deleted",
            err instanceof DiscoveryHttpError ? `${err.status} ${err.errorCode ?? ""}` : err,
          );
          return err instanceof DiscoveryHttpError ? err.status : null;
        }
      },
      removeFromDeviceGroup: async (deviceIdB64, attestationB64) => {
        const deviceKey = await getDeviceKey(a.accountIdB64);
        if (deviceKey === null) return "later";
        try {
          const result = await removeDeviceByKey({
            accountIdBytes: a.accountIdBytes,
            accountIdB64: a.accountIdB64,
            deviceKey,
            targetKey: b64UrlDecode(deviceIdB64),
            attestation: attestationB64 === null ? null : b64UrlDecode(attestationB64),
          });
          return result === "needs-attestation" ? "needs-gesture" : "removed";
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("device removal: DeviceGroup removal not done", err);
          return "later";
        }
      },
      retireFromChats: async (deviceIdB64) => {
        try {
          const tally = await receive.retireDevice(b64UrlDecode(deviceIdB64));
          return { failed: tally.failed };
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("device removal: chat fan-out not done", err);
          return null;
        }
      },
      save: (deviceIdB64, record) => {
        writeRemoval(storage(), a.accountIdB64, deviceIdB64, record);
        removalsChanged();
      },
    };
    return { ports, revokeError: () => revokeError };
  }

  function run(
    a: { accountIdB64: string; accountIdBytes: Uint8Array },
    record: RemovalOwed,
    grant: string | null,
  ): Promise<RemoveDeviceResult> {
    const key = `${a.accountIdB64}:${record.deviceIdB64}`;
    const running = inflight.get(key);
    if (running !== undefined) return running;
    const { ports, revokeError } = portsFor(a);
    const task = runRemoval(record, ports, { grant, now: () => Date.now() })
      .then((outcome): RemoveDeviceResult => ({ kind: "ran", outcome, revokeError: revokeError() }))
      .finally(() => {
        inflight.delete(key);
        removalsChanged();
      });
    inflight.set(key, task);
    return task;
  }

  return {
    owed,
    blocked,
    async remove(targetKey, grant, attestation) {
      const a = account();
      if (a === null || chats.accountIdentity() === null) return { kind: "not-ready" };
      if (blocked()) return { kind: "blocked" };
      const deviceIdB64 = deviceIdB64Url(targetKey);
      let record = readRemoval(storage(), a.accountIdB64, deviceIdB64) ?? newRemoval(deviceIdB64, Date.now());
      if (attestation !== undefined && attestation !== null) {
        record = withAttestation(record, b64UrlEncode(attestation), Date.now() + ATTESTATION_REUSE_MS);
      }
      return run(a, record, grant);
    },
    async resume() {
      const a = account();
      if (a === null || chats.accountIdentity() === null || blocked()) return;
      for (const record of readRemovals(storage(), a.accountIdB64)) {
        if (!resumesByItself(record)) continue;
        try {
          await run(a, record, null);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("device removal: resume failed", err);
        }
      }
    },
  };
}
