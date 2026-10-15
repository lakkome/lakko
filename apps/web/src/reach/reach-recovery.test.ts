// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 1, "Versions after a restore" (amended 2026-10-01): a
 * device that holds no version state reads the reach keys its mailbox holds
 * before it registers anything, and resumes each class from the highest
 * version registered there, so a key rotated away is never registered again.
 *
 * Driven through the real `ReachSession` with the REAL core deriving the keys
 * and a fake DS. IndexedDB is absent under node, so every session starts with
 * an empty book: exactly a fresh install or a restore.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import { ReachSession } from "./reach-service";
import {
  EMPTY_REACH_BOOK,
  REACH_RECOVERY_ACCOUNT_WIDE_VERSIONS,
  REACH_RECOVERY_SCOPED_VERSIONS,
  applyVersionChecks,
  bookHoldsNoVersionState,
  bumpVersion,
  versionChecksDue,
  versionOf,
} from "./reach-model";
import type { DsClient, MailboxReachState, MailboxReachUpdate } from "../ds/client";
import { journalHex } from "../ds/access";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GROUP = new Uint8Array(32).fill(0x0a);
const GROUP_HEX = journalHex(GROUP);

function reachPk(seed: Uint8Array, cls: number, scope: Uint8Array, version: number): Uint8Array {
  const s = wasm.reachSeed(seed, cls, scope, version);
  try {
    return wasm.reachVerifyingKey(s);
  } finally {
    s.fill(0);
  }
}

function fakeDs(read: () => Promise<MailboxReachState>): {
  ds: DsClient;
  puts: MailboxReachUpdate[];
  reads: () => number;
} {
  const puts: MailboxReachUpdate[] = [];
  let reads = 0;
  const ds = {
    getMailboxReach: vi.fn(async () => {
      reads += 1;
      return read();
    }),
    putMailboxReach: vi.fn(async (_p: Uint8Array, update: MailboxReachUpdate) => {
      puts.push(update);
    }),
  } as unknown as DsClient;
  return { ds, puts, reads: () => reads };
}

const hexOf = (b: Uint8Array): string => journalHex(b);

describe("a restored device resumes from the keys the mailbox holds", () => {
  it("contact v1 and member v1 on the DS: resumes at v1, never registers v0", async () => {
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x71));
    const seed = account.ed25519Seed;
    const empty = new Uint8Array(0);
    const contactV0 = reachPk(seed, wasm.ReachClass.Contact, empty, 0);
    const contactV1 = reachPk(seed, wasm.ReachClass.Contact, empty, 1);
    const memberV0 = reachPk(seed, wasm.ReachClass.Member, GROUP, 0);
    const memberV1 = reachPk(seed, wasm.ReachClass.Member, GROUP, 1);
    const { ds, puts } = fakeDs(async () => ({
      keys: [
        { publicKey: contactV1, expiresAtMs: null },
        { publicKey: memberV1, expiresAtMs: null },
      ],
      open: false,
    }));
    const session = await ReachSession.open("acc", seed, account.signaturePublicKey, { ds, accountMailbox: new Uint8Array(32).fill(0xaa) });
    session.setScopeProvider(() => ({ groupIdsHex: [GROUP_HEX], channels: [] }));
    try {
      await session.syncNow(true);
      expect(session.currentVersion("contact")).toBe(1);
      expect(session.currentVersion("member", GROUP)).toBe(1);
      const added = puts.flatMap((p) => (p.add ?? []).map((a) => hexOf(a.publicKey)));
      for (const pk of [contactV0, memberV0]) expect(added).not.toContain(hexOf(pk));
      // The v1 keys are already there: not re-added either.
      for (const pk of [contactV1, memberV1]) expect(added).not.toContain(hexOf(pk));
      // The grant this device hands out names the version that works.
      const grant = session.grant("contact");
      expect(grant.version).toBe(1);
      grant.free();
    } finally {
      session.stop();
      account.free();
    }
  });

  it("a group joined AFTER the first sync is checked when it appears", async () => {
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x72));
    const seed = account.ed25519Seed;
    const memberV0 = reachPk(seed, wasm.ReachClass.Member, GROUP, 0);
    const memberV2 = reachPk(seed, wasm.ReachClass.Member, GROUP, 2);
    const { ds, puts, reads } = fakeDs(async () => ({
      keys: [{ publicKey: memberV2, expiresAtMs: null }],
      open: false,
    }));
    let groups: string[] = [];
    const session = await ReachSession.open("acc", seed, account.signaturePublicKey, { ds, accountMailbox: new Uint8Array(32).fill(0xaa) });
    session.setScopeProvider(() => ({ groupIdsHex: groups, channels: [] }));
    try {
      await session.syncNow(true);
      expect(reads()).toBe(1);
      // The re-admission Welcome arrives later.
      groups = [GROUP_HEX];
      await session.syncNow(true);
      expect(reads()).toBe(2);
      expect(session.currentVersion("member", GROUP)).toBe(2);
      const added = puts.flatMap((p) => (p.add ?? []).map((a) => hexOf(a.publicKey)));
      expect(added).not.toContain(hexOf(memberV0));
      // Once checked, a later sync does not read again.
      await session.syncNow(true);
      expect(reads()).toBe(2);
    } finally {
      session.stop();
      account.free();
    }
  });

  it("a failed read registers nothing and fails the sync", async () => {
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x73));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { ds, puts } = fakeDs(async () => {
      throw Object.assign(new Error("HTTP 503"), { status: 503 });
    });
    const session = await ReachSession.open("acc", account.ed25519Seed, account.signaturePublicKey, { ds, accountMailbox: new Uint8Array(32).fill(0xaa) });
    session.setScopeProvider(() => ({ groupIdsHex: [GROUP_HEX], channels: [] }));
    try {
      await expect(session.syncNow(true)).rejects.toThrow();
      expect(puts).toEqual([]);
      // A request whose answer key cannot be registered is not sent either.
      await expect(session.newReply("contact-request")).rejects.toThrow();
      expect(puts).toEqual([]);
    } finally {
      session.stop();
      account.free();
      warn.mockRestore();
    }
  });

  it("nothing registered on the DS: every class starts at 0", async () => {
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x74));
    const seed = account.ed25519Seed;
    const contactV0 = reachPk(seed, wasm.ReachClass.Contact, new Uint8Array(0), 0);
    const { ds, puts } = fakeDs(async () => ({ keys: [], open: false }));
    const session = await ReachSession.open("acc", seed, account.signaturePublicKey, { ds, accountMailbox: new Uint8Array(32).fill(0xaa) });
    session.setScopeProvider(() => ({ groupIdsHex: [], channels: [] }));
    try {
      await session.syncNow(true);
      expect(session.currentVersion("contact")).toBe(0);
      const added = puts.flatMap((p) => (p.add ?? []).map((a) => hexOf(a.publicKey)));
      expect(added).toContain(hexOf(contactV0));
    } finally {
      session.stop();
      account.free();
    }
  });
});

