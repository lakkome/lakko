// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Cross-device sync runtime (Phase 2a3e2b-web).
 *
 * Sits **inside** `<AuthProvider>` (it reads the session +
 * identitySecrets the auth side publishes) and manages the
 * per-account DeviceGroup lifecycle that powers cross-device
 * reactive state — drafts, per-thread flags, conversation
 * order, read cursors.
 *
 * Three concerns rolled into one Solid context:
 *
 *   1. **Lifecycle.** On every transition into a `session`
 *      state the context lazily ensures a DeviceGroup handle
 *      via [`ensureDeviceGroup`], snapshots it after every
 *      mutation, and tears it down on logout / forgetDevice.
 *
 *   2. **Outbound state mutations.** UI calls
 *      `setDraft(groupId, ...)` / `setFlags(...)` /
 *      `setReadCursor(...)` / `setOrder(...)`; each call
 *      composes the matching typed wasm message, persists the
 *      DeviceGroup snapshot, and publishes the wire bytes to
 *      the DeviceGroup DS journal so every other device on
 *      the account converges.
 *
 *   3. **Inbound state mutations.** A background poll loop
 *      pulls the DeviceGroup journal, runs `processIncoming`
 *      on each row, dispatches application payloads through
 *      [`parseDeviceGroupPayload`], and folds the typed result
 *      into the per-state reactive maps with the last-write-
 *      wins resolver in `state.ts`.
 *
 * Same wasm Identity / DeviceGroup handle pair the
 * `apps/web/src/devices` orchestrator uses on the add-device
 * surface — but this context holds onto it for the duration
 * of the session rather than acquiring + releasing per call,
 * because the poll loop needs a long-lived handle.
 */
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../../auth/AuthContext";
import { ensureDeviceIdentitySeed } from "../../auth/device-identity-store";
import { getDeviceKey } from "../../auth/key-store";
import { DsClient, DsHttpError, DsNetworkError } from "../../ds/client";
import { loadWasm } from "../../wasm";
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
  ParsedDeviceGroupPayload,
} from "../../../wasm/lakko_ffi_wasm";

import { ensureDeviceGroup, persistDeviceGroupSnapshot } from "../lifecycle";
import { decodeJournalEntry, encodeJournalEntry } from "../journal-envelope";
import { fetchDeviceAttestKey } from "../../auth/device-attest-key";
import { mintHlc } from "./hlc";
import {
  groupIdHex,
  shouldApply,
  type DraftState,
  type FlagsState,
  type OrderState,
  type ReadCursorState,
} from "./state";

// ---------- Reactive maps + public API shape ---------------------

interface DraftMap {
  readonly [groupIdHex: string]: DraftState;
}
interface FlagsMap {
  readonly [groupIdHex: string]: FlagsState;
}
interface ReadCursorMap {
  readonly [groupIdHex: string]: ReadCursorState;
}

/** Public surface of the cross-device sync context. */
export interface DeviceGroupSyncApi {
  /** Reactive: `true` once the per-session DeviceGroup handle
   *  is loaded and the poll loop has started. UI uses this to
   *  decide whether to render "syncing…" vs the live state. */
  readonly ready: Accessor<boolean>;
  /** Reactive snapshot of the per-group draft map. */
  readonly drafts: Accessor<DraftMap>;
  /** Reactive snapshot of the per-group flags map. */
  readonly flags: Accessor<FlagsMap>;
  /** Reactive snapshot of the per-group read cursor map. */
  readonly readCursors: Accessor<ReadCursorMap>;
  /** Reactive snapshot of the whole-account explicit
   *  conversation ordering. `null` when no Order payload has
   *  arrived yet (UI falls back to "by latest message"). */
  readonly order: Accessor<OrderState | null>;

  /** Convenience helper — return the draft for `groupId` or
   *  `undefined` if none. `groupId` is the raw bytes. */
  getDraft(groupId: Uint8Array): DraftState | undefined;
  /** Convenience helper — return the flags for `groupId` or
   *  `undefined`. */
  getFlags(groupId: Uint8Array): FlagsState | undefined;
  /** Convenience helper — return the read cursor or
   *  `undefined`. */
  getReadCursor(groupId: Uint8Array): ReadCursorState | undefined;

  /** Compose + publish a `Draft` payload. Empty `text` clears
   *  the receivers' draft for the same group (the wire shape
   *  permits this canonically). */
  setDraft(
    groupId: Uint8Array,
    text: string,
    replyToId?: Uint8Array,
  ): Promise<void>;

  /** Compose + publish a `Flags` payload. Last-write-wins for
   *  the WHOLE struct — the caller supplies the full snapshot. */
  setFlags(
    groupId: Uint8Array,
    flags: {
      readonly pinned: boolean;
      readonly muted: boolean;
      readonly archived: boolean;
      readonly unreadOverride: boolean;
    },
  ): Promise<void>;

