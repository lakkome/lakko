// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Global registry for active 1×1 MLS groups + the canonical
 * per-session `WasmIdentity` whose provider all groups share.
 *
 * ## Why one canonical Identity
 *
 * Every `Group` operation — `createApplicationMessage`,
 * `processIncoming`, `addMember`, `joinFromWelcome`, … — reads and
 * writes MLS state through the OpenMLS provider that's owned by
 * the `Identity` instance passed in. If the caller spins up a
 * fresh `Identity.fromSeed(exportKey)` per call (the 1.16e4
 * pattern), every operation talks to a brand-new empty provider:
 *
 *   * private keys minted by `createKeyPackage` end up in a
 *     provider that's about to be discarded — Welcome decryption
 *     fails;
 *   * application-message ratchet state advances inside the
 *     `MlsGroup` Rust struct but the provider never persists it,
 *     so the next reload starts from the wrong epoch even if we
 *     `restoreStorage` from disk.
 *
 * Phase 1.16e6c1 fixes that by promoting **one** `WasmIdentity`
 * per session into the chat context. `AuthProvider` builds it
 * once (`Identity.fromSeed(exportKey)` right after signup / login
 * / recover) and hands it off via `setIdentity`. Every Group
 * created or joined in this session is bound to that same
 * provider — so a single `snapshotStorage()` captures the entire
 * MLS state.
 *
 * ## Lifecycle
 *
 *   * `setIdentity(id)` — replaces the canonical Identity.
 *     `free()`s the previous one to release the WASM allocation.
 *   * `clear()` — drops every active group and frees the
 *     canonical Identity. Called from `AuthProvider.logout` /
 *     `forgetDevice`.
 *   * `persistSnapshot(accountIdB64)` — captures the canonical
 *     Identity's provider storage, encrypts under the device root
 *     key (1.16c3c2), writes to the IDB `mls-store` (1.16e6c1
 *     foundation; 1.16e6c2 wires auto-persist hooks at every
 *     mutation point).
 *
 * Eviction of group entries:
 *
 *   * `add(...)` upserts (same `groupIdB64` replaces the prior
 *     entry — useful when a Welcome arrives after a reload and
 *     the in-memory replacement should win).
 *   * `remove(groupIdB64)` is a deliberate user gesture (Phase
 *     1.16e6+ adds `Group::leave` plumbing).
 *   * `clear()` is called from `AuthProvider.logout` /
 *     `forgetDevice` so per-account state never bleeds across
 *     sessions.
 */
