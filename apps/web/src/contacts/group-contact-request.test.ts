// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` section 6.4 - the receive side of a contact
 * request sent inside a group. The envelope, the request and the receiver's
 * check are the REAL core's; the setting, the contact list and the inbox are
 * stand-ins.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { decodeContactRequestBody, type IncomingContactRequest } from "./client";
import {
  duplicateOfHeld,
  groupRequestRowId,
  groupRequestVerdict,
  openArrivedGroupRequest,
  type ArrivedGroupContactRequest,
} from "./group-contact-request";
import { createPendingInbox, fromStoredRow, loadInbox, toStoredRow } from "./inbox-store";
import { waitingSentRequests } from "./contact-list-model";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_ID = new Uint8Array(32).fill(0xab);
const GROUP_B64 = base64Encode(GROUP_ID);

interface Parties {
  readonly sender: wasm.Identity;
  readonly recipient: wasm.Identity;
  readonly other: wasm.Identity;
  readonly envelope: Uint8Array;
}

function parties(): Parties {
  const sender = new wasm.Identity("sender");
  const recipient = new wasm.Identity("recipient");
  const other = new wasm.Identity("other");
  const request = wasm.ContactRequest.compose(
    sender.ed25519Seed,
    new Uint8Array(16).fill(1),
    recipient.x25519PublicKey,
    new Uint8Array(32).fill(2),
    1n,
    new Uint8Array(0),
    new Uint8Array(0),
    undefined,
    new Uint8Array(0),
    new Uint8Array(0),
    new TextEncoder().encode("hi"),
  );
  return { sender, recipient, other, envelope: wasm.composeGroupContactRequestEnvelope(recipient.x25519PublicKey, request) };
}

function arrived(p: Parties, senderAccountKey: Uint8Array | undefined, journalMessageId = 7): ArrivedGroupContactRequest {
  return {
    groupIdB64: GROUP_B64,
    groupId: GROUP_ID,
    journalMessageId,
    createdAt: "2026-11-05T10:00:00Z",
    plaintext: p.envelope,
    senderAccountKey,
  };
}

function deps(me: wasm.Identity, setting: { allows: boolean; contacts?: string[] }) {
  return {
    check: (plaintext: Uint8Array, secret: Uint8Array, sender: Uint8Array) =>
      wasm.checkGroupContactRequest(plaintext, secret, sender),
    x25519Secret: me.x25519SecretKey,
    decode: (w: Uint8Array, id: number, at: string, via: string) =>
      decodeContactRequestBody(wasm as never, id, w, at, me.x25519SecretKey, via),
    allowsGroupMembers: () => setting.allows,
    isContact: (k: string) => (setting.contacts ?? []).includes(k),
    keyB64: base64Encode,
  };
}

