// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 1 (amended 2026-11-06) — Remove contact rotates the shared
 * `contact` key; the retired version stays registered; the profile a removed
 * contact's one-to-one carries holds no contact grant; a row under the
 * retired key is processed and regranted from a contact, dropped from anyone
 * else.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { DsClient } from "../ds/client";
import { ReachSession } from "./reach-service";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

function contactKey(seed: Uint8Array, version: number): Uint8Array {
  const s = wasm.reachSeed(seed, wasm.ReachClass.Contact, new Uint8Array(0), version);
  try {
    return wasm.reachVerifyingKey(s);
  } finally {
    s.fill(0);
  }
}

describe("Remove contact rotates the contact key", () => {
  it("registers the new key, keeps the retired one, and the removed contact's chat gets no grant", async () => {
    const seed = crypto.getRandomValues(new Uint8Array(32));
    const accountKey = wasm.reachVerifyingKey(seed);
    const puts: { add: { publicKey: Uint8Array }[]; remove: Uint8Array[] }[] = [];
    const ds = {
      getMailboxReach: vi.fn(async () => ({ keys: [], open: false })),
      putMailboxReach: vi.fn(async (_m: Uint8Array, body: { add: { publicKey: Uint8Array }[]; remove: Uint8Array[] }) => {
        puts.push(body);
      }),
    } as unknown as DsClient;
    const sess = await ReachSession.open(`acct-${hex(seed).slice(0, 8)}`, seed, accountKey, {
      ds,
      now: () => 1_000,
      accountMailbox: new Uint8Array(32).fill(0xaa),
    });
    await sess.syncNow(true);
    const gid = new Uint8Array(32).fill(7);
    const before = sess.profileGrantSignature(gid, true, true);

    await sess.rotateOnContactRemoved();

    // The new contact version is what a remaining contact's chat now carries.
    const after = sess.profileGrantSignature(gid, true, true);
    expect(after).not.toBe(before);
    expect(after).toContain("contact:1");
    // A removed contact's one-to-one carries none.
    expect(sess.profileGrantSignature(gid, true, false)).not.toContain("contact:");
    expect(sess.profileGrants(gid, true, false).length).toBe(1);

    // The rotation registered the new key and removed nothing of the window.
    const last = puts.at(-1)!;
    expect(last.add.map((a) => hex(a.publicKey))).toContain(hex(contactKey(seed, 1)));
    expect(last.remove.map(hex)).not.toContain(hex(contactKey(seed, 0)));

    // The receive path, by the key that admitted the row.
    expect(sess.contactRowVerdict(contactKey(seed, 1), false)).toBe("process");
    expect(sess.contactRowVerdict(undefined, false)).toBe("process");
    expect(sess.contactRowVerdict(contactKey(seed, 0), true)).toBe("regrant");
    expect(sess.contactRowVerdict(contactKey(seed, 0), false)).toBe("drop");
    sess.stop();
  });
});
