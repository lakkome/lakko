// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9 — the order an end is carried out in, and nothing else.
 *
 * From one durable record, each step recorded before the next runs:
 *
 *   1. Record the end (title, left or removed) with its notice.
 *   2. Publish `conversation_ended` over the DeviceGroup. A device that heard
 *      of it from that register starts after this step.
 *   3. Stop the poll, the beacon and the shrink rotation, retire the push
 *      subscription.
 *   4. Wipe, delete the MLS group, persist the provider snapshot durably, and
 *      drop the known-groups row and the registry entry LAST.
 *
 * Then the synced registers are cleared through their own tombstones (only by
 * the device that started the end: the tombstones reach the others). A failure
 * there holds the record back but none of the steps above: the history is
 * already gone, and a register left over is inert until it is retried.
 *
 * A step that fails leaves the record where it was, and the next
 * {@link MembershipEndRuntime.resume} takes it up there; every step is
 * idempotent. A step is never re-entered while it runs.
 *
 * No Solid and no wasm here: the context binds {@link EndPort}, so the order
 * can be run by a test and a crash can be simulated at every step.
 */

import { endKey, type EndNotice, type EndOrigin, type EndRecord, type EndRefKind, type MembershipEndStore } from "./membership-end";

/** What the steps do. Each is idempotent and rejects when it did not finish. */
export interface EndPort {
  /** Step 2: fold the `ended` register and queue it, durably. Rejects while
   *  the synced state is not loaded. "Nothing to tell" is a resolve. */
  publishEnd(record: EndRecord): Promise<void>;
  /** Step 3. */
  stopReceiving(record: EndRecord): Promise<void>;
  /** Step 4. Resolves `"replaced"` when a Welcome put the group back while
   *  the end was under way: this device is in it again. */
  wipe(record: EndRecord): Promise<"wiped" | "replaced">;
  /** The synced registers, through their own tombstones. */
  clearSynced(record: EndRecord): Promise<void>;
}

/** The end of a conversation, as the caller knows it. */
export interface NewEnd {
  readonly refKind: EndRefKind;
  readonly idHex: string;
  readonly segmentsB64: readonly string[];
  /** The ids a route can name it by, for the dialog. */
  readonly routeIdsB64: readonly string[];
  readonly title: string;
  readonly left: boolean;
  readonly origin: EndOrigin;
  readonly endedEpoch: bigint;
  /** Text the composer holds, shown in the notice. */
  readonly draft: string | null;
}

export class MembershipEndRuntime {
  /** Per record: the run in progress. */
  private readonly running = new Map<string, Promise<void>>();

  constructor(
    private readonly store: MembershipEndStore,
    private readonly port: EndPort,
    private readonly now: () => number = () => Date.now(),
    /** A new end that tells the person: the dialog over the open
     *  conversation or the short notice (UX section 9). */
    private readonly onNotice?: (notice: EndNotice) => void,
  ) {}

  /**
   * Start an end. A conversation already ending is left alone. A device that
   * heard of it starts past step 2. Rejects when the record could not be
   * written: nothing has happened then, and the trigger is asked again at the
   * next commit or launch.
   */
  async begin(end: NewEnd): Promise<void> {
    const key = endKey(end.refKind, end.idHex);
    if (this.store.record(key) !== undefined) {
      await this.run(key);
      return;
    }
    const record: EndRecord = {
      key,
      refKind: end.refKind,
      idHex: end.idHex,
      segmentsB64: end.segmentsB64,
      routeIdsB64: end.routeIdsB64,
      title: end.title,
      left: end.left,
      origin: end.origin,
      endedEpoch: end.endedEpoch.toString(),
      stage: end.origin === "heard" ? "published" : "recorded",
      syncedCleared: end.origin === "heard",
      atMs: this.now(),
    };
    const notice: EndNotice | null = end.left
      ? null
      : { key, title: end.title, routeIdsB64: end.routeIdsB64, draft: end.draft, atMs: record.atMs };
    await this.store.begin(record);
    // In memory only: a notice nobody saw is not shown at the next start.
    if (notice !== null) this.onNotice?.(notice);
    await this.run(key);
  }

  /** Take up every owed record. Never rejects. */
  async resume(): Promise<void> {
    await Promise.all(this.store.records().map((r) => this.run(r.key)));
  }

  /** Run one record as far as it goes. Concurrent callers share the run. A
   *  failure is logged and left for the next {@link resume}. */
  run(key: string): Promise<void> {
    const inFlight = this.running.get(key);
    if (inFlight !== undefined) return inFlight;
    const run = this.advance(key)
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[membership-end] step did not finish; retried", err instanceof Error ? err.message : "");
      })
      .finally(() => {
        this.running.delete(key);
      });
    this.running.set(key, run);
    return run;
  }

  private async advance(key: string): Promise<void> {
    let record = this.store.record(key);
    if (record === undefined) return;
    // A step that returns after a sign-out goes no further: the next step
    // would be taken in another account's stores. The record is on disk, and
    // the account's next session takes it up.
    const account = this.store.boundAccount();
    const still = (): boolean => this.store.boundAccount() === account;
    if (record.stage === "recorded") {
      await this.port.publishEnd(record);
      if (!still()) return;
      record = await this.reach(record, { stage: "published" });
    }
    if (record.stage === "published") {
      await this.port.stopReceiving(record);
      if (!still()) return;
      record = await this.reach(record, { stage: "stopped" });
    }
    if (record.stage === "stopped") {
      const outcome = await this.port.wipe(record);
      if (!still()) return;
      record = await this.reach(record, { stage: "wiped" });
      if (outcome === "replaced") {
        await this.store.finish(key);
        return;
      }
    }
    if (!record.syncedCleared) {
      await this.port.clearSynced(record);
      if (!still()) return;
      record = await this.reach(record, { syncedCleared: true });
    }
    await this.store.finish(key);
  }

  private async reach(record: EndRecord, change: Partial<EndRecord>): Promise<EndRecord> {
    const next = { ...record, ...change };
    await this.store.update(next);
    return next;
  }
}
