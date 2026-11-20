// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Handle replacements never overlap (`handle-replacements.ts`). */
import { describe, expect, it } from "vitest";

import { createReplacementQueue } from "./handle-replacements";

describe("handle replacements", () => {
  it("a second replacement starts only after the first finished, even when the first fails", async () => {
    const queue = createReplacementQueue();
    const events: string[] = [];
    let releaseFirst: () => void = () => undefined;
    const first = queue(
      () =>
        new Promise<void>((_resolve, reject) => {
          events.push("first started");
          releaseFirst = () => {
            events.push("first ended");
            reject(new Error("reload failed"));
          };
        }),
    );
    const second = queue(async () => {
      events.push("second started");
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(events).toEqual(["first started"]);
    releaseFirst();
    await expect(first).rejects.toThrow("reload failed");
    await second;
    expect(events).toEqual(["first started", "first ended", "second started"]);
  });
});
