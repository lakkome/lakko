// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { filterInviteContactRows, inviteContactRows } from "./invite-contacts";

const contacts = [
  { signaturePubkeyB64: "k-zoe", name: "Zoe", username: "zoe_k" },
  { signaturePubkeyB64: "k-anna", name: "Anna", username: null },
  { signaturePubkeyB64: "k-boris", name: "boris", username: "b0ris" },
];

const rows = () =>
  inviteContactRows(
    contacts,
    (c) => c.name,
    (c) => c.username,
  );

describe("inviteContactRows", () => {
  it("lists every contact, by name", () => {
    expect(rows().map((r) => r.name)).toEqual(["Anna", "boris", "Zoe"]);
  });

  it("carries the key the invitation is addressed to, and the username when known", () => {
    expect(rows()[0]).toEqual({ keyB64: "k-anna", name: "Anna", username: null });
    expect(rows()[2]).toEqual({ keyB64: "k-zoe", name: "Zoe", username: "zoe_k" });
  });

  it("lists a contact once even if the record is doubled", () => {
    const anna = { signaturePubkeyB64: "k-anna", name: "Anna", username: null };
    const doubled = inviteContactRows(
      [anna, anna],
      (c) => c.name,
      (c) => c.username,
    );
    expect(doubled).toHaveLength(1);
  });

  it("is empty for a person with no contacts", () => {
    expect(inviteContactRows([], () => "", () => null)).toEqual([]);
  });
});

describe("filterInviteContactRows", () => {
  it("keeps everything for an empty query", () => {
    expect(filterInviteContactRows(rows(), "  ")).toHaveLength(3);
  });

  it("matches by name or by username, ignoring case and a typed @", () => {
    expect(filterInviteContactRows(rows(), "ann").map((r) => r.keyB64)).toEqual(["k-anna"]);
    expect(filterInviteContactRows(rows(), "@ZOE_").map((r) => r.keyB64)).toEqual(["k-zoe"]);
    expect(filterInviteContactRows(rows(), "b0").map((r) => r.keyB64)).toEqual(["k-boris"]);
  });

  it("keeps nothing when nobody matches", () => {
    expect(filterInviteContactRows(rows(), "nobody")).toEqual([]);
  });
});
