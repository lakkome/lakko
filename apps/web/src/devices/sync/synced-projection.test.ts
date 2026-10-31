// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The edit-to-change-set builder, driven into the REAL Rust fold. The fold's
 * own merge rules are tested in `lakko-sync`; what is tested here is that the
 * web builds the registers it meant to and reads the folder back the way the
 * rest of the app expects - including that an edit leaves the registers it
 * did not name alone.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../../wasm/lakko_ffi_wasm";
import {
  diffFolderDraft,
  EXCLUDE_ARCHIVED,
  INCLUDE_GROUPS,
  newFolderDraft,
  stampEdits,
  type ConversationRef,
  type RegisterEdit,
} from "../../chat/folders/folder-model";
import { buildFolderChangeSet, mergeRestoredFold, projectSyncedState } from "./synced-projection";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

let clock = 1_000n;
const mint = (): bigint => (clock += 1n);

function apply(s: wasm.SyncedState, edits: readonly RegisterEdit[]): void {
  const cs = buildFolderChangeSet(wasm, stampEdits(edits, mint));
  try {
    s.applyFolderChanges(cs);
  } finally {
    cs.free();
  }
}

const FID = "ab".repeat(16);
const G1: ConversationRef = { kind: 2, id: new Uint8Array(32).fill(1) };
const G2: ConversationRef = { kind: 2, id: new Uint8Array(32).fill(2) };
const FAR: ConversationRef = { kind: 3, id: new Uint8Array(32).fill(7) };

describe("buildFolderChangeSet into the fold", () => {
  it("creates a folder the projection reads back", () => {
    const s = new wasm.SyncedState();
    apply(s, diffFolderDraft(FID, null, { ...newFolderDraft(G1), name: "Work", includeKinds: INCLUDE_GROUPS }));
    const [f] = projectSyncedState(s).folders;
    expect(f?.idHex).toBe(FID);
    expect(f?.name).toBe("Work");
    expect(f?.includeKinds).toBe(INCLUDE_GROUPS);
    expect(f?.excludeFlags).toBe(EXCLUDE_ARCHIVED);
    expect(f?.included.map((r) => r.id)).toEqual([G1.id]);
  });

  it("an edit touching one chat leaves a chat this device cannot see in place", () => {
    const s = new wasm.SyncedState();
    apply(s, diffFolderDraft(FID, null, { ...newFolderDraft(), name: "Work", included: [G1, FAR] }));
    const before = projectSyncedState(s).folders[0];
    if (before === undefined) throw new Error("no folder");
    // This device resolves G1 but not FAR; the person adds G2.
    apply(s, diffFolderDraft(FID, before, { ...before, included: [...before.included, G2] }));
    const after = projectSyncedState(s).folders[0];
    const ids = after?.included.map((r) => r.id[0]) ?? [];
    expect(ids.sort()).toEqual([1, 2, 7]);
  });

  it("a folder is not listed before its name arrives, and never after its deletion", () => {
    const s = new wasm.SyncedState();
    apply(s, [{ t: "member", folderIdHex: FID, ref: G1, state: 1 }]);
    expect(projectSyncedState(s).folders).toEqual([]);
    apply(s, [{ t: "meta", folderIdHex: FID, name: "Work", includeKinds: 0, excludeFlags: 0 }]);
    expect(projectSyncedState(s).folders.length).toBe(1);
    apply(s, [{ t: "removed", folderIdHex: FID }]);
    apply(s, [{ t: "meta", folderIdHex: FID, name: "Back", includeKinds: 0, excludeFlags: 0 }]);
    expect(projectSyncedState(s).folders).toEqual([]);
  });

  it("folder pins and order", () => {
    const s = new wasm.SyncedState();
    const F2 = "cd".repeat(16);
    apply(s, [
      { t: "meta", folderIdHex: FID, name: "A", includeKinds: INCLUDE_GROUPS, excludeFlags: 0 },
      { t: "meta", folderIdHex: F2, name: "B", includeKinds: INCLUDE_GROUPS, excludeFlags: 0 },
    ]);
    apply(s, [{ t: "order", folderIdsHex: [F2, FID] }]);
    apply(s, [{ t: "pins", folderIdHex: FID, pinned: [G2, G1] }]);
    const fs = projectSyncedState(s).folders;
    expect(fs.map((f) => f.name)).toEqual(["B", "A"]);
    expect(fs[1]?.pinned.map((r) => r.id[0])).toEqual([2, 1]);
  });

  it("refuses a register that breaks a bound before anything is applied", () => {
    expect(() =>
      buildFolderChangeSet(
        wasm,
        stampEdits([{ t: "meta", folderIdHex: FID, name: "", includeKinds: 0, excludeFlags: 0 }], mint),
      ),
    ).toThrow();
    expect(() =>
      buildFolderChangeSet(wasm, stampEdits([{ t: "removed", folderIdHex: "zz" }], mint)),
    ).toThrow();
  });

  it("projects the four maps from local writes", () => {
    const s = new wasm.SyncedState();
    const gid = new Uint8Array(32).fill(5);
    s.applyFlags(mint(), gid, true, false, true, false);
    s.applyConversationOrder(mint(), [gid]);
    const p = projectSyncedState(s);
    expect(p.flags["05".repeat(32)]?.archived).toBe(true);
    expect(p.order?.groupIds).toEqual([gid]);
  });

  it("round-trips the fold through its bytes and its snapshot parts", () => {
    const s = new wasm.SyncedState();
    apply(s, [{ t: "meta", folderIdHex: FID, name: "Work", includeKinds: INCLUDE_GROUPS, excludeFlags: 0 }]);
    const back = wasm.SyncedState.fromBytes(s.toBytes());
    expect(projectSyncedState(back).folders.map((f) => f.name)).toEqual(["Work"]);
    const parts = s.snapshotParts();
    expect(parts.length).toBeGreaterThanOrEqual(1);
    for (const p of parts) p.free();
  });
});

