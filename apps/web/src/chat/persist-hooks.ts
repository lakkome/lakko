// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `onPersist` hook every `ChatClient` and `ContactsClient` on the page is
 * given: the MLS snapshot write that must hold before a publish.
 *
 * ## Why it throws where `persistSnapshot` returns
 *
 * `ChatContext.persistSnapshot` treats "nothing to persist" as success: no
 * session, no canonical Identity, no device key. Those are exactly the states
 * a sign-out or Forget device puts the page in while work that already
 * advanced the MLS state is suspended on its persist. A hook built on it
 * answered success there, and the caller then published: a send whose ratchet
 * step was never recorded, or a group's founding Welcome whose founder state
 * was never recorded. The rule is the other way round: a persist that did not
 * happen must stop the publish. So the hook resolves only when the snapshot
 * was written (`persistSnapshotDurable`), and throws otherwise.
 *
 * Callers that persist best-effort (`ContactsClient.persistBestEffort`) catch
 * and log the throw, as they always did for a failed write; callers that
 * persist before a publish (`persistDurableOrThrow`) abort the publish.
 */

/** A persist was asked for while no account was signed in on this page. */
export class NoSessionPersistError extends Error {
  override name = "NoSessionPersistError";
  constructor() {
    super("no signed-in account: the MLS snapshot was not persisted");
  }
}

/** The slice of the chats context the hook needs. */
export interface DurableSnapshotWriter {
  persistSnapshotDurable(accountIdB64: string): Promise<void>;
}

/**
 * A hook that writes the MLS snapshot of the account `accountOf()` names now,
 * or throws: {@link NoSessionPersistError} when it names none, and whatever
 * `persistSnapshotDurable` throws (no Identity, no device key, the session
 * changed under the write, the account's stores sealed by a wipe, an
 * IndexedDB failure).
 */
export function durableSnapshotHook(
  chats: DurableSnapshotWriter,
  accountOf: () => string | null,
): () => Promise<void> {
  return async () => {
    const account = accountOf();
    if (account === null) throw new NoSessionPersistError();
    await chats.persistSnapshotDurable(account);
  };
}

/** The account a session state names: the signed-in account, else `null`. */
export function sessionAccountOf(state: {
  readonly kind: string;
  readonly account?: { readonly accountId: string };
}): string | null {
  return state.kind === "session" && state.account !== undefined ? state.account.accountId : null;
}
