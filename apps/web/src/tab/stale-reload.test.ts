// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** docs/ux/one-tab.md section 6 — a stale refusal reloads once, never in a loop. */
import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import { STALE_RELOAD_KEY, STALE_RELOAD_WINDOW_MS, staleReaction } from "./stale-reload";

describe("staleReaction", () => {
  it("reloads the first time and stays when the next refusal follows that reload", () => {
    const s = new MemoryStorage();
    const t0 = 1_000_000;
    expect(staleReaction(s, t0)).toBe("reload");
    // The reloaded page is refused again 30 s later (the live loop).
    expect(staleReaction(s, t0 + 30_000)).toBe("stay");
    expect(staleReaction(s, t0 + 60_000)).toBe("stay");
  });

  it("reloads again once the window has passed", () => {
    const s = new MemoryStorage();
    expect(staleReaction(s, 0)).toBe("reload");
    expect(staleReaction(s, STALE_RELOAD_WINDOW_MS)).toBe("reload");
  });

  it("a mark from the future or one that does not parse does not hold the page", () => {
    const s = new MemoryStorage();
    s.setItem(STALE_RELOAD_KEY, "not a time");
    expect(staleReaction(s, 5_000)).toBe("reload");
    s.setItem(STALE_RELOAD_KEY, String(10_000_000));
    expect(staleReaction(s, 5_000)).toBe("reload");
  });

  it("no storage, or a storage that refuses the mark: stays (the side that cannot loop)", () => {
    expect(staleReaction(null, 1)).toBe("stay");
    const refusing = {
      getItem: (): string | null => null,
      setItem: (): void => {
        throw new Error("quota");
      },
    };
    expect(staleReaction(refusing, 1)).toBe("stay");
  });
});
