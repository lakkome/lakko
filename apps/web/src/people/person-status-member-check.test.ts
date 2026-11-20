// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 2 — a group member who is not a contact is asked about
 * from their profile's account id, and a member row drawn before that id was
 * known is asked about once it is, while the row is still in view.
 */
import { afterEach, describe, expect, it } from "vitest";

import { personAccountIdHex, type PersonAccountIdSources } from "./person-account-id";
import { checkWhenVisible, installPersonChecker, recheckVisiblePeople, visiblePeople } from "./person-status";

const MEMBER = "bWVtYmVyLWFjY291bnQta2V5LTMyLWJ5dGVzLWxvbmc=";
const PROFILE_ID = "0123456789abcdef0123456789abcdef";

function sources(over: Partial<PersonAccountIdSources> = {}): PersonAccountIdSources {
  return {
    contactIdB64: () => null,
    profileIdHex: () => null,
    pendingIdHex: () => null,
    recordIdHex: () => null,
    decodeB64: (b64) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)),
    ...over,
  };
}

const ROW = {} as Element;

afterEach(() => {
  installPersonChecker(null);
});

describe("the account id of a group member who is not a contact", () => {
  it("comes from the member's own profile", () => {
    expect(personAccountIdHex(MEMBER, sources({ profileIdHex: () => PROFILE_ID }))).toBe(PROFILE_ID);
  });

  it("a contact record wins over the profile claim", () => {
    const contactId = btoa(String.fromCharCode(...new Array<number>(16).fill(0xab)));
    expect(
      personAccountIdHex(MEMBER, sources({ contactIdB64: () => contactId, profileIdHex: () => PROFILE_ID })),
    ).toBe("ab".repeat(16));
  });

  it("is unknown with no contact, no profile id and no record", () => {
    expect(personAccountIdHex(MEMBER, sources())).toBeNull();
  });

  it("a malformed value falls through to the next source", () => {
    expect(
      personAccountIdHex(
        MEMBER,
        sources({ contactIdB64: () => "!!", profileIdHex: () => "short", recordIdHex: () => PROFILE_ID }),
      ),
    ).toBe(PROFILE_ID);
  });
});

describe("a member row in view", () => {
  it("is asked about when drawn and again when an account id is learned", () => {
    const asked: string[] = [];
    installPersonChecker((k) => asked.push(k));
    const cleanup = checkWhenVisible(ROW, () => MEMBER);
    expect(asked).toEqual([MEMBER]);
    expect(visiblePeople()).toEqual([MEMBER]);
    recheckVisiblePeople();
    expect(asked).toEqual([MEMBER, MEMBER]);
    cleanup();
    expect(visiblePeople()).toEqual([]);
    recheckVisiblePeople();
    expect(asked).toEqual([MEMBER, MEMBER]);
  });

  it("two rows of one person are one person in view until both are gone", () => {
    const asked: string[] = [];
    installPersonChecker((k) => asked.push(k));
    const a = checkWhenVisible(ROW, () => MEMBER);
    const b = checkWhenVisible(ROW, () => MEMBER);
    expect(visiblePeople()).toEqual([MEMBER]);
    a();
    expect(visiblePeople()).toEqual([MEMBER]);
    b();
    expect(visiblePeople()).toEqual([]);
  });
});
