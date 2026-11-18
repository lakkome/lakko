// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { filterContacts } from "./contact-filter";

const list = [
  { name: "Anna Berg", username: "anna", accountKeyB64: "a" },
  { name: "Ivan", accountKeyB64: "b" },
];

describe("contact picker filter", () => {
  it("matches name and username, with or without @", () => {
    expect(filterContacts(list, "berg").map((c) => c.accountKeyB64)).toEqual(["a"]);
    expect(filterContacts(list, "@ann").map((c) => c.accountKeyB64)).toEqual(["a"]);
    expect(filterContacts(list, "IVAN").map((c) => c.accountKeyB64)).toEqual(["b"]);
    expect(filterContacts(list, " ")).toHaveLength(2);
  });
});
