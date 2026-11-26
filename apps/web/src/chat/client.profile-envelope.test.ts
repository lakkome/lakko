// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 — the FOREGROUND receive loop lifts a profile envelope out of the
 * timeline, binds it to the sender's signature key resolved at receipt (C-68),
 * and never renders it — not even a malformed one.
 *
 * Same posture as `client.service-envelopes.test.ts`: the REAL wasm core
 * composes and parses every envelope (`composeProfileEnvelope`,
 * `decodeProfileEnvelope`, `parseBodyEnvelope`); only the DS and the MLS group
 * are stand-ins, and neither has an opinion about plaintext.
 *
 * ## Discrimination (verified by running the broken variant)
 *
 * With the profile arm removed from `toEvent`, six cases fail: "a profile is
 * classified as a profile" with `expected 'application' to be 'profile'`, "a
 * profile never reaches the timeline as text" with the bubble text
 * `lakko/profile/v1…` containing the sender's name, "a malformed profile is
 * dropped, not rendered", and the mixed batch. With `markOwnPublished` removed
 * from `sealAndPublish`, "the chat route's client drops a profile the publisher
 * client sent" fails with `expected [ { kind: 'application', … } ] to have a
 * length of +0 but got 1`.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { ChatClient, type IncomingChatEvent } from "./client";
import type { DsClient } from "../ds/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
let wasm: typeof wasmNs;

/** ADR-0101 section 8: a v6 profile envelope is signed by an account
 *  identity; this test's profiles are signed by one fixed identity. */
let profileSignerHeld: WasmIdentity | undefined;
function profileSigner(): WasmIdentity {
  return (profileSignerHeld ??= wasm.Identity.fromSeed(new Uint8Array(64).fill(0x5a)));
}

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  wasm = wasmNs;
});

const SENDER_KEY = new Uint8Array(32).fill(0x7e);
const OTHER_KEY = new Uint8Array(32).fill(0x11);

/** A group that hands each payload back as an Application frame from leaf 3,
 *  with a roster in which leaf 3 is SENDER_KEY. */
function group(groupId: Uint8Array, senderLeaf = 3): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    members: () => [
      { leafIndex: 0, signaturePublicKey: OTHER_KEY },
      { leafIndex: 3, signaturePublicKey: SENDER_KEY },
    ],
    processIncomingVerifyingRoles: vi.fn((_id: unknown, payload: Uint8Array) => ({
      kind: "Application",
      applicationData: payload,
      senderLeaf,
    })),
  } as unknown as WasmGroup;
}

function dsWith(...payloads: Uint8Array[]): DsClient {
  return {
    fetchGroup: vi.fn(async () => ({
      messages: payloads.map((payload, i) => ({ id: i + 1, payload, createdAt: "2026-09-13T00:00:00Z" })),
      hasMore: false,
    })),
  } as unknown as DsClient;
}

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xc7, nextGid]);
}

async function classify(senderLeaf: number, ...payloads: Uint8Array[]): Promise<IncomingChatEvent[]> {
  const chat = new ChatClient(dsWith(...payloads), { identityProvider: () => IDENTITY_TOKEN });
  return (await chat.pollMessages(null, group(freshGid(), senderLeaf), 0)).events;
}

function profileEnvelope(
  name = "Alice Petrova",
  username = "alice",
  at = 1_700n,
  accountId?: Uint8Array,
): Uint8Array {
  return wasm.composeProfileEnvelope(profileSigner(), name, username, at, accountId);
}

describe("ADR-0048 — a profile is filtered in the foreground loop", () => {
  it("a profile is classified as a profile", async () => {
    const sent = profileEnvelope();
    const [ev] = await classify(3, sent);
    expect(ev?.kind).toBe("profile");
    if (ev?.kind !== "profile") throw new Error("unreachable");
    // ADR-0101 section 8 — the envelope rides along verbatim.
    expect(Array.from(ev.profile?.envelope ?? [])).toEqual(Array.from(sent));
    const { envelope: _envelope, ...fields } = ev.profile ?? {};
    expect(fields).toEqual({
      displayName: "Alice Petrova",
      username: "alice",
      updatedAtMs: 1_700n,
      accountId: null,
      // ADR-0060 profile v3 — no certificate and no grants in this envelope.
      usernameCertificate: null,
      grants: [],
    });
  });

  it("a profile's claimed account id rides along (ADR-0048 v2)", async () => {
    const id = new Uint8Array(16).fill(0x5a);
    const [ev] = await classify(3, profileEnvelope("Alice Petrova", "alice", 1_700n, id));
    if (ev?.kind !== "profile") throw new Error("unreachable");
    expect(ev.profile?.accountId).toEqual(id);
  });

  it("is bound to the sender's signature key resolved at receipt, never the leaf", async () => {
    const [ev] = await classify(3, profileEnvelope());
    if (ev?.kind !== "profile") throw new Error("expected a profile");
    expect(ev.senderKey).toBe(base64Encode(SENDER_KEY));
  });

  it("an unattributable sender carries no key (the consumer drops it)", async () => {
    const [ev] = await classify(9, profileEnvelope());
    if (ev?.kind !== "profile") throw new Error("expected a profile");
    expect(ev.senderKey).toBeUndefined();
  });

  it("a profile never reaches the timeline as text", async () => {
    const [ev] = await classify(3, profileEnvelope("Zed Unique Name"));
    const asText = (ev as { text?: string } | undefined)?.text ?? "";
    expect(asText).not.toContain("Zed Unique Name");
    expect(ev?.kind).not.toBe("application");
  });

  it("the real parser never degrades the bytes to chat (the defect is closed in the core)", () => {
    const parsed = wasm.parseBodyEnvelope(profileEnvelope("Zed Unique Name"));
    // Before 2026-09-29 the lossy-UTF-8 fallback rendered these bytes as a
    // chat bubble; the core now classifies them as a service envelope.
    expect(parsed.kind).toBe("service-envelope");
    expect(parsed.text).toBe("");
  });

  it("a malformed profile is dropped, not rendered", async () => {
    // A valid envelope plus one trailing byte: the domain matches, the core
    // refuses the body as a whole.
    const good = profileEnvelope();
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    bad[good.length] = 0x41;
    const [ev] = await classify(3, bad);
    expect(ev?.kind).toBe("profile");
    if (ev?.kind !== "profile") throw new Error("unreachable");
    expect(ev.profile).toBeUndefined();
  });

  it("a legacy plaintext starting with `l` is still chat", async () => {
    // Clears the cheap first-byte peek; the codec answers `wrongDomain`, so
    // the row falls through to the chat parser rather than being swallowed.
    const [ev] = await classify(3, new TextEncoder().encode("lunch at six?"));
    expect(ev?.kind).toBe("application");
    if (ev?.kind !== "application") throw new Error("unreachable");
    expect(ev.text).toBe("lunch at six?");
  });

  it("a mixed batch classifies every row independently", async () => {
    const events = await classify(
      3,
      wasm.composeEnvelopeText("before"),
      profileEnvelope(),
      wasm.composeCommitterBeacon(),
      wasm.composeEnvelopeText("after"),
    );
    expect(events.map((e) => e.kind)).toEqual(["application", "profile", "committerBeacon", "application"]);
  });
});

describe("ADR-0048 — an envelope published by another ChatClient on the page is an echo", () => {
  it("the chat route's client drops a profile the publisher client sent", async () => {
    const gid = freshGid();
    // Publisher: seals via a stand-in group and publishes as DS id 1.
    const publisherDs = {
      publishGroup: vi.fn(async () => ({ id: 1, createdAt: "2026-09-13T00:00:00Z" })),
    } as unknown as DsClient;
    const publisher = new ChatClient(publisherDs, { identityProvider: () => IDENTITY_TOKEN });
    const sealing = {
      get groupId(): Uint8Array {
        return gid;
      },
      createApplicationMessage: () => new Uint8Array([1, 2, 3]),
    } as unknown as WasmGroup;
    await publisher.broadcastControl(null, sealing, profileEnvelope());

    // The chat route's own client polls the same row back.
    const receiving = group(gid);
    const route = new ChatClient(dsWith(new Uint8Array([1, 2, 3])), { identityProvider: () => IDENTITY_TOKEN });
    const res = await route.pollMessages(null, receiving, 0);
    expect(res.events).toHaveLength(0);
    expect(receiving.processIncomingVerifyingRoles).not.toHaveBeenCalled();
  });
});
