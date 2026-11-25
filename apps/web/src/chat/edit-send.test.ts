// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { sendEdit, type EditSendPort } from "./edit-send";

function harness() {
  const drawn = new Map<number, { text: string; opId: number | undefined }>();
  const notSent = new Map<number, string>();
  const pending: { resolve: (id: number) => void; reject: (e: Error) => void }[] = [];
  const port: EditSendPort = {
    apply: (target, text, opId) => drawn.set(target, { text, opId }),
    drawnText: (target) => drawn.get(target)?.text,
    publish: () =>
      new Promise<number>((resolve, reject) => {
        pending.push({ resolve, reject });
      }),
    markNotSent: (target, text) => notSent.set(target, text),
    clearNotSent: (target) => notSent.delete(target),
  };
  return {
    port,
    drawn,
    notSent,
    /** Settle the `n`th publish (0 = the first), the newest by default. */
    resolve: (id: number, n = pending.length - 1) => pending[n]?.resolve(id),
    reject: (n = pending.length - 1) => pending[n]?.reject(new Error("offline")),
  };
}

describe("sendEdit", () => {
  it("draws the new text before the publish resolves", async () => {
    const h = harness();
    const done = sendEdit(h.port, 7, "new text");
    expect(h.drawn.get(7)).toEqual({ text: "new text", opId: undefined });
    h.resolve(42);
    await expect(done).resolves.toBe(true);
    expect(h.drawn.get(7)).toEqual({ text: "new text", opId: 42 });
    expect(h.notSent.size).toBe(0);
  });

  it("keeps the edited text and says Not sent when the publish fails", async () => {
    const h = harness();
    const done = sendEdit(h.port, 7, "new text");
    h.reject();
    await expect(done).resolves.toBe(false);
    expect(h.drawn.get(7)?.text).toBe("new text");
    expect(h.notSent.get(7)).toBe("new text");
  });

  it("does not give an older edit's id or failure to a newer edit", async () => {
    const h = harness();
    const first = sendEdit(h.port, 7, "first");
    const second = sendEdit(h.port, 7, "second");
    h.reject(0);
    await first;
    expect(h.notSent.size).toBe(0);
    h.resolve(50, 1);
    await second;
    expect(h.drawn.get(7)).toEqual({ text: "second", opId: 50 });
  });

  it("clears Not sent when the edit is sent again", async () => {
    const h = harness();
    const first = sendEdit(h.port, 7, "t");
    h.reject();
    await first;
    expect(h.notSent.has(7)).toBe(true);
    const again = sendEdit(h.port, 7, "t");
    expect(h.notSent.has(7)).toBe(false);
    h.resolve(9);
    await expect(again).resolves.toBe(true);
  });
});
