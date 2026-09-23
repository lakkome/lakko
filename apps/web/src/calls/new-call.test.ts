// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  ACCOUNT_ID_LEN,
  NEW_CALL_RETURN_PATH,
  NewCallArgError,
  SIGNATURE_KEY_LEN,
  newCallId,
  newCallStart,
} from "./new-call";
import { CALL_ID_LEN } from "./invite";
import { paths } from "../routes/paths";

const owner = {
  accountId: new Uint8Array(ACCOUNT_ID_LEN).fill(3),
  signaturePublicKey: new Uint8Array(SIGNATURE_KEY_LEN).fill(9),
};

describe("newCallId", () => {
  it("is 16 bytes and comes from the source it was handed", () => {
    const id = newCallId((out) => out.fill(7));
    expect(id.length).toBe(CALL_ID_LEN);
    expect([...id]).toEqual(Array.from({ length: CALL_ID_LEN }, () => 7));
  });

  it("uses the platform random source when none is injected", () => {
    const a = newCallId();
    const b = newCallId();
    expect(a.length).toBe(CALL_ID_LEN);
    // Two 16-byte draws colliding is not something a test needs to tolerate.
    expect([...a]).not.toEqual([...b]);
  });
});

describe("newCallStart", () => {
  it("always starts one kind of call: group, video, owned here", () => {
    const callId = newCallId((out) => out.fill(1));
    const start = newCallStart(owner, callId);
    expect(start.mediaVideo).toBe(true);
    expect(start.isOwner).toBe(true);
    expect(start.callId).toBe(callId);
    expect(start.ownerAccountId).toBe(owner.accountId);
    expect(start.ownerSignatureKey).toBe(owner.signaturePublicKey);
  });

  it("returns to the destination the button was pressed on, never the design route", () => {
    const start = newCallStart(owner, newCallId((out) => out.fill(1)));
    expect(start.routePath).toBe(paths.calls);
    expect(NEW_CALL_RETURN_PATH).toBe(paths.calls);
    expect(start.routePath.startsWith("/call/")).toBe(false);
  });

  it("refuses an identity of the wrong shape rather than founding a call nobody can join", () => {
    const callId = newCallId((out) => out.fill(1));
    expect(() => newCallStart(owner, new Uint8Array(8))).toThrow(NewCallArgError);
    expect(() =>
      newCallStart({ ...owner, accountId: new Uint8Array(15) }, callId),
    ).toThrow(NewCallArgError);
    expect(() =>
      newCallStart({ ...owner, signaturePublicKey: new Uint8Array(31) }, callId),
    ).toThrow(NewCallArgError);
  });
});
