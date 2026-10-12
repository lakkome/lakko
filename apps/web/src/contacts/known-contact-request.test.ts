// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.7 case 1 / UX 16.6 - `known-contact-request.ts`. */
import { describe, expect, it } from "vitest";

import {
  establishedContactOfAccount,
  knownContactRequestVerdict,
  pinKeysOf,
} from "./known-contact-request";

describe("knownContactRequestVerdict", () => {
  const base = { senderIsContact: true, check: "current" as const, requestKeyB64: "new", trustedKeyB64: "new" };
  it("a stranger's request is the ordinary request row", () => {
    expect(knownContactRequestVerdict({ ...base, senderIsContact: false })).toBe("show");
    expect(
      knownContactRequestVerdict({ ...base, senderIsContact: false, check: "refused", trustedKeyB64: null }),
    ).toBe("show");
  });
  it("a contact's request from the key this device trusts is answered silently", () => {
    expect(knownContactRequestVerdict(base)).toBe("answer");
  });
  it("a refused change is dropped: nothing shown, nothing sent", () => {
    expect(knownContactRequestVerdict({ ...base, check: "refused", trustedKeyB64: "old" })).toBe("drop");
  });
  it("no verdict yet: it waits, unshown", () => {
    expect(knownContactRequestVerdict({ ...base, check: "inconclusive", trustedKeyB64: "old" })).toBe("wait");
  });
  it("a request from any other key of a contact's account is dropped", () => {
    expect(knownContactRequestVerdict({ ...base, requestKeyB64: "old" })).toBe("drop");
    expect(knownContactRequestVerdict({ ...base, trustedKeyB64: null })).toBe("drop");
  });
});

describe("establishedContactOfAccount", () => {
  const none = { pinKeysB64: [], bindingKeyB64: null };
  it("a row whose key the pin names is an established contact", () => {
    const rows = [{ signaturePubkeyB64: "real", accountIdHex: "aabb" }];
    expect(establishedContactOfAccount(rows, "AABB", { pinKeysB64: ["real"], bindingKeyB64: null })).toBe(rows[0]);
  });
  it("a row whose key a verified binding names is an established contact", () => {
    const rows = [{ signaturePubkeyB64: "real", accountIdHex: "aabb" }];
    expect(establishedContactOfAccount(rows, "aabb", { pinKeysB64: [], bindingKeyB64: "real" })).toBe(rows[0]);
  });
  it("a row with neither is not: the account id may be the peer's own claim", () => {
    const rows = [{ signaturePubkeyB64: "real", accountIdHex: "aabb" }];
    expect(establishedContactOfAccount(rows, "aabb", none)).toBeUndefined();
  });
  it("is undefined for an unknown or missing id", () => {
    const rows = [{ signaturePubkeyB64: "k", accountIdHex: "aabb" }];
    expect(establishedContactOfAccount(rows, "ccdd", { pinKeysB64: ["k"], bindingKeyB64: null })).toBeUndefined();
    expect(establishedContactOfAccount(rows, null, { pinKeysB64: ["k"], bindingKeyB64: null })).toBeUndefined();
  });

  it("a member who claimed another account's id does not swallow that account's later request", () => {
    // The member's row carries the victim's account id from the member's own
    // profile. Nothing this device verified names the member's key for it.
    const rows = [{ signaturePubkeyB64: "member-key", accountIdHex: "aabb" }];
    // No pin and no binding for the victim's account: a stranger.
    const before = establishedContactOfAccount(rows, "aabb", none);
    expect(before).toBeUndefined();
    expect(
      knownContactRequestVerdict({
        senderIsContact: before !== undefined,
        check: "current",
        requestKeyB64: "victim-key",
        trustedKeyB64: "victim-key",
      }),
    ).toBe("show");
    // The same when this device already holds the victim's real key (a pin
    // from any earlier fetch): it names the victim's key, not the member's.
    const pinned = establishedContactOfAccount(rows, "aabb", { pinKeysB64: ["victim-key"], bindingKeyB64: null });
    expect(pinned).toBeUndefined();
  });
  it("after a proven rotation the row's kept key is still named by the pin", () => {
    const rows = [{ signaturePubkeyB64: "old", accountIdHex: "aabb" }];
    const keys = pinKeysOf({ currentKeyB64: "new", provenPredecessorsB64: ["old"] });
    expect(establishedContactOfAccount(rows, "aabb", { pinKeysB64: keys, bindingKeyB64: null })).toBe(rows[0]);
    expect(pinKeysOf(undefined)).toEqual([]);
  });
  it("a claimed row does not hide the real contact row of the same account", () => {
    const rows = [
      { signaturePubkeyB64: "member-key", accountIdHex: "aabb" },
      { signaturePubkeyB64: "victim-key", accountIdHex: "aabb" },
    ];
    expect(establishedContactOfAccount(rows, "aabb", { pinKeysB64: ["victim-key"], bindingKeyB64: null })).toBe(rows[1]);
  });
});
