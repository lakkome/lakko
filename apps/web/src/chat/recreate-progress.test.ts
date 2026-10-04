// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  RECREATE_IDLE,
  recreateProgressReduce as reduce,
  recreateProgressView as view,
  type RecreateProgress,
} from "./recreate-progress";

const inviting: RecreateProgress = { kind: "inviting", groupIdB64: "new" };

describe("recreate sheet progress (UX 16.5)", () => {
  it("is Creating... over the new group once handed off, and only there", () => {
    const s = reduce(RECREATE_IDLE, { kind: "handed-off", groupIdB64: "new" });
    expect(s).toEqual(inviting);
    expect(view(s, "new")).toEqual({ open: true, working: true, failedNames: null });
    expect(view(s, "old")).toEqual({ open: false, working: false, failedNames: null });
    expect(view(RECREATE_IDLE, "new").open).toBe(false);
  });

  it("cannot be dismissed while working", () => {
    expect(reduce(inviting, { kind: "dismissed" })).toBe(inviting);
  });

  it("closes on its own when every invitation landed", () => {
    expect(reduce(inviting, { kind: "adds-finished", groupIdB64: "new", failed: [] })).toEqual(
      RECREATE_IDLE,
    );
  });

  it("stays with the names when some invitations failed, until Done", () => {
    const s = reduce(inviting, { kind: "adds-finished", groupIdB64: "new", failed: ["Ann", "Bo"] });
    expect(s).toEqual({ kind: "some-failed", groupIdB64: "new", names: ["Ann", "Bo"] });
    expect(view(s, "new")).toEqual({ open: true, working: false, failedNames: ["Ann", "Bo"] });
    expect(reduce(s, { kind: "dismissed" })).toEqual(RECREATE_IDLE);
  });

  it("ignores a report for another group, or one nobody is waiting on", () => {
    expect(reduce(inviting, { kind: "adds-finished", groupIdB64: "other", failed: ["Ann"] })).toBe(
      inviting,
    );
    expect(
      reduce(RECREATE_IDLE, { kind: "adds-finished", groupIdB64: "new", failed: ["Ann"] }),
    ).toBe(RECREATE_IDLE);
    const failed: RecreateProgress = { kind: "some-failed", groupIdB64: "new", names: ["Ann"] };
    expect(reduce(failed, { kind: "adds-finished", groupIdB64: "new", failed: [] })).toBe(failed);
  });

  it("a second recreate replaces whatever the sheet held", () => {
    const failed: RecreateProgress = { kind: "some-failed", groupIdB64: "new", names: ["Ann"] };
    expect(reduce(failed, { kind: "handed-off", groupIdB64: "newer" })).toEqual({
      kind: "inviting",
      groupIdB64: "newer",
    });
  });
});
