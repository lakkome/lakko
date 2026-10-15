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
  onCleanup,
  type Accessor,
  type JSX,
  useContext,
} from "solid-js";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { loadWasm } from "../wasm";
import { dsAccess } from "../ds/access";
import {
  clearDsAccess,
  installMailboxAccess,
  installPredecessorChainAccess,
} from "../ds/session-access";
import {
  loadMlsSnapshot,
  saveMlsSnapshot,
  saveSnapshotWithCommitEntry,
} from "./mls-store";
import type { NewCommitEntry, PendingCommitEntry } from "./commit-outbox";

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
  /** ADR-0050 — the account identity (phrase-seeded), kept beside the
   *  device identity above for everything that is about the PERSON rather
   *  than a leaf: contact requests and acks, invite capabilities, the
   *  discovery bundle, the safety number. `identity()` is the device. */
  readonly accountIdentity: Accessor<WasmIdentity | null>;
  /** ADR-0074 section 3.5 — after a proven key rotation, the identity of the
   *  account key the conversation leaves are still bound to (the proven
   *  predecessor this device holds); `null` when the account never rotated.
   *  Its mailbox is where peers address this account's leaves. */
  readonly predecessorAccountIdentity: Accessor<WasmIdentity | null>;
  /** ADR-0074 section 3.5a - own predecessors are a CHAIN. After a second
   *  rotation the account has held keys between the conversation key above
   *  and the current one; these are their identities, older first. Each
   *  one's mailbox is read inside its own 30-day window, and a committer
   *  delegation may still name any of them. Empty until a second rotation. */
  readonly intermediatePredecessorIdentities: Accessor<readonly WasmIdentity[]>;
  /** Every own predecessor identity, OLDEST first: the conversation key,
   *  then the intermediate ones. */
  readonly ownPredecessorIdentities: Accessor<readonly WasmIdentity[]>;
  /** The account identity for everything that is about this account INSIDE a
   *  conversation (the role table, the journal ownership, an envelope sealed
   *  to the member): the predecessor while one is held, else the account
   *  identity. Never for the directory, statements or the safety number. */
  readonly conversationAccountIdentity: Accessor<WasmIdentity | null>;
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
  /** Replace the account identity (ADR-0050). Frees the previous one. */
  setAccountIdentity(identity: WasmIdentity | null): void;
  /** Replace the predecessor identity (ADR-0074). Frees the previous one. */
  setPredecessorAccountIdentity(identity: WasmIdentity | null): void;
  /** Replace the intermediate predecessor identities (ADR-0074 section
   *  3.5a). Frees the ones no longer listed. */
  setIntermediatePredecessorIdentities(identities: readonly WasmIdentity[]): void;
  /** ADR-0074 section 3.5 — a proven key rotation in a LIVE session: `next`
   *  becomes the account identity. When no predecessor is held yet, the
   *  identity that was the account's becomes the predecessor AS THE SAME
   *  OBJECT (never freed, never rebuilt): its MLS provider holds the
   *  account's PublisherGroups, which must survive the rotation. When one is
   *  held already (a second rotation), the identity that was the account's
   *  joins the intermediate predecessors instead of being freed. */
  rotateAccountIdentity(next: WasmIdentity): void;
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
  /** The FAIL-CLOSED sibling of `persistSnapshot`: identical write, but it
   *  THROWS where `persistSnapshot` silently no-ops — no canonical Identity,
   *  a canonical Identity other than `expectedIdentity` (the session changed
   *  under the caller), or no device key. Used by the KeyPackage republish
   *  (`chat/keypackage-publisher.ts`), which must not upload a KeyPackage
   *  whose private key did not reach disk. */
  persistSnapshotDurable(
    accountIdB64: string,
    expectedIdentity?: WasmIdentity,
  ): Promise<void>;
  /** ADR-0040 (G-35) — persist the canonical Identity's provider snapshot AND a
   *  durable pending-commit outbox entry in ONE atomic IndexedDB transaction,
   *  awaited, FAIL-CLOSED (throws). A group-membership commit path calls this
   *  BEFORE it publishes the commit; a throw MUST abort the publish so the group
   *  is never advanced on the wire without a durable, re-publishable record of
   *  the commit. Unlike `persistSnapshot` (which no-ops when there is no
   *  identity / device key), this THROWS in those cases — a commit we cannot
   *  durably record must not be published. */
  persistSnapshotWithCommit(
    accountIdB64: string,
    entry: NewCommitEntry,
  ): Promise<void>;
  /** ADR-0040 F1 (lakko-security) — reset the in-memory group `groupIdB64` back
   *  to the last durable snapshot (epoch N), dropping an un-published eager
   *  merge. A group-membership commit path calls this (via
   *  `ContactsClient.onCommitPersistAbort`) after a FAIL-CLOSED persist-abort:
   *  the core create-commit already eager-merged N -> N+1 in the shared provider,
   *  but the atomic persist threw so the durable snapshot stays at N with no
   *  outbox entry — leaving the in-memory group poisoned at an un-published N+1.
   *  Left alone, a subsequent same-session commit would build on N+1 and publish
   *  a commit peers (still at N) drop as wrong-epoch, stranding the WHOLE group
   *  ("device-ahead"), and the outbox sweep cannot catch it (a clean atomic abort
   *  wrote no entry). This restores the shared provider from the durable N
   *  snapshot and RELOADS the group via `Group.load`, replacing the cached
   *  registry reference so `props.entry.group` (read reactively via `get`) sees
   *  the epoch-N group on the next access. THROWS (fail-safe) when there is no
   *  canonical Identity / device key / durable snapshot — these four paths all
   *  operate on a created-and-persisted group, so a missing snapshot is a hard,
   *  surfaced error, never a silent leave-poisoned. */
  resetGroupToDurableSnapshot(
    accountIdB64: string,
    groupIdB64: string,
  ): Promise<void>;
  /** Wipe every active group AND the canonical Identity —
   *  called from `AuthProvider.logout` / `forgetDevice`. */
  clear(): void;
}

