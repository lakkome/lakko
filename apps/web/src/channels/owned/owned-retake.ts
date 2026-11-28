// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 item 8 - after a key reset, the account takes its own
 * channels back from its list: the rules and the order, over ports (the
 * runtime in `owned-runtime.ts` binds them to the channels state and the
 * services; the tests pass fakes).
 *
 * ## Order ({@link retakeAfterReset})
 *
 * 1. The retake is recorded as owed (`owned-facts.ts`), before anything
 *    else, so a reload finishes it at the next publisher pass.
 * 2. The list key `R`: from the NEW keys lock when an earlier attempt already
 *    made it, else from the lock of what sealed the reset (the password
 *    lock, or the passkey lock of the passkey used). No lock: there is
 *    nothing to take back.
 * 3. The new keys lock under the new lineage seed (`R ‖ S`), written before
 *    any channel is touched: from here on this device needs nothing but its
 *    own keys to continue.
 * 4. Sealed by a passkey: the password lock of the current export key is
 *    made when it is not there (the password was changed without the keys).
 * 5. Every passkey lock the list names is deleted (the reset deleted the
 *    passkeys).
 * 6. Every entry not settled yet ({@link retakeEntries}): the owner key is
 *    moved with the recovery key of `R`; `204` takes the channel locally
 *    (the list entry's read key, admission seeds and epoch seeds go into the
 *    channel's access record, and it is recorded as owned so the publisher
 *    pass founds its PublisherGroup again, rotates and reseals); a refusal
 *    that says the channel is not this list's drops the entry; anything else
 *    is tried again.
 * 7. Dropped entries leave the list; with nothing left to retry the record
 *    of step 1 goes.
 *
 * Nothing is shown while this runs (UX `identity-recovery.md` 15.1b).
 */
import type { Wasm } from "../../wasm";
import {
  finishRetake,
  noteRetakeDone,
  readRetake,
  startRetake,
  type FactsStorage,
} from "./owned-facts";
import { withChannelChanges, type OwnedEntry, type OwnedList } from "./owned-list";
import type { OpenedOwned, OwnedChannelsStore } from "./owned-store";

type Identity = InstanceType<Wasm["Identity"]>;

/** What one owner change by recovery proof came to. */
export type RetakeVerdict = "owned" | "drop" | "retry";

/**
 * The verdict of `PUT /v1/channels/{id}/owner` with the recovery proof:
 *
 * | status            | verdict | why                                             |
 * |-------------------|---------|-------------------------------------------------|
 * | 204               | owned   | the owner key is the new account's              |
 * | 401               | drop    | the channel's recovery key is not this list's   |
 * | 404, 410          | drop    | the channel is gone                             |
 * | none, anything else | retry | no answer, a limit, a server failure            |
 *
 * Pure.
 */
export function retakeVerdict(status: number | null): RetakeVerdict {
  if (status === 204) return "owned";
  if (status === 401 || status === 404 || status === 410) return "drop";
  return "retry";
}

/** What a retaken channel is on this device, read off its list entry. */
export interface RetakenChannel {
  /** `public` when the entry lists a read key, `private` when it lists
   *  admission seeds and no read key, `null` when it lists neither (the
   *  newest metadata record says it later). */
  readonly visibility: "public" | "private" | null;
  /**
   * Which subscriber rows stay let in:
   * - `tickets`: a private channel with listed admission seeds. Every row is
   *   judged again by its ticket under those seeds; a row with no valid
   *   ticket is not resealed to.
   * - `everyone-listed`: every row the service lists at the retake stays a
   *   reader (the entry carries nothing to judge a row with).
   */
  readonly admission: "tickets" | "everyone-listed";
}

/** The rule above. Pure. */
export function retakenChannel(entry: OwnedEntry): RetakenChannel {
  if (entry.readKey !== null) return { visibility: "public", admission: "everyone-listed" };
  if (entry.admission.length > 0) return { visibility: "private", admission: "tickets" };
  return { visibility: null, admission: "everyone-listed" };
}