describe("a request sealed inside a group opens only for the person it is for", () => {
  it("is the core's group contact request envelope", () => {
    const p = parties();
    expect(wasm.isGroupContactRequestEnvelope(p.envelope)).toBe(true);
  });

  it("every other member drops it without a trace", () => {
    const p = parties();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    expect(openArrivedGroupRequest(arrived(p, p.sender.signaturePublicKey), deps(p.other, { allows: true }))).toBeNull();
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("opens for the person and becomes an ordinary request with the group it came through", () => {
    const p = parties();
    const opened = openArrivedGroupRequest(arrived(p, p.sender.signaturePublicKey), deps(p.recipient, { allows: true }));
    expect(opened).not.toBeNull();
    expect(opened?.request.intro).toBe("hi");
    expect(base64Encode(opened?.request.fromPubkey ?? new Uint8Array())).toBe(base64Encode(p.sender.signaturePublicKey));
    expect(opened?.request.viaGroupIdB64).toBe(GROUP_B64);
    expect(opened?.row.viaGroupIdB64).toBe(GROUP_B64);
    expect(opened?.request.messageId).toBeLessThan(0);
  });

  it("is dropped when the member who posted it is not the request's sender", () => {
    const p = parties();
    expect(openArrivedGroupRequest(arrived(p, p.other.signaturePublicKey), deps(p.recipient, { allows: true }))).toBeNull();
    expect(openArrivedGroupRequest(arrived(p, undefined), deps(p.recipient, { allows: true }))).toBeNull();
  });

  it("is dropped when the person's setting hides them from this group, unless the sender is a contact", () => {
    const p = parties();
    const hidden = deps(p.recipient, { allows: false });
    expect(openArrivedGroupRequest(arrived(p, p.sender.signaturePublicKey), hidden)).toBeNull();
    const contact = deps(p.recipient, { allows: false, contacts: [base64Encode(p.sender.signaturePublicKey)] });
    expect(openArrivedGroupRequest(arrived(p, p.sender.signaturePublicKey), contact)).not.toBeNull();
    expect(groupRequestVerdict({ allowsGroupMembers: false, senderIsContact: false })).toBe("drop");
  });
});

describe("one request per person", () => {
  const req = (key: number, pseudonym: number, via?: string): IncomingContactRequest => ({
    messageId: 1,
    fromPubkey: new Uint8Array(32).fill(key),
    fromAccountId: null,
    fromPseudonym: new Uint8Array(32).fill(pseudonym),
    validAfterMs: 0n,
    intro: null,
    profile: null,
    replyGrantB64: null,
    linkAnswer: null,
    createdAt: "",
    ...(via === undefined ? {} : { viaGroupIdB64: via }),
  });

  it("the same request through the inbox and a group is listed once", () => {
    expect(duplicateOfHeld(req(1, 9), [req(1, 9, "g")])).toBe(true);
    expect(duplicateOfHeld(req(1, 9, "g"), [req(1, 9)])).toBe(true);
  });

  it("a request through a group from someone already listed is not listed again", () => {
    expect(duplicateOfHeld(req(1, 8, "g"), [req(1, 9)])).toBe(true);
    expect(duplicateOfHeld(req(2, 9, "g"), [req(1, 9)])).toBe(false);
  });

  it("a group row's id is negative, stable, and differs by row and group", () => {
    const a = groupRequestRowId(GROUP_B64, 7);
    expect(a).toBeLessThan(0);
    expect(groupRequestRowId(GROUP_B64, 7)).toBe(a);
    expect(groupRequestRowId(GROUP_B64, 8)).not.toBe(a);
    expect(groupRequestRowId("b3RoZXI=", 7)).not.toBe(a);
    expect(Number.isSafeInteger(a)).toBe(true);
  });
});

describe("the group a request came through survives the page", () => {
  it("is stored with the row and read back", () => {
    const data = new Map<string, string>();
    const st = {
      get length() {
        return data.size;
      },
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
      removeItem: (k: string) => void data.delete(k),
      key: (i: number) => [...data.keys()][i] ?? null,
    };
    const p = parties();
    const opened = openArrivedGroupRequest(arrived(p, p.sender.signaturePublicKey), deps(p.recipient, { allows: true }));
    if (opened === null) throw new Error("not opened");
    const inbox = createPendingInbox({ accountIdB64: "acct", isAccepted: () => false, onChange: () => undefined, storage: st });
    inbox.addFetched([{ request: opened.request, row: opened.row }]);
    const stored = loadInbox("acct", st).pending[0];
    expect(stored?.viaGroupIdB64).toBe(GROUP_B64);
    expect(stored === undefined ? undefined : fromStoredRow(stored).viaGroupIdB64).toBe(GROUP_B64);
    expect(toStoredRow({ messageId: 1, payload: new Uint8Array([1]), createdAt: "" }).viaGroupIdB64).toBeUndefined();
  });
});

describe("a request from a contact first met in a group is listed", () => {
  it("kept although the sender is a contact, while this device holds no one-to-one with them", () => {
    const data = new Map<string, string>();
    const st = {
      get length() {
        return data.size;
      },
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
      removeItem: (k: string) => void data.delete(k),
      key: (i: number) => [...data.keys()][i] ?? null,
    };
    const p = parties();
    const opened = openArrivedGroupRequest(arrived(p, p.sender.signaturePublicKey), deps(p.recipient, { allows: true }));
    if (opened === null) throw new Error("not opened");
    let holdsChat = false;
    const inbox = createPendingInbox({
      accountIdB64: "acct",
      isAccepted: () => true,
      keepsFromContact: (r) => r.viaGroupIdB64 !== undefined && !holdsChat,
      onChange: () => undefined,
      storage: st,
    });
    inbox.addFetched([{ request: opened.request, row: opened.row }]);
    expect(inbox.pending()).toHaveLength(1);
    // With a one-to-one already here, the same request is not listed.
    holdsChat = true;
    const second = createPendingInbox({
      accountIdB64: "other",
      isAccepted: () => true,
      keepsFromContact: (r) => r.viaGroupIdB64 !== undefined && !holdsChat,
      onChange: () => undefined,
      storage: st,
    });
    second.addFetched([{ request: opened.request, row: opened.row }]);
    expect(second.pending()).toHaveLength(0);
  });
});

describe("the sender's side", () => {
  it("a request sent inside a group stands in Sent although its recipient is a contact", () => {
    const record = {
      recipientUsername: "",
      recipientAccountIdHex: "aa",
      sentAtMs: 1,
      recipientKeyB64: "key",
      throughGroupIdB64: "walk",
    };
    expect(waitingSentRequests([record], new Set(["aa"]), new Set(["key"]))).toEqual([record]);
    const { throughGroupIdB64: _, ...plain } = record;
    expect(waitingSentRequests([plain], new Set(["aa"]), new Set(["key"]))).toEqual([]);
  });
});