const ChatContext = createContext<ChatApi>();

export function ChatProvider(props: { children: JSX.Element }): JSX.Element {
  const [groups, setGroups] = createSignal<readonly ActiveGroup[]>([]);
  const [identity, setIdentitySig] = createSignal<WasmIdentity | null>(null);
  const [accountIdentity, setAccountIdentitySig] =
    createSignal<WasmIdentity | null>(null);

  function replaceAccountIdentity(next: WasmIdentity | null): void {
    const prev = accountIdentity();
    if (prev !== null && prev !== next) {
      try {
        prev.free();
      } catch {
        // eslint-disable-next-line no-console
        console.warn("ChatProvider: free() on a stale account Identity threw");
      }
    }
    setAccountIdentitySig(next);
    // ADR-0053 — the account mailbox is read under this identity's key.
    installMailboxAccess("account", next);
  }

  const [predecessorIdentity, setPredecessorIdentitySig] =
    createSignal<WasmIdentity | null>(null);

  function replacePredecessorIdentity(next: WasmIdentity | null): void {
    const prev = predecessorIdentity();
    if (prev !== null && prev !== next) {
      try {
        prev.free();
      } catch {
        // eslint-disable-next-line no-console
        console.warn("ChatProvider: free() on a stale predecessor Identity threw");
      }
    }
    setPredecessorIdentitySig(next);
    // ADR-0074 section 3.5 — the mailbox the conversation leaves name.
    installMailboxAccess("predecessor", next);
  }

  const [intermediateIdentities, setIntermediateIdentitiesSig] = createSignal<
    readonly WasmIdentity[]
  >([]);

  function replaceIntermediateIdentities(next: readonly WasmIdentity[]): void {
    for (const prev of intermediateIdentities()) {
      if (next.includes(prev)) continue;
      try {
        prev.free();
      } catch {
        // eslint-disable-next-line no-console
        console.warn("ChatProvider: free() on a stale intermediate predecessor Identity threw");
      }
    }
    setIntermediateIdentitiesSig([...next]);
    // ADR-0074 section 3.5a rule 4 — each one's mailbox is still read.
    installPredecessorChainAccess(next);
  }

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
    // ADR-0078 — no device mailbox: nothing is claimed or read for the device
    // identity. Welcomes land in the account mailbox (`replaceAccountIdentity`).
  }

  // ADR-0053 contract point 1 — every request to a conversation group's
  // journal is proven under the access key of the epoch that group is at.
  // The DS client resolves the signer through this source at the moment of
  // each request, so a group reloaded by `resetGroupToDurableSnapshot` or
  // replaced by an upsert is picked up without re-registering.
  const unregisterSource = dsAccess.registerGroupSource((groupId) => {
    const id = identity();
    if (id === null) return null;
    const groupIdB64 = base64Encode(groupId);
    const entry = groups().find((g) => g.groupIdB64 === groupIdB64);
    if (entry === undefined) return null;
    return entry.group.dsAccessSigner(id);
  });
  onCleanup(unregisterSource);

  const api: ChatApi = {
    groups,
    identity,
    accountIdentity,
    setAccountIdentity(next) {
      replaceAccountIdentity(next);
    },
    predecessorAccountIdentity: predecessorIdentity,
    conversationAccountIdentity: () => predecessorIdentity() ?? accountIdentity(),
    intermediatePredecessorIdentities: intermediateIdentities,
    ownPredecessorIdentities: () => {
      const oldest = predecessorIdentity();
      // Without a conversation predecessor (a device whose leaves are bound
      // to the current key, ADR-0074) the earlier keys are still held.
      return oldest === null ? intermediateIdentities() : [oldest, ...intermediateIdentities()];
    },
    setPredecessorAccountIdentity(next) {
      replacePredecessorIdentity(next);
      // Without a conversation key there is no chain either.
      if (next === null) replaceIntermediateIdentities([]);
    },
    setIntermediatePredecessorIdentities(next) {
      replaceIntermediateIdentities(next);
    },
    rotateAccountIdentity(next) {
      const previous = accountIdentity();
      if (predecessorIdentity() === null && previous !== null && previous !== next) {
        // Moved, not freed: set the signal first so `replaceAccountIdentity`
        // finds another object in its slot.
        setPredecessorIdentitySig(previous);
        installMailboxAccess("predecessor", previous);
        setAccountIdentitySig(next);
        installMailboxAccess("account", next);
        return;
      }
      if (predecessorIdentity() !== null && previous !== null && previous !== next) {
        // A second rotation: the key that was current is now an
        // intermediate predecessor. Kept, not freed (its mailbox is read
        // for 30 days and a delegation may still name it).
        setAccountIdentitySig(next);
        installMailboxAccess("account", next);
        replaceIntermediateIdentities([...intermediateIdentities(), previous]);
        return;
      }
      replaceAccountIdentity(next);
    },
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
      // ADR-0042 — `add` no longer subscribes to push here. Subscribing
      // under the raw DS `group_id` was the invariant-#5 leak this ADR
      // closes. Push subscription now routes under the opaque `push_gid`
      // and is driven by the push-routing orchestration
      // (`ensureGroupPushRouting` on chat-open / legacy-sweep, and the
      // receive path when the distribution envelope arrives) — never here,
      // where the account id + device key aren't in scope and the secret
      // may not yet be held.
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
        base64Encode(id.signaturePublicKey),
      );
    },
    async persistSnapshotDurable(accountIdB64, expectedIdentity) {
      const id = identity();
      if (id === null) {
        throw new Error("persistSnapshotDurable: no canonical Identity");
      }
      if (expectedIdentity !== undefined && id !== expectedIdentity) {
        throw new Error(
          "persistSnapshotDurable: the canonical Identity changed (session switched)",
        );
      }
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        throw new Error("persistSnapshotDurable: no device key");
      }
      // Re-checked after the await: a logout / forgetDevice in that window
      // freed the identity this snapshot would be taken from.
      if (identity() !== id) {
        throw new Error(
          "persistSnapshotDurable: the canonical Identity changed (session switched)",
        );
      }
      const snapshotBytes = id.snapshotStorage();
      const knownGroupIds = groups().map((g) => g.groupIdB64);
      await saveMlsSnapshot(
        accountIdB64,
        deviceKey,
        snapshotBytes,
        knownGroupIds,
        base64Encode(id.signaturePublicKey),
      );
    },
    async persistSnapshotWithCommit(accountIdB64, entry) {
      // FAIL-CLOSED (unlike persistSnapshot's no-op returns): a commit path is
      // calling us BEFORE it publishes, so if we cannot durably record the
      // commit + snapshot, we MUST throw so the caller aborts the publish.
      const id = identity();
      if (id === null) {
        throw new Error(
          "persistSnapshotWithCommit: no canonical Identity — commit aborted",
        );
      }
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        throw new Error(
          "persistSnapshotWithCommit: no device key — commit aborted",
        );
      }
      const snapshotBytes = id.snapshotStorage();
      const knownGroupIds = groups().map((g) => g.groupIdB64);
      const full: PendingCommitEntry = {
        ...entry,
        accountId: accountIdB64,
        acked: false,
      };
      // ONE atomic transaction over the snapshot + outbox stores.
      await saveSnapshotWithCommitEntry(
        accountIdB64,
        deviceKey,
        snapshotBytes,
        knownGroupIds,
        full,
        base64Encode(id.signaturePublicKey),
      );
    },
    async resetGroupToDurableSnapshot(accountIdB64, groupIdB64) {
      // FAIL-SAFE throughout: a poisoned in-memory N+1 group that we cannot
      // reset must surface a HARD error, never be silently left in place.
      const canonical = identity();
      if (canonical === null) {
        throw new Error(
          "resetGroupToDurableSnapshot: no canonical Identity — cannot reset (ADR-0040 F1)",
        );
      }
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        throw new Error(
          "resetGroupToDurableSnapshot: no device key — cannot reset (ADR-0040 F1)",
        );
      }
      const snapshot = await loadMlsSnapshot(accountIdB64, deviceKey);
      if (snapshot === null) {
        // These four commit paths only run on a created-and-persisted group, so
        // the durable snapshot MUST exist. Its absence is an unrecoverable
        // inconsistency — throw rather than leave the group poisoned at N+1.
        throw new Error(
          "resetGroupToDurableSnapshot: no durable snapshot for an existing group " +
            "— in-memory group left at an un-published N+1 (ADR-0040 F1)",
        );
      }
      // Restore the WHOLE shared provider back to the durable state. This drops
      // THIS group's un-published eager merge (N+1 -> N). Other groups are
      // restored to their own last-durable epoch too, which — because every
      // successful mutation persists the whole provider — matches their cached
      // in-memory structs in the steady state (see the report's flagged edge:
      // a second concurrent group also mid-failed-persist).
      canonical.restoreStorage(snapshot.snapshotBytes);
      // Rebuild the in-memory MlsGroup for this id from the just-restored
      // provider (the exact `rehydrateMlsState` idiom) — it comes back at N.
      const wasm = await loadWasm();
      const groupIdBytes = base64Decode(groupIdB64);
      const reloaded = wasm.Group.load(canonical, groupIdBytes);
      if (reloaded === undefined) {
        throw new Error(
          "resetGroupToDurableSnapshot: Group.load returned undefined after restore " +
            `for ${groupIdB64} (ADR-0040 F1)`,
        );
      }
      // Replace the cached registry reference, preserving the existing label +
      // creation time. `props.entry.group` (read via `get`) is reactive on the
      // `groups` signal, so every consumer sees the epoch-N group next access.
      const existing = groups().find((g) => g.groupIdB64 === groupIdB64);
      const rest = groups().filter((g) => g.groupIdB64 !== groupIdB64);
      setGroups([
        ...rest,
        {
          groupIdB64,
          label: existing?.label ?? `Group ${groupIdB64.slice(0, 6)}`,
          group: reloaded,
          createdAtMs: existing?.createdAtMs ?? Date.now(),
        },
      ]);
    },
    clear() {
      setGroups([]);
      replaceIdentity(null);
      replaceAccountIdentity(null);
      replacePredecessorIdentity(null);
      replaceIntermediateIdentities([]);
      clearDsAccess();
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
