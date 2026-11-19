// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 amendment (C-88) — a Welcome refused because this device still
 * holds its group as active.
 */
import { describe, expect, it } from "vitest";

import { runHeldWelcomePass, type HeldWelcomeOutcome, type HeldWelcomePort } from "./held-welcomes";
import type { HeldWelcome } from "./membership-end";

const WELCOME: HeldWelcome = { messageId: 1, groupIdB64: "g", welcomeB64: "w", heldAtMs: 0 };
const REFUSED: HeldWelcomeOutcome<string> = { kind: "still-active", groupIdB64: "g" };

function port(
  outcomes: HeldWelcomeOutcome<string>[],
  over: Partial<HeldWelcomePort<string>> = {},
): HeldWelcomePort<string> & { held(): readonly HeldWelcome[]; log: string[] } {
  let held: readonly HeldWelcome[] = [WELCOME];
  const log: string[] = [];
  return {
    log,
    held: () => held,
    open: async () => {
      log.push("open");
      return outcomes.shift() ?? REFUSED;
    },
    readJournal: async () => {
      log.push("read");
      return true;
    },
    settle: async () => {
      log.push("settle");
    },
    drop: async (id) => {
      log.push(`drop ${id}`);
      held = [];
    },
    noteFailed: () => log.push("failed"),
    current: () => true,
    ...over,
  };
}

describe("a held Welcome", () => {
  it("opens once the end it waited for ran, and is gone", async () => {
    const p = port([REFUSED, { kind: "joined", entry: "group" }]);
    expect(await runHeldWelcomePass(p)).toEqual(["group"]);
    expect(p.log).toEqual(["settle", "open", "read", "settle", "open", "drop 1"]);
    expect(p.held()).toEqual([]);
  });

  it("goes as a duplicate or a forgery when the journal is drained and the group still active", async () => {
    const p = port([REFUSED, REFUSED]);
    expect(await runHeldWelcomePass(p)).toEqual([]);
    expect(p.log).toEqual(["settle", "open", "read", "settle", "open", "drop 1"]);
  });

  it("ADR-0091 section 5: never goes while a re-entry record names its group", async () => {
    const p = port([REFUSED, REFUSED], { keepForReentry: (g) => g === "g" });
    await runHeldWelcomePass(p);
    expect(p.log.some((l) => l.startsWith("drop"))).toBe(false);
    expect(p.held()).toEqual([WELCOME]);
  });

  it("ADR-0091 section 5: joined as a re-entry, it goes like any join", async () => {
    const p = port([{ kind: "joined", entry: "back" }], { keepForReentry: () => true });
    expect(await runHeldWelcomePass(p)).toEqual(["back"]);
    expect(p.log).toEqual(["settle", "open", "drop 1"]);
  });

  it("stays when the read did not reach the head: a refused read proves nothing", async () => {
    const p = port([REFUSED, REFUSED], { readJournal: async () => false });
    await runHeldWelcomePass(p);
    expect(p.log.some((l) => l.startsWith("drop"))).toBe(false);
    expect(p.held()).toEqual([WELCOME]);
  });

  it("opens at once when the group is already gone", async () => {
    const p = port([{ kind: "joined", entry: "group" }]);
    expect(await runHeldWelcomePass(p)).toEqual(["group"]);
    expect(p.log).toEqual(["settle", "open", "drop 1"]);
  });

  it("goes when it fails for any other reason, and the person is told only of a real failure", async () => {
    const failed = port([{ kind: "failed" }]);
    await runHeldWelcomePass(failed);
    expect(failed.log).toEqual(["settle", "open", "failed", "drop 1"]);
    const notOurs = port([{ kind: "not-ours" }]);
    await runHeldWelcomePass(notOurs);
    expect(notOurs.log).toEqual(["settle", "open", "drop 1"]);
  });

  it("does nothing with none held", async () => {
    const p = port([], { held: () => [] });
    await runHeldWelcomePass(p);
    expect(p.log).toEqual([]);
  });

  it("stops when the session it was started for ended", async () => {
    let live = true;
    const p = port([REFUSED], {
      current: () => live,
      readJournal: async () => {
        live = false;
        return true;
      },
    });
    await runHeldWelcomePass(p);
    expect(p.log).toEqual(["settle", "open"]);
  });
});
