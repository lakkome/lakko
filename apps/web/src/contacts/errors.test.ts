// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The Add-by-username "not found" copy (`docs/ux/who-can-find-me.md` section 6.1). */
import { describe, expect, it } from "vitest";

import { ContactsError } from "./client";
import { formatContactsError } from "./errors";

describe("formatContactsError", () => {
  it("a name that does not resolve reads as not found, never as unregistered", () => {
    const message = formatContactsError(ContactsError.usernameUnknown("anna"), "contacts.error.send");
    expect(message).toBe("No one found. Check the spelling, or ask them for an invite link.");
    // The searcher must not learn whether the account exists (ADR-0060).
    expect(message).not.toContain("registered");
    expect(message).not.toContain("@anna");
  });
});