  /** Compose + publish a `ReadCursor` payload. */
  setReadCursor(
    groupId: Uint8Array,
    upToMessageId: Uint8Array,
    readAtMs: bigint,
  ): Promise<void>;

  /** Compose + publish an `Order` payload — explicit display
   *  ordering for the conversation list. */
  setOrder(groupIds: readonly Uint8Array[]): Promise<void>;
}

const DeviceGroupSyncContext = createContext<DeviceGroupSyncApi>();

// ---------- Provider implementation ------------------------------

export function DeviceGroupSyncProvider(props: {
  readonly children: JSX.Element;
}): JSX.Element {
  const auth = useAuth();

  // Reactive state maps.
  const [drafts, setDrafts] = createSignal<DraftMap>({});
  const [flags, setFlags] = createSignal<FlagsMap>({});
  const [readCursors, setReadCursors] = createSignal<ReadCursorMap>({});
  const [order, setOrder] = createSignal<OrderState | null>(null);
  const [ready, setReady] = createSignal(false);

  // Long-lived handles for the active session. `null` between
  // sessions. The poll loop reads them every iteration; the
  // send paths reads them per call.
  let identity: WasmIdentity | null = null;
  let group: WasmDeviceGroup | null = null;
  let pollAbort: AbortController | null = null;
  let activeAccountIdB64: string | null = null;

  // ----- Lifecycle: session ↔ anon transitions --------------------

  createEffect(() => {
    const s = auth.state();
    if (s.kind !== "session") {
      // Tear down on logout / forgetDevice. Cancel the poll
      // loop, free the wasm handles, clear all reactive state.
      teardown();
      return;
    }
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      // Session restored from cache without an export key
      // (cold-start). DeviceGroup needs the DeviceIdentity
      // seed which lives behind the device key — we can fetch
      // that without the export key, so this branch CAN still
      // bootstrap. The bootstrap function below pulls
      // `accountIdBytes` from `secrets`, so we have to wait.
      //
      // No-op for now; the effect re-runs when secrets land.
      return;
    }
    // Idempotent — if we already bootstrapped for the same
    // account, skip the heavy work.
    if (activeAccountIdB64 === s.account.accountId) return;
    void bootstrap(s.account.accountId, secrets.accountId);
  });

  onCleanup(() => {
    teardown();
  });

  async function bootstrap(
    accountIdB64: string,
    accountIdBytes: Uint8Array,
  ): Promise<void> {
    // Already bootstrapping concurrently — drop the duplicate.
    if (activeAccountIdB64 === accountIdB64 && identity !== null) return;
    activeAccountIdB64 = accountIdB64;

    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) {
      // No device key — recovery / forget-device race. Bail;
      // the next session transition retries.
      activeAccountIdB64 = null;
      return;
    }

    let ensured;
    try {
      const seed = await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
      ensured = await ensureDeviceGroup(
        accountIdBytes,
        accountIdB64,
        seed,
        deviceKey,
      );
      seed.fill(0);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("DeviceGroupSyncProvider bootstrap failed", err);
      activeAccountIdB64 = null;
      return;
    }

    identity = ensured.identity;
    group = ensured.group;
    setReady(true);

    // Kick off the poll loop. The abort controller is the
    // only handle we need to stop it cleanly.
    pollAbort = new AbortController();
    void runPollLoop(
      ensured.group,
      ensured.identity,
      accountIdB64,
      deviceKey,
      pollAbort.signal,
    );
  }

  function teardown(): void {
    if (pollAbort !== null) {
      pollAbort.abort();
      pollAbort = null;
    }
    if (group !== null) {
      try {
        group.free();
      } catch {
        // best-effort
      }
      group = null;
    }
    if (identity !== null) {
      try {
        identity.free();
      } catch {
        // best-effort
      }
      identity = null;
    }
    activeAccountIdB64 = null;
    setReady(false);
    setDrafts({});
    setFlags({});
    setReadCursors({});
    setOrder(null);
  }

  // ----- Inbound poll loop ---------------------------------------

  async function runPollLoop(
    g: WasmDeviceGroup,
    id: WasmIdentity,
    accountIdB64: string,
    deviceKey: CryptoKey,
    signal: AbortSignal,
  ): Promise<void> {
    const ds = new DsClient();
    const groupId = g.groupId();
    let cursor = 0;

    while (!signal.aborted) {
      let raw;
      try {
        raw = await ds.fetchGroup(groupId, {
          after: cursor,
          waitSecs: 25,
        });
      } catch (err) {
        if (signal.aborted) return;
        if (err instanceof DsHttpError && err.status === 404) {
          // Fresh journal slot — keep polling.
          continue;
        }
        if (err instanceof DsNetworkError) {
          // Transient — back off briefly + retry.
          await sleepCancellable(2000, signal);
          continue;
        }
        // Unexpected error — log + back off; we don't want a
        // single bad iteration to permanently kill the loop.
        // eslint-disable-next-line no-console
        console.warn("DeviceGroup poll loop fetch failed", err);
        await sleepCancellable(5000, signal);
        continue;
      }

      // G-13 (ADR-0015): the 32-byte verifying key peers feed to
      // `processIncomingVerifyingRemovals`. Cached (24 h). A zero key is a
      // fail-closed fallback — a removal the policy requires to be attested
      // is refused rather than applied unverified; while the policy is off,
      // non-remove entries and unattested removals pass through regardless.
      let attestVk: Uint8Array;
      try {
        attestVk = await fetchDeviceAttestKey();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("device-attest key fetch failed; attested removals will be refused", err);
        attestVk = new Uint8Array(32);
      }

      let mutated = false;
      for (const message of raw.messages) {
        cursor = message.id;
        // Unwrap the DeviceGroup journal envelope (the commit / message plus
        // any removal attestation that travelled with it).
        let entry;
        try {
          entry = decodeJournalEntry(message.payload);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("DeviceGroup journal envelope decode failed", message.id, err);
          continue;
        }
        const attestation = entry.attestation ?? new Uint8Array(0);
        let processed;
        try {
          // Refuse-to-apply an unattested removal under the policy.
          processed = g.processIncomingVerifyingRemovals(
            id,
            entry.message,
            attestation,
            attestVk,
            BigInt(Math.floor(Date.now() / 1000)),
          );
        } catch (err) {
          // Bad frame / wrong epoch / our own echo at the wrong epoch / a
          // removal refused for want of a valid attestation — log + skip.
          // Cursor still advances.
          // eslint-disable-next-line no-console
          console.warn("DeviceGroup processIncomingVerifyingRemovals failed", message.id, err);
          continue;
        }
        mutated = true;
        if (processed.kind === "Application") {
          const data = processed.applicationData;
          if (data === undefined) continue;
          try {
            const wasm = await loadWasm();
            const parsed = wasm.parseDeviceGroupPayload(data);
            applyParsed(parsed);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("parseDeviceGroupPayload failed", err);
          }
        }
        // Commit / Proposal / ExternalJoinProposal — no
        // application-state effect; we already advanced the
        // ratchet inside processIncoming.
      }

      if (mutated) {
        // Persist the post-batch snapshot so subsequent app
        // launches pick up the new epoch + counter state.
        await persistDeviceGroupSnapshot(id, accountIdB64, deviceKey);
      }
    }
  }

  function applyParsed(parsed: ParsedDeviceGroupPayload): void {
    const hlc = parsed.hlc;
    switch (parsed.kind) {
      case "readCursor": {
        const key = groupIdHex(parsed.groupId);
        const next: ReadCursorState = {
          upToMessageId: parsed.upToMessageId,
          readAtMs: parsed.readAtMs,
          updatedAtHlc: hlc,
        };
        setReadCursors((prev) => {
          if (!shouldApply(hlc, prev[key]?.updatedAtHlc)) return prev;
          return { ...prev, [key]: next };
        });
        break;
      }
      case "draft": {
        const key = groupIdHex(parsed.groupId);
        const next: DraftState = {
          text: parsed.text,
          replyToId: parsed.replyToId,
          updatedAtHlc: hlc,
        };
        setDrafts((prev) => {
          if (!shouldApply(hlc, prev[key]?.updatedAtHlc)) return prev;
          return { ...prev, [key]: next };
        });
        break;
      }
      case "flags": {
        const key = groupIdHex(parsed.groupId);
        const next: FlagsState = {
          pinned: parsed.pinned,
          muted: parsed.muted,
          archived: parsed.archived,
          unreadOverride: parsed.unreadOverride,
          updatedAtHlc: hlc,
        };
        setFlags((prev) => {
          if (!shouldApply(hlc, prev[key]?.updatedAtHlc)) return prev;
          return { ...prev, [key]: next };
        });
        break;
      }
      case "order": {
        const next: OrderState = {
          groupIds: parsed.orderGroupIds(),
          updatedAtHlc: hlc,
        };
        setOrder((prev) => {
          if (!shouldApply(hlc, prev?.updatedAtHlc)) return prev;
          return next;
        });
        break;
      }
      default:
        // Older client + future-version payload (or new
        // discriminant our build doesn't know). Drop.
        // eslint-disable-next-line no-console
        console.warn("Unknown DeviceGroup payload kind", parsed.kind);
    }
  }

  // ----- Outbound send paths -------------------------------------

  async function publishGroupMessage(wireBytes: Uint8Array): Promise<void> {
    if (group === null || identity === null || activeAccountIdB64 === null) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const ds = new DsClient();
    // G-13 (ADR-0015): wrap in the journal envelope (application messages
    // carry no attestation).
    await ds.publishGroup(group.groupId(), encodeJournalEntry(wireBytes, null));
    const deviceKey = await getDeviceKey(activeAccountIdB64);
    if (deviceKey !== null) {
      await persistDeviceGroupSnapshot(
        identity,
        activeAccountIdB64,
        deviceKey,
      );
    }
  }

  async function setDraftImpl(
    groupId: Uint8Array,
    text: string,
    replyToId?: Uint8Array,
  ): Promise<void> {
    if (group === null || identity === null) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const hlc = mintHlc();
    const bytes = group.createDeviceGroupDraftMessage(
      identity,
      groupId,
      text,
      replyToId ?? new Uint8Array(0),
      hlc,
    );
    await publishGroupMessage(bytes);
    // Update local state optimistically — the wire echo will
    // be deduped by the LWW resolver on the same HLC value.
    const key = groupIdHex(groupId);
    setDrafts((prev) => ({
      ...prev,
      [key]: {
        text,
        replyToId: replyToId ?? new Uint8Array(0),
        updatedAtHlc: hlc,
      },
    }));
  }

  async function setFlagsImpl(
    groupId: Uint8Array,
    next: {
      readonly pinned: boolean;
      readonly muted: boolean;
      readonly archived: boolean;
      readonly unreadOverride: boolean;
    },
  ): Promise<void> {
    if (group === null || identity === null) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const hlc = mintHlc();
    const bytes = group.createDeviceGroupFlagsMessage(
      identity,
      groupId,
      next.pinned,
      next.muted,
      next.archived,
      next.unreadOverride,
      hlc,
    );
    await publishGroupMessage(bytes);
    const key = groupIdHex(groupId);
    setFlags((prev) => ({
      ...prev,
      [key]: { ...next, updatedAtHlc: hlc },
    }));
  }

  async function setReadCursorImpl(
    groupId: Uint8Array,
    upToMessageId: Uint8Array,
    readAtMs: bigint,
  ): Promise<void> {
    if (group === null || identity === null) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const hlc = mintHlc();
    const bytes = group.createDeviceGroupReadCursorMessage(
      identity,
      groupId,
      upToMessageId,
      readAtMs,
      hlc,
    );
    await publishGroupMessage(bytes);
    const key = groupIdHex(groupId);
    setReadCursors((prev) => ({
      ...prev,
      [key]: { upToMessageId, readAtMs, updatedAtHlc: hlc },
    }));
  }

  async function setOrderImpl(
    groupIds: readonly Uint8Array[],
  ): Promise<void> {
    if (group === null || identity === null) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const hlc = mintHlc();
    // Defensive copy — `createDeviceGroupOrderMessage` takes a
    // plain array of Uint8Arrays; mutating later shouldn't
    // affect the wire bytes.
    const ids = groupIds.map((g) => new Uint8Array(g));
    const bytes = group.createDeviceGroupOrderMessage(identity, ids, hlc);
    await publishGroupMessage(bytes);
    setOrder({ groupIds: ids, updatedAtHlc: hlc });
  }

  // ----- Hook API ------------------------------------------------

  const api: DeviceGroupSyncApi = {
    ready,
    drafts,
    flags,
    readCursors,
    order,
    getDraft(groupId) {
      return drafts()[groupIdHex(groupId)];
    },
    getFlags(groupId) {
      return flags()[groupIdHex(groupId)];
    },
    getReadCursor(groupId) {
      return readCursors()[groupIdHex(groupId)];
    },
    setDraft: setDraftImpl,
    setFlags: setFlagsImpl,
    setReadCursor: setReadCursorImpl,
    setOrder: setOrderImpl,
  };

  return (
    <DeviceGroupSyncContext.Provider value={api}>
      {props.children}
    </DeviceGroupSyncContext.Provider>
  );
}

/** Hook — every component that reads or writes cross-device
 *  sync state calls this. Throws when invoked outside
 *  `<DeviceGroupSyncProvider>` (programmer error). */
export function useDeviceGroupSync(): DeviceGroupSyncApi {
  const ctx = useContext(DeviceGroupSyncContext);
  if (ctx === undefined) {
    throw new Error(
      "useDeviceGroupSync() called outside <DeviceGroupSyncProvider>",
    );
  }
  return ctx;
}

// ----- Helpers -----------------------------------------------------

function sleepCancellable(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}
