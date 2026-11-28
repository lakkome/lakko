// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The Add-by-username "not found" copy (`docs/ux/who-can-find-me.md` section 6.1). */
import { describe, expect, it } from "vitest";

import { ContactsError, ContactStartedOver } from "./client";
import { formatContactsError } from "./errors";

describe("formatContactsError", () => {
  it("a name that does not resolve reads as not found, never as unregistered", () => {
    const message = formatContactsError(ContactsError.usernameUnknown("anna"), "contacts.error.send");
    expect(message).toBe("No one found. Check the spelling, or ask them for an invite link.");
    // The searcher must not learn whether the account exists (ADR-0060).
    expect(message).not.toContain("registered");
    expect(message).not.toContain("@anna");
  });

  it("ADR-0101 section 11: an action toward a person who started over says nothing was sent", () => {
    // Not the offline line (nothing is wrong with the connection) and no
    // alarm: the plain "could not confirm" of an unconfirmed change.
    const started = new ContactStartedOver("YWNjb3VudC1pZA==");
    expect(formatContactsError(started, "contacts.error.send", "Anna")).toBe(
      "Couldn't confirm this is still Anna's account, so nothing was sent.",
    );
    expect(formatContactsError(started, "contacts.error.send")).toBe(
      "Couldn't confirm this is still the same account, so nothing was sent.",
    );
    // Also when it arrives wrapped.
    expect(
      formatContactsError(new Error("accept failed", { cause: started }), "contacts.error.send", "Anna"),
    ).toBe("Couldn't confirm this is still Anna's account, so nothing was sent.");
  });
});
