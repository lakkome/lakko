// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { memberRowActions, rowRoleOf, type GroupRole } from "./member-row-actions";

const roles: readonly GroupRole[] = ["owner", "admin", "member"];

function row(viewerRole: GroupRole | null, rowRole: GroupRole, ownRow = false, membershipEnded = false) {
  return memberRowActions({ viewerRole, rowRole, ownRow, membershipEnded });
}

describe("memberRowActions", () => {
  it("never offers anything on the owner's row", () => {
    for (const viewer of [...roles, null]) {
      const r = row(viewer, "owner");
      expect(r.remove).toBe(viewer === null);
      expect(r.adminToggle).toBeNull();
    }
  });

  it("an admin removes plain members only and never changes roles (seen live 2026-11-22)", () => {
    expect(row("admin", "owner")).toEqual({ mark: "owner", remove: false, adminToggle: null });
    expect(row("admin", "admin")).toEqual({ mark: "admin", remove: false, adminToggle: null });
    expect(row("admin", "member")).toEqual({ mark: null, remove: true, adminToggle: null });
  });

  it("the owner removes anyone else and makes or takes back admins", () => {
    expect(row("owner", "admin")).toEqual({ mark: "admin", remove: true, adminToggle: "take" });
    expect(row("owner", "member")).toEqual({ mark: null, remove: true, adminToggle: "make" });
  });

  it("a plain member acts on no row but sees the marks", () => {
    for (const r of roles) {
      const out = row("member", r);
      expect(out.remove).toBe(false);
      expect(out.adminToggle).toBeNull();
      expect(out.mark).toBe(r === "member" ? null : r);
    }
  });

  it("nobody acts on their own row, and the own row keeps its mark", () => {
    for (const viewer of roles) {
      const out = row(viewer, viewer, true);
      expect(out.remove).toBe(false);
      expect(out.adminToggle).toBeNull();
      expect(out.mark).toBe(viewer === "member" ? null : viewer);
    }
    expect(row(null, "member", true).remove).toBe(false);
  });

  it("a group with no role table: anyone removes anyone else, no marks, no toggle", () => {
    for (const r of roles) {
      expect(row(null, r)).toEqual({ mark: null, remove: true, adminToggle: null });
    }
  });

  it("an ended membership shows a read-only roster without role marks", () => {
    for (const viewer of [...roles, null]) {
      for (const r of roles) {
        expect(row(viewer, r, false, true)).toEqual({ mark: null, remove: false, adminToggle: null });
      }
    }
  });
});

describe("rowRoleOf", () => {
  it("reads the owner, the admins and everyone else", () => {
    const admins = new Set(["bb"]);
    expect(rowRoleOf("aa", "aa", admins)).toBe("owner");
    expect(rowRoleOf("bb", "aa", admins)).toBe("admin");
    expect(rowRoleOf("cc", "aa", admins)).toBe("member");
    expect(rowRoleOf("aa", null, admins)).toBe("member");
  });
});