/** What the retake needs from the channels runtime. */
export interface RetakePorts {
  /** `PUT /v1/channels/{id}/owner` signed with the recovery key
   *  `ChannelRecoveryKey.fromListKey(listKey, id)`, naming the owner key the
   *  new account derives. The HTTP status, or `null` for no answer. */
  changeOwner(idHex: string, listKey: Uint8Array): Promise<number | null>;
  /** The channel is this account's again: put it on this device (list,
   *  access record, seeds, the marks the re-founding reads, the account's
   *  register). Idempotent; throws when it must be tried again. */
  takeChannel(entry: OwnedEntry, plan: RetakenChannel): Promise<void>;
}

/** One pass over the entries. */
export interface RetakePass {
  readonly taken: string[];
  readonly dropped: string[];
  readonly retry: string[];
}

/**
 * Item 8, per entry: every entry of `list` not in `done` gets its owner
 * change; `onSettled` is told once an entry is taken or dropped. Never
 * throws: a port that throws leaves its entry to retry.
 */
export async function retakeEntries(
  list: OwnedList,
  listKey: Uint8Array,
  done: ReadonlySet<string>,
  ports: RetakePorts,
  onSettled: (idHex: string, verdict: "owned" | "drop") => void,
): Promise<RetakePass> {
  const out: RetakePass = { taken: [], dropped: [], retry: [] };
  for (const entry of list.channels) {
    if (done.has(entry.idHex)) continue;
    let verdict: RetakeVerdict;
    try {
      verdict = retakeVerdict(await ports.changeOwner(entry.idHex, listKey));
      if (verdict === "owned") await ports.takeChannel(entry, retakenChannel(entry));
    } catch {
      verdict = "retry";
    }
    if (verdict === "retry") {
      out.retry.push(entry.idHex);
      continue;
    }
    (verdict === "owned" ? out.taken : out.dropped).push(entry.idHex);
    onSettled(entry.idHex, verdict);
  }
  return out;
}

/**
 * What sealed the reset (ADR-0101 section 10): the lock to open. A passkey's
 * lock is found by its `prf` output and the server half alone;
 * `credentialTag` (its 16-byte tag in the seal announcement) is taken for
 * the caller's convenience and not needed to open it, since every passkey
 * lock the list names is deleted after a reset whatever its tag.
 */
export type ResetSealedBy =
  | { readonly kind: "password"; readonly exportKey: Uint8Array }
  | { readonly kind: "passkey"; readonly prfOutput: Uint8Array; readonly credentialTag: Uint8Array };

/** What a retake came to. */
export type RetakeOutcome =
  /** Every entry is settled and the locks are in order. */
  | "done"
  /** Something got no answer; the publisher pass (or the next call)
   *  continues. */
  | "retry"
  /** No lock opened: the account has no list this password or passkey
   *  opens. Nothing is owed. */
  | "nothing";

/** The retake's inputs. Every byte string is secret. */
export interface RetakeInput {
  readonly accountIdB64: string;
  /** `S`; `null` when the caller has none (the new keys lock must then
   *  already exist). */
  readonly serverHalf: Uint8Array | null;
  readonly sealedBy: ResetSealedBy | null;
  /** The lineage identity AFTER the reset (the new account identity). */
  readonly newLineageIdentity: Identity;
  /** The export key of the password in force, when the caller holds it. */
  readonly currentExportKey: Uint8Array | null;
  /**
   * A recall (ADR-0074): the root of the keys lock of the account's OLD
   * keys, which the recall device kept. It opens the list (`R` and `S`)
   * where no seal secret does. `null`/absent otherwise.
   */
  readonly oldKeysRoot?: Uint8Array | null;
}

/**
 * Steps 6 and 7 for an opened list. `"retry"` while an entry is left, and
 * on a storage failure. Never throws.
 */
