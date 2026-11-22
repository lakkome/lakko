// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `public/sw.js` tells a HIDDEN tab about a push wake only after its own
 * background read.
 *
 * The wake un-parks a hidden tab's receive sweep, which fetches the same
 * journal rows the worker's read fetches. A row the tab applied first cannot
 * be decrypted a second time by the read (its key is gone), so a message
 * from someone else notified nothing. Seen on Android on 2026-11-22, where the
 * open chat's poll raced the push read the same way.
 *
 * `sw.js` may run as a classic script and cannot be imported: `handlePush` is
 * cut out of the source and evaluated against fakes, so what is tested is the
 * code that ships.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const SW = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../public/sw.js"), "utf8");

function sliceFunction(prefix: string, name: string): string {
  const start = SW.indexOf(`${prefix} ${name}(`);
  if (start < 0) throw new Error(`${name} not found in sw.js`);
  const end = SW.indexOf("\n}\n", start);
  return SW.slice(start, end + 2);
}

interface Tab {
  visibilityState: "visible" | "hidden";
  postMessage: (m: unknown) => void;
}

function load(tabs: Tab[], events: string[], preview: unknown): () => Promise<void> {
  const wake = { groupHex: "ab".repeat(32), pointer: 20, isCall: false };
  const registration = {
    showNotification: async (title: string): Promise<void> => {
      events.push(`notify:${title}`);
    },
  };
  const make = new Function(
    "self",
    "admitWake",
    "runningWindows",
    "backgroundPreview",
    "handleCallNotice",
    "readNotificationStrings",
    "storedIncomingCallWord",
    "storedNewMessageWord",
    "console",
    `${sliceFunction("function", "genericReasonLine")}\n${sliceFunction("async function", "handlePush")}\nreturn handlePush;`,
  ) as (...a: unknown[]) => (w: unknown) => Promise<void>;
  const handlePush = make(
    { registration },
    async () => ({ accountId: "acct", groupHex: "cd".repeat(32) }),
    async () => tabs,
    async () => {
      events.push("read");
      return { preview, showGeneric: true, reason: "a row did not decrypt and the app does not hold it" };
    },
    async () => undefined,
    async () => null,
    () => "Incoming call",
    () => "New message",
    { info: (line: string) => events.push(`log:${line}`) },
  );
  return () => handlePush(wake);
}

describe("sw.js push wake and open tabs", () => {
  it("tells a hidden tab only after the background read", async () => {
    const events: string[] = [];
    const tab: Tab = { visibilityState: "hidden", postMessage: () => events.push("hidden-tab") };
    await load([tab], events, { title: "Call walk", body: "Mira: hi", tag: "t" })();

    expect(events.indexOf("read")).toBeGreaterThanOrEqual(0);
    expect(events.indexOf("hidden-tab")).toBeGreaterThan(events.indexOf("read"));
  });

  it("tells a visible tab at once", async () => {
    const events: string[] = [];
    const tab: Tab = { visibilityState: "visible", postMessage: () => events.push("visible-tab") };
    await load([tab], events, { title: "Call walk", body: "Mira: hi", tag: "t" })();

    expect(events.indexOf("visible-tab")).toBeLessThan(events.indexOf("read"));
  });

  it("logs why a generic notification is shown, with no identifier", async () => {
    const events: string[] = [];
    await load([], events, null)();

    expect(events).toContain("log:[push] generic notification: a row did not decrypt and the app does not hold it");
    expect(events).toContain("notify:Lakko");
  });
});
