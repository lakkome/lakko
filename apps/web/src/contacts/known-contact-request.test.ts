// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.7 case 1 / UX 16.6 - `known-contact-request.ts`. */
import { describe, expect, it } from "vitest";

import {
  carriedContactSinceMs,
  contactSignedBy,
  establishedContactOfAccount,
  knownContactRequestVerdict,
  pinKeysOf,
  requestPredatesContact,
  SIBLING_ADMIT_WAIT_MS,
  siblingMayAdmit,
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
  it("base scenario 7: answered silently only while a one-to-one is held here, else listed", () => {
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: true })).toBe("answer");
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false })).toBe("show");
    // Dropping and waiting do not depend on it.
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, requestKeyB64: "old" })).toBe("drop");
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, check: "refused" })).toBe("drop");
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, check: "inconclusive" })).toBe("wait");
  });
  it("after a lost cursor: a request no newer than the contact is dropped, not listed", () => {
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, predatesContact: true })).toBe("drop");
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, predatesContact: false })).toBe("show");
    // With a chat held it is answered as before.
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: true, predatesContact: true })).toBe("answer");
  });
  it("a sibling may still admit this device: the request waits, then is listed", () => {
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, siblingMayAdmit: true })).toBe("wait");
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: false, siblingMayAdmit: false })).toBe("show");
    // Once the chat is here it is answered; an old one is still dropped.
    expect(knownContactRequestVerdict({ ...base, holdsOneToOne: true, siblingMayAdmit: true })).toBe("answer");
    expect(
      knownContactRequestVerdict({ ...base, holdsOneToOne: false, siblingMayAdmit: true, predatesContact: true }),
    ).toBe("drop");
  });
  it("siblingMayAdmit: another device, and the request younger than the wait", () => {
    const at = "2026-11-22T09:00:00Z";
    const t = Date.parse(at);
    expect(siblingMayAdmit({ otherDeviceCount: 1, requestCreatedAt: at, nowMs: t + 1000 })).toBe(true);
    expect(siblingMayAdmit({ otherDeviceCount: 1, requestCreatedAt: at, nowMs: t + SIBLING_ADMIT_WAIT_MS })).toBe(false);
    expect(siblingMayAdmit({ otherDeviceCount: 0, requestCreatedAt: at, nowMs: t + 1000 })).toBe(false);
    expect(siblingMayAdmit({ otherDeviceCount: null, requestCreatedAt: at, nowMs: t + 1000 })).toBe(false);
    expect(siblingMayAdmit({ otherDeviceCount: 2, requestCreatedAt: "bad", nowMs: t })).toBe(false);
  });
  it("requestPredatesContact compares the DS stamp with the relationship time", () => {
    const since = Date.parse("2026-11-22T09:00:00Z");
    expect(requestPredatesContact("2026-11-21T09:00:00Z", since)).toBe(true);
    expect(requestPredatesContact("2026-11-22T09:00:00Z", since)).toBe(true);
    expect(requestPredatesContact("2026-11-22T09:19:00Z", since)).toBe(false);
    expect(requestPredatesContact("not a date", since)).toBe(false);
    expect(requestPredatesContact("2026-11-21T09:00:00Z", undefined)).toBe(false);
    expect(requestPredatesContact("2026-11-21T09:00:00Z", 0)).toBe(false);
  });
  it("a contact carried to this device is one only since it arrived (seen live 2026-11-23)", () => {
    const acceptedElsewhere = Date.parse("2026-11-01T09:00:00Z");
    const arrived = Date.parse("2026-11-23T08:54:58Z");
    const since = carriedContactSinceMs(acceptedElsewhere, arrived);
    expect(since).toBe(arrived);
    // Re-add requests the account's other device answered before this one joined.
    expect(requestPredatesContact("2026-11-22T18:30:00Z", since)).toBe(true);
    expect(
      knownContactRequestVerdict({
        senderIsContact: true,
        check: "current",
        requestKeyB64: "K",
        trustedKeyB64: "K",
        holdsOneToOne: false,
        predatesContact: requestPredatesContact("2026-11-22T18:30:00Z", since),
        siblingMayAdmit: false,
      }),
    ).toBe("drop");
    // A request after the contact arrived here is a new one.
    expect(requestPredatesContact("2026-11-23T09:10:00Z", since)).toBe(false);
    // A clock behind the other device's acceptance never moves it back.
    expect(carriedContactSinceMs(acceptedElsewhere, 0)).toBe(acceptedElsewhere);
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

describe("contactSignedBy", () => {
  it("a row under the key that signed is that contact, with no account id on it", () => {
    const rows = [{ signaturePubkeyB64: "other" }, { signaturePubkeyB64: "signer" }];
    expect(contactSignedBy(rows, "signer")).toBe(rows[1]);
  });
  it("a row no pin or binding vouches for is still found by the signing key", () => {
    const rows = [{ signaturePubkeyB64: "signer", accountIdHex: "aabb" }];
    expect(establishedContactOfAccount(rows, "aabb", { pinKeysB64: [], bindingKeyB64: null })).toBeUndefined();
    expect(contactSignedBy(rows, "signer")).toBe(rows[0]);
  });
  it("another key finds nothing, so a claimed account id swallows no request", () => {
    const rows = [{ signaturePubkeyB64: "member-key", accountIdHex: "aabb" }];
    expect(contactSignedBy(rows, "victim-key")).toBeUndefined();
    expect(contactSignedBy(rows, "")).toBeUndefined();
  });
});
