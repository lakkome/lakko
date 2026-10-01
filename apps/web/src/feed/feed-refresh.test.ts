// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { INITIAL_REFRESH_STATE, outcomeOf, recordCheck, refreshFailed } from "./feed-refresh";

describe("feed refresh banner rule", () => {
  it("classifies a run", () => {
    expect(outcomeOf({ tried: 0, answered: 0 })).toBeNull();
    expect(outcomeOf({ tried: 3, answered: 0 })).toBe("nothing");
    expect(outcomeOf({ tried: 3, answered: 1 })).toBe("reached");
  });

  it("is off before any check", () => {
    expect(refreshFailed(INITIAL_REFRESH_STATE)).toBe(false);
  });

  it("shows when nothing of either kind answered", () => {
    let s = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 2, answered: 0 });
    s = recordCheck(s, "channels", { tried: 1, answered: 0 });
    expect(refreshFailed(s)).toBe(true);
  });

  it("shows when the only kind with sources reached nothing", () => {
    const s = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 2, answered: 0 });
    expect(refreshFailed(s)).toBe(true);
  });

  it("a check where some answered is not a failure", () => {
    const s = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 3, answered: 1 });
    expect(refreshFailed(s)).toBe(false);
  });

  it("channels reached means no banner even if groups reached nothing", () => {
    let s = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 2, answered: 0 });
    s = recordCheck(s, "channels", { tried: 1, answered: 1 });
    expect(refreshFailed(s)).toBe(false);
  });

  it("goes at the next check that reaches anything", () => {
    let s = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 2, answered: 0 });
    expect(refreshFailed(s)).toBe(true);
    s = recordCheck(s, "groups", { tried: 2, answered: 1 });
    expect(refreshFailed(s)).toBe(false);
  });

  it("a run that tried nothing changes nothing", () => {
    const failed = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 2, answered: 0 });
    expect(recordCheck(failed, "groups", { tried: 0, answered: 0 })).toBe(failed);
    const ok = recordCheck(INITIAL_REFRESH_STATE, "groups", { tried: 2, answered: 2 });
    expect(recordCheck(ok, "groups", { tried: 0, answered: 0 })).toBe(ok);
  });
});
