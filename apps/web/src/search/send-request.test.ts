// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { base64Encode } from "../auth/account-id";
import { toHex, type PersonResult } from "./people";
import { sendFoundRequest, type SendDeps } from "./send-request";

const KEY = new Uint8Array(32).fill(0xa1);
const X25519 = new Uint8Array(32).fill(0xa3);
const ACC = new Uint8Array(16).fill(0xa2);
const INTRO = new TextEncoder().encode("hi");

const found = (over: Partial<PersonResult> = {}): PersonResult => ({
  keyB64: base64Encode(KEY),
  keyHex: toHex(KEY),
  accountIdHex: toHex(ACC),
  name: "Anna Weber",
  username: null,
  reach: "grant",
  grantWireB64: "GRANT",
  ...over,
});

function deps(over: Partial<SendDeps> = {}): SendDeps & {
  sendByUsername: ReturnType<typeof vi.fn>;
  sendToBundle: ReturnType<typeof vi.fn>;
} {
  return {
    reachCheck: vi.fn(async () => true),
    identityFetch: vi.fn(async () => ({ signaturePub: KEY, x25519Pub: X25519 })),
    sendByUsername: vi.fn(async () => undefined),
    sendToBundle: vi.fn(async () => undefined),
    isRefusal: () => false,
    errorMessage: () => () => "could not send",
    ...over,
  } as never;
}

describe("sendFoundRequest", () => {
  it("checks the grant again, then sends through the bundle with that grant", async () => {
    const d = deps();
    expect(await sendFoundRequest(found(), INTRO, d)).toEqual({ kind: "sent" });
    expect(d.reachCheck).toHaveBeenCalledWith(KEY, "GRANT");
    expect(d.sendToBundle).toHaveBeenCalledWith(ACC, KEY, X25519, INTRO, "GRANT");
  });

  it("is Request refused, and sends nothing, when the person changed who can find them", async () => {
    const d = deps({ reachCheck: vi.fn(async () => false) });
    expect(await sendFoundRequest(found(), INTRO, d)).toEqual({ kind: "refused" });
    expect(d.sendToBundle).not.toHaveBeenCalled();
    expect(d.sendByUsername).not.toHaveBeenCalled();
  });

  it("sends nothing when the identity bundle publishes another key for the account", async () => {
    const d = deps({ identityFetch: vi.fn(async () => ({ signaturePub: new Uint8Array(32), x25519Pub: X25519 })) });
    expect(await sendFoundRequest(found(), INTRO, d)).toEqual({ kind: "refused" });
    expect(d.sendToBundle).not.toHaveBeenCalled();
  });

  it("a person found under a PROVEN predecessor key gets the request at their current keys (ADR-0074 3.5a)", async () => {
    const CURRENT = new Uint8Array(32).fill(0xb7);
    const provenPredecessor = vi.fn(async () => true);
    const d = deps({
      identityFetch: vi.fn(async () => ({ signaturePub: CURRENT, x25519Pub: X25519 })),
      provenPredecessor,
    });
    expect(await sendFoundRequest(found(), INTRO, d)).toEqual({ kind: "sent" });
    expect(provenPredecessor).toHaveBeenCalledWith(KEY, ACC);
    // Sealed to the published key, never to the key the hit was found under.
    expect(d.sendToBundle).toHaveBeenCalledWith(ACC, CURRENT, X25519, INTRO, "GRANT");
  });

  it("an UNPROVEN mismatch is still refused", async () => {
    const d = deps({
      identityFetch: vi.fn(async () => ({ signaturePub: new Uint8Array(32).fill(0xb7), x25519Pub: X25519 })),
      provenPredecessor: vi.fn(async () => false),
    });
    expect(await sendFoundRequest(found(), INTRO, d)).toEqual({ kind: "refused" });
    expect(d.sendToBundle).not.toHaveBeenCalled();
  });

  it("sends to a person in Everyone by their verified username, with no check", async () => {
    const d = deps();
    const person = found({ reach: "open", username: "anna", grantWireB64: null });
    expect(await sendFoundRequest(person, INTRO, d)).toEqual({ kind: "sent" });
    expect(d.reachCheck).not.toHaveBeenCalled();
    expect(d.sendByUsername).toHaveBeenCalledWith("anna", INTRO, null);
  });

  it("is refused when the username no longer resolves (they left Everyone)", async () => {
    const d = deps({
      sendByUsername: vi.fn(async () => {
        throw new Error("username-unknown");
      }),
      isRefusal: () => true,
    });
    const person = found({ reach: "open", username: "anna", grantWireB64: null });
    expect(await sendFoundRequest(person, INTRO, d)).toEqual({ kind: "refused" });
  });

  it("reports any other failure as a failure", async () => {
    const d = deps({
      sendToBundle: vi.fn(async () => {
        throw new Error("offline");
      }),
    });
    const out = await sendFoundRequest(found(), INTRO, d);
    expect(out.kind).toBe("failed");
    expect(out.kind === "failed" ? out.message() : null).toBe("could not send");
  });

  it("offers nothing for a contact or a hidden person", async () => {
    const d = deps();
    expect(await sendFoundRequest(found({ reach: "contact", grantWireB64: null }), INTRO, d)).toEqual({ kind: "refused" });
    expect(await sendFoundRequest(found({ reach: "none", grantWireB64: null }), INTRO, d)).toEqual({ kind: "refused" });
    expect(d.sendToBundle).not.toHaveBeenCalled();
  });
});