describe("the end of a membership (ADR-0087 section 9)", () => {
  /** Fold the register a compose returned, as `writeConversationEnd` does,
   *  and return what the outbox's edit then sends. */
  function write(s: wasm.SyncedState, register: wasm.ConversationEndRegister | undefined): RegisterEdit[] {
    expect(register).toBeDefined();
    const cs = new wasm.FolderChangeSet();
    cs.addConversationEnd(register as wasm.ConversationEndRegister);
    s.applyFolderChanges(cs);
    cs.free();
    return [{ t: "conversation-end", ref: G1 }];
  }

  /** What another device of the account folds from the same outbox entry. */
  function send(from: wasm.SyncedState, to: wasm.SyncedState, edits: readonly RegisterEdit[]): void {
    const cs = buildFolderChangeSet(wasm, stampEdits(edits, mint), (ref) =>
      from.conversationEnd(ref.kind, ref.id),
    );
    expect(cs.registerCount).toBe(1);
    to.applyFolderChanges(cs);
    cs.free();
  }

  it("an end reaches the projection of the device that wrote it and of the one that heard it", () => {
    const here = new wasm.SyncedState();
    const there = new wasm.SyncedState();
    send(here, there, write(here, here.composeConversationEnded(mint(), G1.kind, G1.id, 7n)));
    for (const s of [here, there]) {
      expect(projectSyncedState(s).endedConversations).toEqual([{ ref: G1, epoch: 7n }]);
    }
  });

  it("a later join clears it on the other device too", () => {
    const here = new wasm.SyncedState();
    const there = new wasm.SyncedState();
    send(here, there, write(here, here.composeConversationEnded(mint(), G1.kind, G1.id, 7n)));
    send(here, there, write(here, here.composeConversationJoined(mint(), G1.kind, G1.id, 9n)));
    expect(projectSyncedState(there).endedConversations).toEqual([]);
  });

  it("a stale end does not bury a join, whatever the stamps say", () => {
    const rejoined = new wasm.SyncedState();
    write(rejoined, rejoined.composeConversationEnded(mint(), G1.kind, G1.id, 7n));
    write(rejoined, rejoined.composeConversationJoined(mint(), G1.kind, G1.id, 9n));
    // A device offline across both applies the old removal now, with a newer
    // stamp than either. The epoch is what decides.
    const late = new wasm.SyncedState();
    const register = late.composeConversationEnded(mint(), G1.kind, G1.id, 7n);
    write(late, register);
    late.mergeFrom(rejoined);
    expect(projectSyncedState(late).endedConversations).toEqual([]);
  });

  it("nothing to tell is undefined: a first join writes no register", () => {
    const s = new wasm.SyncedState();
    expect(s.composeConversationJoined(mint(), G1.kind, G1.id, 3n)).toBeUndefined();
    write(s, s.composeConversationEnded(mint(), G1.kind, G1.id, 7n));
    expect(s.composeConversationEnded(mint(), G1.kind, G1.id, 7n)).toBeUndefined();
  });

  it("an outbox entry with no register held sends nothing", () => {
    const cs = buildFolderChangeSet(wasm, stampEdits([{ t: "conversation-end", ref: G1 }], mint), () => undefined);
    expect(cs.registerCount).toBe(0);
    cs.free();
  });

  it("a channel writes epoch 0, and its stamp decides", () => {
    const s = new wasm.SyncedState();
    const channel: ConversationRef = FAR;
    write(s, s.composeConversationEnded(mint(), channel.kind, channel.id, 0n));
    expect(s.isConversationEnded(channel.kind, channel.id)).toBe(true);
    write(s, s.composeConversationJoined(mint(), channel.kind, channel.id, 0n));
    expect(s.isConversationEnded(channel.kind, channel.id)).toBe(false);
  });
});

