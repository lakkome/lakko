// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { decodeDraft, encodeDraft } from "./article-drafts";

describe("article drafts (UX 11.1)", () => {
  it("round-trips the article and its images", async () => {
    const article = { title: "T", blocks: [{ kind: "paragraph" as const, runs: [{ kind: "text" as const, bold: false, italic: false, text: "x" }] }] };
    const stored = JSON.parse(JSON.stringify(encodeDraft(article, [{ mime: "image/jpeg", bytes: new Uint8Array([1, 2, 3]) }])));
    const back = decodeDraft(stored);
    expect(back?.article).toEqual(article);
    expect(back?.images).toHaveLength(1);
    expect(Array.from(new Uint8Array(await back!.images[0]!.arrayBuffer()))).toEqual([1, 2, 3]);
    expect(back?.images[0]?.type).toBe("image/jpeg");
  });

  it("reads anything else, a deleted row included, as no draft", () => {
    expect(decodeDraft(null)).toBeNull();
    expect(decodeDraft({ v: 2 })).toBeNull();
    expect(decodeDraft("x")).toBeNull();
  });
});
