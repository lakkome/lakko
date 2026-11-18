// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { dropIndex, remapIndex } from "./drag-reorder";
import { movedTo as moved } from "./poll-draft";

const rows = [
  { top: 0, bottom: 40 },
  { top: 40, bottom: 80 },
  { top: 80, bottom: 120 },
  { top: 120, bottom: 160 },
];

describe("drag to reorder (UX 7.1, 8.1)", () => {
  it("lands where the pointer passed a row's middle", () => {
    expect(dropIndex(5, rows, 3)).toBe(0);
    expect(dropIndex(61, rows, 3)).toBe(1);
    expect(dropIndex(150, rows, 3)).toBe(3);
  });

  it("never lands past the last movable row (the trailing empty field)", () => {
    expect(dropIndex(150, rows, 2)).toBe(2);
    expect(dropIndex(-100, rows, 2)).toBe(0);
  });

  it("keeps another index (the quiz's correct answer) on the same option", () => {
    const list = ["a", "b", "c", "d"];
    for (let from = 0; from < 4; from++) {
      for (let to = 0; to < 4; to++) {
        const after = moved(list, from, to);
        for (let i = 0; i < 4; i++) expect(after[remapIndex(i, from, to)]).toBe(list[i]);
      }
    }
  });
});