import {
  createContext,
  createSignal,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { PushClient } from "../push/client";
import { getOrCreateDevicePushId } from "../push/device-id";
import { saveMlsSnapshot } from "./mls-store";

/**
 * 1.20e — Subscribe this device to push wakes for the given
 * group. Idempotent server-side via composite-PK INSERT IGNORE;
 * safe to call on every group `add` (which includes rehydrate
 * upserts). All failures are logged and swallowed — push is a
 * UX nicety, foreground polling still works without it.
 */
async function subscribeToGroupPushBestEffort(
  groupId: Uint8Array,
): Promise<void> {
  try {
    const devicePseudonym = getOrCreateDevicePushId();
    const client = new PushClient();
    await client.subscribeGroup(devicePseudonym, groupId);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push group subscribe failed", err);
  }
}

/** One entry in the active-groups registry. */
export interface ActiveGroup {
  /** base64(group_id) — also serves as the URL slug at
   *  `/chat/{groupIdB64Url}` (with URL-safe alphabet). */
  readonly groupIdB64: string;
  /** UI-rendered title. "Group with @alice" or
   *  "Joined group (Welcome 42)". */
  readonly label: string;
  /** Live MLS group reference. Phase-1 caller is responsible
   *  for not holding stale copies after `remove`. */
  readonly group: WasmGroup;
  /** ms since epoch — used to display "active for N minutes"
   *  and to pick a stable initial sort order. */
  readonly createdAtMs: number;
}

/** Public surface of the chat context. */
export interface ChatApi {
  /** Reactive list of all active groups, sorted by
   *  creation time (oldest first). */
  readonly groups: Accessor<readonly ActiveGroup[]>;
  /** Canonical `WasmIdentity` for this session. Same instance
   *  used for every Group operation so `snapshotStorage()`
   *  captures the entire MLS state. `null` until
   *  `AuthProvider` resolves the user's export key (signup /
   *  login / recover); also `null` after a session restored
   *  from IDB until the user re-enters their password. */
  readonly identity: Accessor<WasmIdentity | null>;
  /** Look up a group by its base64-encoded `group_id`. Returns
   *  `undefined` if the group isn't in the registry — the UI
   *  surfaces a "group not found, please re-create" panel. */
  get(groupIdB64: string): ActiveGroup | undefined;
  /** Insert (or replace) a group in the registry. The
   *  `groupIdB64` field is computed from the live
   *  `group.groupId` accessor. */
  add(input: Omit<ActiveGroup, "groupIdB64" | "createdAtMs">): ActiveGroup;
  /** Drop a group from the registry. No-op when missing. */
  remove(groupIdB64: string): void;
  /** Replace the canonical Identity. Frees the previous
   *  instance to release the WASM allocation. Pass `null` to
   *  drop the canonical Identity without setting a new one
   *  (used by `clear()` internally and by `logout` to keep the
   *  provider order tidy). */
  setIdentity(identity: WasmIdentity | null): void;
  /** Capture the canonical Identity's provider storage, encrypt
   *  under the device root key for `accountIdB64`, and write
   *  the resulting blob to the `mls-store` IDB row.
   *
   *  No-op (resolves successfully) when:
   *    * no canonical Identity is set — the user hasn't
   *      authenticated this session, so there's nothing to
   *      persist;
   *    * no device root key is in IDB for `accountIdB64` —
   *      caller is in a half-reset state (forgetDevice in
   *      flight) and we'd just throw away the snapshot anyway.
   *
   *  Throws on actual encryption / IDB-write failures so the
   *  caller can decide whether to retry (transient) or surface
   *  the error (persistent — disk full, etc.). */
  persistSnapshot(accountIdB64: string): Promise<void>;
  /** Wipe every active group AND the canonical Identity —
   *  called from `AuthProvider.logout` / `forgetDevice`. */
  clear(): void;
}

const ChatContext = createContext<ChatApi>();

export function ChatProvider(props: { children: JSX.Element }): JSX.Element {
  const [groups, setGroups] = createSignal<readonly ActiveGroup[]>([]);
  const [identity, setIdentitySig] = createSignal<WasmIdentity | null>(null);

  function replaceIdentity(next: WasmIdentity | null): void {
    const prev = identity();
    // Free the prior allocation when it's actually different —
    // re-setting the same instance (as a no-op idempotency
    // safety net) must not call `.free()` and yank the bytes
    // out from under live Group references.
    if (prev !== null && prev !== next) {
      try {
        prev.free();
      } catch {
        // wasm-bindgen throws if `free()` is called twice. The
        // duplicate is a programming bug we'd rather log than
        // crash on.
        // eslint-disable-next-line no-console
        console.warn("ChatProvider: free() on a stale Identity threw");
      }
    }
    setIdentitySig(next);
  }

  const api: ChatApi = {
    groups,
    identity,
    get(groupIdB64) {
      return groups().find((g) => g.groupIdB64 === groupIdB64);
    },
    add(input) {
      const groupIdB64 = base64Encode(input.group.groupId);
      const entry: ActiveGroup = {
        groupIdB64,
        label: input.label,
        group: input.group,
        createdAtMs: Date.now(),
      };
      // Upsert by groupIdB64 — a duplicate Welcome on the same
      // group should reuse the slot rather than pile up.
      const next = groups().filter((g) => g.groupIdB64 !== groupIdB64);
      setGroups([...next, entry]);
      // 1.20e — subscribe this device to push wakes for the
      // group. Fire-and-forget: the message can still arrive
      // via foreground polling if push-gw is unreachable.
      // Idempotent server-side (composite PK on
      // `push_group_subscriptions`), so re-running on a
      // rehydrate / dedupe upsert is a no-op.
      void subscribeToGroupPushBestEffort(input.group.groupId);
      return entry;
    },
    remove(groupIdB64) {
      setGroups(groups().filter((g) => g.groupIdB64 !== groupIdB64));
    },
    setIdentity(next) {
      replaceIdentity(next);
    },
    async persistSnapshot(accountIdB64) {
      const id = identity();
      if (id === null) {
        // No canonical Identity → nothing to persist. Treat as
        // success so callers can `.persistSnapshot(...)` from
        // generic post-mutation hooks without first checking
        // the auth state.
        return;
      }
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        // The device key vanished out from under us
        // (`forgetDevice` wiping IDB while a snapshot was
        // queued, private mode quota, …). Drop the snapshot
        // rather than throwing — there's no recoverable state.
        return;
      }
      const snapshotBytes = id.snapshotStorage();
      const knownGroupIds = groups().map((g) => g.groupIdB64);
      await saveMlsSnapshot(
        accountIdB64,
        deviceKey,
        snapshotBytes,
        knownGroupIds,
      );
    },
    clear() {
      setGroups([]);
      replaceIdentity(null);
    },
  };

  return (
    <ChatContext.Provider value={api}>{props.children}</ChatContext.Provider>
  );
}

/** Hook — every component that touches the chat registry calls
 *  this. Throws if invoked outside `<ChatProvider>` — that's a
 *  wiring bug worth surfacing loudly. */
export function useChats(): ChatApi {
  const ctx = useContext(ChatContext);
  if (ctx === undefined) {
    throw new Error("useChats() called outside <ChatProvider>");
  }
  return ctx;
}
