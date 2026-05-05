// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Global registry for active 1×1 MLS groups.
 *
 * Phase 1.16e5 lifts the per-group state out of `Contacts.tsx`
 * so the `/chat/:id` route can resolve a `groupId` URL segment
 * back to a `WasmGroup` instance. The store is **memory-only**
 * — Phase-1 doesn't persist MLS state. A page reload drops every
 * group; the peer keeps theirs. See ROADMAP 1.16e6 for the
 * persistence sub-sprint.
 *
 * Eviction:
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
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";

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
  /** Wipe everything — called from `AuthProvider.logout` /
   *  `forgetDevice`. */
  clear(): void;
}

const ChatContext = createContext<ChatApi>();

export function ChatProvider(props: { children: JSX.Element }): JSX.Element {
  const [groups, setGroups] = createSignal<readonly ActiveGroup[]>([]);

  const api: ChatApi = {
    groups,
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
      return entry;
    },
    remove(groupIdB64) {
      setGroups(groups().filter((g) => g.groupIdB64 !== groupIdB64));
    },
    clear() {
      setGroups([]);
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