export async function continueRetake(
  wasm: Wasm,
  store: OwnedChannelsStore,
  storage: FactsStorage | undefined,
  accountIdB64: string,
  opened: OpenedOwned,
  ports: RetakePorts,
): Promise<{ readonly outcome: "done" | "retry"; readonly list: OwnedList }> {
  let list = opened.list;
  try {
    const record = readRetake(storage, accountIdB64);
    if (record === null) return { outcome: "done", list };
    const pass = await retakeEntries(list, opened.listKey, new Set(record.done), ports, (idHex) =>
      noteRetakeDone(storage, accountIdB64, idHex),
    );
    if (pass.dropped.length > 0) {
      const written = await store.writeList(opened.listKey, (current) =>
        withChannelChanges(wasm, current, [], pass.dropped, () => 0),
      );
      list = written.list;
    }
    if (pass.retry.length > 0) return { outcome: "retry", list };
    finishRetake(storage, accountIdB64);
    return { outcome: "done", list };
  } catch {
    return { outcome: "retry", list };
  }
}

/**
 * Item 8, the whole of it (see the module comment for the order). Never
 * throws: a failure before the new keys lock is written answers `"retry"`
 * and the caller, which still holds `S`, calls again; after it the publisher
 * pass continues by itself.
 */
export async function retakeAfterReset(
  wasm: Wasm,
  store: OwnedChannelsStore,
  storage: FactsStorage | undefined,
  input: RetakeInput,
  ports: RetakePorts,
): Promise<{ readonly outcome: RetakeOutcome; readonly opened: OpenedOwned | null }> {
  let opened: OpenedOwned | null = null;
  try {
    startRetake(storage, input.accountIdB64);
    // An earlier attempt already made the new keys lock: it holds R and S.
    const again = await store.openWithKeys(input.newLineageIdentity);
    if (again !== "absent") {
      opened = again;
    } else if (input.oldKeysRoot !== undefined && input.oldKeysRoot !== null) {
      // A recall: the old keys lock holds `R` and `S` both.
      const old = await store.openWithKeysRoot(input.oldKeysRoot);
      if (old === "absent" || old.serverHalf === null) {
        finishRetake(storage, input.accountIdB64);
        return { outcome: "nothing", opened: null };
      }
      await store.makeKeysLock(input.newLineageIdentity, old.listKey, old.serverHalf);
      opened = old;
    } else {
      // An unsealed reset opens no lock: nothing comes back.
      if (input.sealedBy === null) {
        finishRetake(storage, input.accountIdB64);
        return { outcome: "nothing", opened: null };
      }
      if (input.serverHalf === null) return { outcome: "retry", opened: null };
      const sealed =
        input.sealedBy.kind === "password"
          ? await store.openWithPassword({ exportKey: input.sealedBy.exportKey, serverHalf: input.serverHalf })
          : await store.openWithPasskey({ prfOutput: input.sealedBy.prfOutput, serverHalf: input.serverHalf });
      if (sealed === "absent") {
        finishRetake(storage, input.accountIdB64);
        return { outcome: "nothing", opened: null };
      }
      await store.makeKeysLock(input.newLineageIdentity, sealed.listKey, input.serverHalf);
      opened = { ...sealed, serverHalf: input.serverHalf.slice() };
    }
    const serverHalf = opened.serverHalf;
    if (
      serverHalf !== null &&
      input.currentExportKey !== null &&
      (input.sealedBy === null || input.sealedBy.kind === "passkey")
    ) {
      // "makes the password lock again if the reset was sealed by a passkey
      // and its export key names no password lock".
      await store.relockPassword({ listKey: opened.listKey, serverHalf, newExportKey: input.currentExportKey });
    }
    await store.removeAllPasskeyLocks(opened.listKey);
    const read = await store.readList(opened.listKey);
    if (read !== null) opened = { ...opened, list: read.list, listVersion: read.version };
  } catch {
    return { outcome: "retry", opened };
  }
  const continued = await continueRetake(wasm, store, storage, input.accountIdB64, opened, ports);
  return { outcome: continued.outcome, opened: { ...opened, list: continued.list } };
}