describe("mergeRestoredFold - a restore merges, never replaces (ADR-0058 5.2)", () => {
  it("keeps a backup's hand-added chat AND a local deletion marker", () => {
    const local = new wasm.SyncedState();
    const backup = new wasm.SyncedState();
    const KEEP = "0a".repeat(16);
    const GONE = "0b".repeat(16);
    const into = (s: wasm.SyncedState, edits: RegisterEdit[]): void => {
      const cs = buildFolderChangeSet(wasm, stampEdits(edits, mint));
      s.applyFolderChanges(cs);
      cs.free();
    };
    // Both devices know both folders.
    const base: RegisterEdit[] = [
      { t: "meta", folderIdHex: KEEP, name: "Work", includeKinds: 0, excludeFlags: 0 },
      { t: "meta", folderIdHex: GONE, name: "Old", includeKinds: INCLUDE_GROUPS, excludeFlags: 0 },
    ];
    into(local, base);
    into(backup, base);
    // The backup added a chat to Work by hand; this device deleted Old.
    into(backup, [{ t: "member", folderIdHex: KEEP, ref: G2, state: 1 }]);
    into(local, [{ t: "removed", folderIdHex: GONE }]);
    // The backup even renamed Old after this device deleted it: deletion is final.
    into(backup, [{ t: "meta", folderIdHex: GONE, name: "Renamed", includeKinds: 0, excludeFlags: 0 }]);

    expect(mergeRestoredFold(local, backup)).toBe(true);
    const p = projectSyncedState(local);
    expect(p.folders.map((f) => f.idHex)).toEqual([KEEP]);
    expect(p.folders[0]?.included.map((r) => r.id)).toEqual([G2.id]);
  });


  const A = new Uint8Array(32).fill(0xa1);
  const B = new Uint8Array(32).fill(0xb2);
  const C = new Uint8Array(32).fill(0xc3);
  const hex = (g: Uint8Array): string => Array.from(g, (x) => x.toString(16).padStart(2, "0")).join("");

  function sides(): { local: wasm.SyncedState; backup: wasm.SyncedState } {
    const local = new wasm.SyncedState();
    const backup = new wasm.SyncedState();
    // A: local is newer. B: backup is newer. C: only in the backup.
    local.applyFlags(200n, A, true, false, false, false);
    backup.applyFlags(100n, A, false, true, false, false);
    local.applyFlags(100n, B, false, false, false, false);
    backup.applyFlags(300n, B, false, false, true, false);
    backup.applyDraft(50n, C, "from the backup", new Uint8Array(0));
    local.applyDraft(10n, A, "stale local", new Uint8Array(0));
    backup.applyDraft(90n, A, "newer in the backup", new Uint8Array(0));
    local.applyConversationOrder(400n, [A, B]);
    backup.applyConversationOrder(10n, [B, A]);
    return { local, backup };
  }

  it("keeps the newer register from each side, whichever is the base", () => {
    const { local, backup } = sides();
    mergeRestoredFold(local, backup);
    const p = projectSyncedState(local);
    expect(p.flags[hex(A)]).toMatchObject({ pinned: true, muted: false, updatedAtHlc: 200n });
    expect(p.flags[hex(B)]).toMatchObject({ archived: true, updatedAtHlc: 300n });
    expect(p.drafts[hex(C)]?.text).toBe("from the backup");
    expect(p.drafts[hex(A)]?.text).toBe("newer in the backup");
    expect(p.order?.updatedAtHlc).toBe(400n);
    expect(p.order?.groupIds).toEqual([A, B]);
  });

  it("is order-independent for the registers it merges", () => {
    const one = sides();
    const two = sides();
    mergeRestoredFold(one.local, one.backup);
    mergeRestoredFold(two.backup, two.local);
    const ab = projectSyncedState(one.local);
    const ba = projectSyncedState(two.backup);
    expect(ab.flags).toEqual(ba.flags);
    expect(ab.drafts).toEqual(ba.drafts);
    expect(ab.order).toEqual(ba.order);
  });

  it("keeps folders from both sides, and the newer meta of a folder both hold", () => {
    const { local, backup } = sides();
    const F1 = "01".repeat(16);
    const F2 = "02".repeat(16);
    const meta = (s: wasm.SyncedState, hlc: bigint, id: string, name: string): void => {
      const cs = buildFolderChangeSet(wasm, [
        { t: "meta", hlc, folderIdHex: id, name, includeKinds: INCLUDE_GROUPS, excludeFlags: 0 },
      ]);
      s.applyFolderChanges(cs);
      cs.free();
    };
    meta(local, 500n, F1, "Local newer");
    meta(backup, 100n, F1, "Backup older");
    meta(backup, 100n, F2, "Only in backup");
    mergeRestoredFold(local, backup);
    const p = projectSyncedState(local);
    const byId = new Map(p.folders.map((f) => [f.idHex, f.name]));
    expect(byId.get(F1)).toBe("Local newer");
    expect(byId.get(F2)).toBe("Only in backup");
  });

  it("an empty local fold takes the backup whole, folder hand lists included", () => {
    const local = new wasm.SyncedState();
    const backup = new wasm.SyncedState();
    const cs = buildFolderChangeSet(
      wasm,
      stampEdits(
        diffFolderDraft(FID, null, { ...newFolderDraft(G1), name: "Work" }),
        mint,
      ),
    );
    backup.applyFolderChanges(cs);
    cs.free();
    mergeRestoredFold(local, backup);
    const p = projectSyncedState(local);
    expect(p.folders[0]?.included.map((r) => r.id)).toEqual([G1.id]);
  });
});
