// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 third amendment, point 1 - a device holding the DeviceGroup keeps
 * a long poll on its knocks while the page is in the foreground.
 *
 * The loop ends only when its signal aborts (the handle it reads with is
 * being replaced, or the session ends): an admission, a failed read and a
 * failed answer all leave it listening. Before 2026-11-11 it ended after an
 * admission and relied on the handle reload to start a new one; seen live, a
 * page then stopped answering knocks, and a restored device founded the
 * DeviceGroup over it.
 */

/** One knock row as the DS returns it. */
export interface KnockRow {
  readonly id: number;
  readonly payload: Uint8Array;
  readonly createdAt: string;
}

/** What a failed knock read was. */
export type KnockReadFailure = "empty" | "network" | "other";

/** What the listener reads, answers and waits with. */
export interface KnockListenerPorts {
  /** Is the page in the foreground? */
  visible(): boolean;
  /** Resolves when the page is back in the foreground or `signal` aborts. */
  untilVisible(signal: AbortSignal): Promise<void>;
  /** One long poll after the knock cursor. Rejects on failure. */
  read(signal: AbortSignal): Promise<readonly KnockRow[]>;
  /** What a rejected read was. */
  classify(err: unknown): KnockReadFailure;
  /** Answer (and maybe admit) the knocks. `listeningSinceMs` is when this
   *  listening run started. May reject; the loop goes on. */
  answer(rows: readonly KnockRow[], listeningSinceMs: number, signal: AbortSignal): Promise<void>;
  sleep(ms: number, signal: AbortSignal): Promise<void>;
  now(): number;
  /** One log line, with no identifiers. */
  log(line: string): void;
}

/** Run until `signal` aborts. Never rejects. */
export async function runKnockListener(ports: KnockListenerPorts, signal: AbortSignal): Promise<void> {
  ports.log("DeviceGroup knock listener started");
  let listeningSinceMs = ports.now();
  while (!signal.aborted) {
    try {
      if (!ports.visible()) {
        ports.log("DeviceGroup knock listener paused: page in the background");
        await ports.untilVisible(signal);
        // Knocks left from the background stay unanswered when too old.
        listeningSinceMs = ports.now();
        if (!signal.aborted) ports.log("DeviceGroup knock listener resumed");
        continue;
      }
      let rows: readonly KnockRow[];
      try {
        rows = await ports.read(signal);
      } catch (err) {
        if (signal.aborted) break;
        const kind = ports.classify(err);
        if (kind === "other") ports.log("DeviceGroup knock read failed");
        await ports.sleep(kind === "empty" ? 1_000 : kind === "network" ? 2_000 : 5_000, signal);
        continue;
      }
      if (signal.aborted) break;
      // Read while the page went to the background: answered when it is back.
      if (rows.length === 0 || !ports.visible()) continue;
      await ports.answer(rows, listeningSinceMs, signal);
    } catch {
      // An answer that failed (or any port that threw): keep listening. Never
      // reject: a reload awaits this loop.
      if (signal.aborted) break;
      ports.log("DeviceGroup knock answer failed");
      await ports.sleep(2_000, signal).catch(() => undefined);
    }
  }
  ports.log("DeviceGroup knock listener stopped");
}
