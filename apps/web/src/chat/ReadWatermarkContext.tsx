// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0024 — the session-wide read-watermark runtime.
 *
 * Owns the signed-in account's {@link ReadWatermarkStore} (encrypted in
 * IndexedDB by `read-watermark-idb.ts`) so both receive paths feed one place:
 * the open chat view's poll and the background receive for every other chat.
 * The chat view reads its "Sent" / "Read" state from here, so it survives a
 * reload and a chat switch.
 *
 * Mounted beside `<GroupNameProvider>`, above the background receive and the
 * router.
 */

import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import type { MemberIdentity } from "./author-identity";
import type { ReadWatermarkMap } from "./presence-ops";
import { IdbReadWatermarkBackend } from "./read-watermark-idb";
import { ReadWatermarkStore } from "./read-watermark-store";

export interface ReadWatermarkApi {
  /** Bumped on any change. `of` reads it, so a reactive caller re-renders. */
  readonly version: Accessor<number>;
  /** The watermarks for a group (empty when none). Reactive. Never mutate. */
  of(groupIdB64: string): ReadWatermarkMap;
  /** Fold one incoming read receipt. Monotone; a receipt with no resolved
   *  sender is dropped. Never throws. */
  ingest(groupIdB64: string, sender: MemberIdentity | undefined, upTo: number): void;
}

const ReadWatermarkContext = createContext<ReadWatermarkApi>();

export function ReadWatermarkProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const [version, setVersion] = createSignal(0);

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const store = new ReadWatermarkStore(new IdbReadWatermarkBackend(getDeviceKey), (err) => {
    // eslint-disable-next-line no-console
    console.warn("[read-watermark] store persist failed", err);
  });
  const unsubscribe = store.subscribe(() => setVersion((v) => v + 1));

  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === store.boundAccount()) return;
      if (account === null) {
        store.close();
        return;
      }
      void store.open(account);
    });
  });
  onCleanup(unsubscribe);

  const api: ReadWatermarkApi = {
    version,
    of(groupIdB64) {
      version();
      return store.of(groupIdB64);
    },
    ingest(groupIdB64, sender, upTo) {
      const account = untrack(accountId);
      if (account === null || sender === undefined) return;
      store.ingest(account, groupIdB64, sender, upTo);
    },
  };

  return <ReadWatermarkContext.Provider value={api}>{props.children}</ReadWatermarkContext.Provider>;
}

export function useReadWatermarks(): ReadWatermarkApi {
  const ctx = useContext(ReadWatermarkContext);
  if (ctx === undefined) throw new Error("useReadWatermarks() called outside <ReadWatermarkProvider>");
  return ctx;
}