describe("the pure rule", () => {
  const scopes = { groupIdsHex: ["aa"], channels: [{ idHex: "cc", epoch: 7 }] };

  it("a book with version state is not recovering and checks nothing", () => {
    const book = bumpVersion(EMPTY_REACH_BOOK, "contact", "");
    expect(bookHoldsNoVersionState(book)).toBe(false);
    expect(versionChecksDue(book, scopes)).toEqual([]);
  });

  it("a recovering book checks every versioned class it could register, with the stated bounds", () => {
    const book = { ...EMPTY_REACH_BOOK, recovery: { checked: {} } };
    expect(versionChecksDue(book, scopes)).toEqual([
      { cls: "contact", scopeHex: "", from: 0, to: REACH_RECOVERY_ACCOUNT_WIDE_VERSIONS },
      { cls: "fof", scopeHex: "", from: 0, to: REACH_RECOVERY_ACCOUNT_WIDE_VERSIONS },
      { cls: "group", scopeHex: "aa", from: 0, to: REACH_RECOVERY_SCOPED_VERSIONS },
      { cls: "member", scopeHex: "aa", from: 0, to: REACH_RECOVERY_SCOPED_VERSIONS },
      { cls: "channel", scopeHex: "cc", from: 7, to: 7 + REACH_RECOVERY_SCOPED_VERSIONS },
    ]);
  });

  it("the highest registered version wins, lower ones are recorded so the diff removes them", () => {
    const book = { ...EMPTY_REACH_BOOK, recovery: { checked: {} } };
    const pk = (cls: string, scope: string, v: number): string => `${cls}/${scope}/${v}`;
    const ds = new Map<string, number | null>([
      [pk("contact", "", 0), null],
      [pk("contact", "", 3), null],
    ]);
    const out = applyVersionChecks(
      book,
      [{ cls: "contact", scopeHex: "", from: 0, to: 5 }],
      ds,
      pk,
    );
    expect(out.raised).toBe(true);
    expect(versionOf(out.book, "contact", "")).toBe(3);
    expect(Object.keys(out.book.registered).sort()).toEqual([pk("contact", "", 0), pk("contact", "", 3)]);
    expect(out.book.recovery?.checked["contact:"]).toBe(true);
  });
});

describe("the DS read", () => {
  it("parses the owner's reach read, and refuses a malformed one rather than read it as empty", async () => {
    const { parseMailboxReachState } = await import("../ds/client");
    const pk = Buffer.from(new Uint8Array(32).fill(3)).toString("base64");
    expect(
      parseMailboxReachState({
        keys: [
          { reach_pk_b64: pk, expires_at_ms: null },
          { reach_pk_b64: pk, expires_at_ms: 5 },
        ],
        open: true,
      }),
    ).toEqual({
      keys: [
        { publicKey: new Uint8Array(32).fill(3), expiresAtMs: null },
        { publicKey: new Uint8Array(32).fill(3), expiresAtMs: 5 },
      ],
      open: true,
    });
    expect(() => parseMailboxReachState({ keys: [] })).toThrow();
    expect(() => parseMailboxReachState({ keys: [{ reach_pk_b64: "AAAA" }], open: false })).toThrow();
    expect(() => parseMailboxReachState(null)).toThrow();
  });
});
