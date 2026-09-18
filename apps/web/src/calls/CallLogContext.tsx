// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/calls.md` §6 — the session-wide call-log runtime.
 *
 * Owns the signed-in account's {@link CallLogStore} (encrypted in IndexedDB by
 * `call-log-idb.ts`). It sits above the call host rather than inside it,
 * because a call outlives the chat it was placed from: the host records a
 * finished call wherever the user happens to be, and the chat view reads the
 * rows back whenever it is next opened.
 *
 * Mounted beside `<ReadWatermarkProvider>`, above the call host and the
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
import type { CallLogEntry } from "./call-log";
import { IdbCallLogBackend } from "./call-log-idb";
import { CallLogStore } from "./call-log-store";

export interface CallLogApi {
  /** Bumped on any change. `of` reads it, so a reactive caller re-renders. */
  readonly version: Accessor<number>;
  /** A conversation's call rows, oldest first (empty when none). Reactive.
   *  Never mutate them. */
  of(groupIdB64: string): readonly CallLogEntry[];
  /** Record one finished call. Idempotent on the call id; never throws. */
  record(groupIdB64: string, entry: CallLogEntry): void;
  /** Drop a conversation's rows (the chat was deleted or left). */
  forgetGroup(groupIdB64: string): void;
}

const CallLogContext = createContext<CallLogApi>();

export function CallLogProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const [version, setVersion] = createSignal(0);

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const store = new CallLogStore(new IdbCallLogBackend(getDeviceKey), (err) => {
    // eslint-disable-next-line no-console
    console.warn("[call-log] store persist failed", err);
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

  const api: CallLogApi = {
    version,
    of(groupIdB64) {
      version();
      return store.of(groupIdB64);
    },
    record(groupIdB64, entry) {
      const account = untrack(accountId);
      if (account === null) return;
      store.record(account, groupIdB64, entry);
    },
    forgetGroup(groupIdB64) {
      const account = untrack(accountId);
      if (account === null) return;
      store.forgetGroup(account, groupIdB64);
    },
  };

  return <CallLogContext.Provider value={api}>{props.children}</CallLogContext.Provider>;
}

/**
 * The call log, or a no-op stand-in outside the provider.
 *
 * Unlike `useReadWatermarks`, this does not throw: the call host is mounted in
 * more places than the provider is (the standalone `/call` design route among
 * them), and a missing call log must cost a row in the history, never a
 * failure to place or finish a call.
 */
export function useCallLog(): CallLogApi {
  return (
    useContext(CallLogContext) ?? {
      version: () => 0,
      of: () => [],
      record: () => undefined,
      forgetGroup: () => undefined,
    }
  );
}
