// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The answer to a contact request goes out under that request's `reply` grant,
 * then a held `contact` grant, and nothing else: a group, member or channel
 * key has nothing to do with it. Seen live 2026-10-30: a kept group key, stale
 * since the requester left the group, was the only key tried, so neither the
 * Welcome nor the ack ever reached them.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import { base64Encode } from "../auth/account-id";
import type { MailboxWriteAuth } from "../ds/client";
import type { MailboxTarget } from "./mailbox-write";
import {
  EMPTY_RECEIVED_GRANTS,
  fileContactGrant,
  fileDelegations,
  fileFoundGrant,
  fileScopedGrant,
  type ReceivedGrants,
} from "./received-grants";

const session = vi.hoisted(() => ({ store: null as unknown }));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    decodeReachGrant: () => ({ mailboxPkVerified: (key: Uint8Array) => key, free: () => {} }),
    mailboxAddress: (pk: Uint8Array) => pk,
  }),
}));
vi.mock("../contacts/address-resolver", () => ({ personKeysHex: (hex: string) => [hex] }));
vi.mock("./reach-service", () => ({
  reachSession: () => ({ accountKeyHex: "own", receivedGrants: () => session.store, delegateKeys: () => [] }),
  reachSignerForGrant: async () => ({ free: () => {} }),
  reachSignerForGrantWire: async (wire: string) => ({ wire, free: () => {} }),
}));

import { MailboxUnreachableError, writeToPerson, writeWelcome } from "./mailbox-write";

const PERSON = new Uint8Array(32).fill(0xb0);
const HEX = "b0".repeat(32);
const SCOPE = new Uint8Array(32).fill(0xc0);
const SCOPE_HEX = "c0".repeat(32);

const wire = (name: string): string => base64Encode(new TextEncoder().encode(name));
const held = (name: string) => ({ wireB64: wire(name), version: 1, receivedAtMs: 1 });
const nameOf = (w: string): string => new TextDecoder().decode(Uint8Array.from(atob(w), (c) => c.charCodeAt(0)));

/** Everything a person can be held under, the group keys first in the sense
 *  that they are what a stale kept key looks like. */
function everyGrant(): ReceivedGrants {
  let store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, HEX, SCOPE_HEX, "group", held("group"));
  store = fileScopedGrant(store, HEX, SCOPE_HEX, "member", held("member"));
  store = fileScopedGrant(store, HEX, "d0", "channel", held("channel"));
  store = fileFoundGrant(store, HEX, held("found"));
  store = fileContactGrant(store, HEX, held("contact"));
  return fileDelegations(store, "someone", HEX, { envelopeB64: "AAAA", receivedAtMs: 1 });
}

/** A write that records each key and is refused for those in `refusing`. */
function recorder(refusing: readonly string[]) {
  const tried: string[] = [];
  const write = async (_target: MailboxTarget, auth: MailboxWriteAuth): Promise<string> => {
    const name = nameOf((auth.signer as unknown as { wire: string }).wire);
    tried.push(name);
    if (refusing.includes(name)) throw Object.assign(new Error("HTTP 401"), { status: 401 });
    return name;
  };
  return { tried, write };
}

const answerAck = (replyName: string | null, write: ReturnType<typeof recorder>["write"]): Promise<string> =>
  writeToPerson(
    PERSON,
    { explicit: replyName === null ? null : wire(replyName), answersRequest: true, recheck: null },
    write,
  );

const answerWelcome = (replyName: string | null, write: ReturnType<typeof recorder>["write"]): Promise<string> =>
  writeToPerson(
    PERSON,
    {
      scope: SCOPE,
      explicit: replyName === null ? null : wire(replyName),
      answersRequest: true,
      allowMember: true,
      recheck: null,
    },
    write,
  );

beforeEach(() => {
  session.store = everyGrant();
});

describe.each([
  ["the ack", answerAck],
  ["the Welcome", answerWelcome],
])("%s of an accept", (_name, answer) => {
  it("goes under the request's reply grant first, whatever else is held", async () => {
    const r = recorder([]);
    expect(await answer("reply", r.write)).toBe("reply");
    expect(r.tried).toEqual(["reply"]);
  });

  it("moves to a held contact grant at a 401", async () => {
    const r = recorder(["reply"]);
    expect(await answer("reply", r.write)).toBe("contact");
    expect(r.tried).toEqual(["reply", "contact"]);
  });

  it("with every candidate refused, tries no group, member or channel key and no delegation", async () => {
    const r = recorder(["reply", "contact"]);
    await expect(answer("reply", r.write)).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(r.tried).toEqual(["reply", "contact"]);
  });

  it("holding only a stale group key and no reply grant, writes nothing and never unsigned", async () => {
    session.store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, HEX, SCOPE_HEX, "group", held("group"));
    const r = recorder([]);
    await expect(answer(null, r.write)).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(r.tried).toEqual([]);
  });

  it("does not try one grant twice", async () => {
    session.store = fileContactGrant(EMPTY_RECEIVED_GRANTS, HEX, held("reply"));
    const r = recorder(["reply"]);
    await expect(answer("reply", r.write)).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(r.tried).toEqual(["reply"]);
  });
});

describe("any other write with a grant of its own", () => {
  it("moves on to the kept grants at a 401", async () => {
    const r = recorder(["joiner", "contact"]);
    const out = await writeWelcome(PERSON, { scope: SCOPE, explicit: wire("joiner") }, r.write);
    expect(out).toBe("group");
    expect(r.tried).toEqual(["joiner", "contact", "group"]);
  });
});
