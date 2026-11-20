// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 / `docs/ux/deleted-account.md` section 3, Notifications — a
 * background notification names a deleted sender "Deleted account": the app
 * writes the word (device language) into the profile row's `x` mirror for
 * every key of a deleted person, and the service worker reads it before a
 * rename, a profile, a profile the batch carried and a label. When a
 * pull-derived deletion is reverted the app writes the row again without it,
 * and the old name comes back.
 */
import { describe, expect, it } from "vitest";

// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { senderNameFromProfileRow } from "../../public/sw-push-decrypt.js";
import { deletedLabelsOf, type LabelledMember } from "../profile/member-label";
import { encodeProfileRow } from "../profile/profile-idb";
import type { ProfileRow } from "../profile/profile-store";

const enc = (b: Uint8Array): string => Buffer.from(b).toString("base64");
const ACCOUNT = new Uint8Array(32).fill(1);
const LEAF = new Uint8Array(32).fill(2);
const ACCOUNT_B64 = enc(ACCOUNT);
const LEAF_B64 = enc(LEAF);
const DELETED = "Deleted account";

const roster: LabelledMember[] = [{ signaturePublicKey: LEAF, accountKey: ACCOUNT }];

function rowFor(deleted: ReadonlyMap<string, string>): unknown {
  const row = {
    profiles: new Map([
      [
        ACCOUNT_B64,
        { displayName: "Ann", username: "ann", updatedAtMs: 5n, accountIdHex: null },
      ],
    ]),
    verifications: new Map(),
  } as unknown as ProfileRow;
  return JSON.parse(
    encodeProfileRow(row, new Map([[ACCOUNT_B64, "My Ann"]]), new Map([[LEAF_B64, "Ann"]]), deleted),
  );
}

describe("the deleted-name mirror the service worker reads", () => {
  it("covers the account key and every leaf key of a deleted person", () => {
    const labels = deletedLabelsOf([roster], (k) => k === ACCOUNT_B64, enc, [], DELETED);
    expect(labels.get(ACCOUNT_B64)).toBe(DELETED);
    expect(labels.get(LEAF_B64)).toBe(DELETED);
  });

  it("is empty for a person who is not deleted", () => {
    expect(deletedLabelsOf([roster], () => false, enc, [ACCOUNT_B64], DELETED).size).toBe(0);
  });

  it("names the sender Deleted account, over a rename, a profile and a batch profile", () => {
    const payload = rowFor(deletedLabelsOf([roster], (k) => k === ACCOUNT_B64, enc, [], DELETED));
    expect(senderNameFromProfileRow(payload, ACCOUNT_B64)).toBe(DELETED);
    expect(senderNameFromProfileRow(payload, LEAF_B64)).toBe(DELETED);
    const batch = { displayName: "Ann New", username: null, updatedAtMs: 99n };
    expect(senderNameFromProfileRow(payload, ACCOUNT_B64, batch)).toBe(DELETED);
  });

  it("gives the old name back once the row is written without the person", () => {
    const payload = rowFor(deletedLabelsOf([roster], () => false, enc, [], DELETED));
    expect((payload as { x?: unknown }).x).toBeUndefined();
    expect(senderNameFromProfileRow(payload, ACCOUNT_B64)).toBe("My Ann");
  });
});
