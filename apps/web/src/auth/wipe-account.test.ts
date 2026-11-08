// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Forget device and Delete account leave nothing of the account behind
 * (2026-09-30): the test is by inventory, not by listing.
 *
 * Every account-scoped store is seeded for two accounts on one browser
 * (`localStorage`, `sessionStorage` and every IndexedDB database, over the
 * in-memory fakes in `src/test/fake-storage.ts`), the wipe runs for one, and
 * then EVERYTHING left is walked: no key and no value may name the wiped
 * account in any of the encodings the stores use (standard base64, base64url,
 * hex), and every entry of the other account and every device setting must
 * still be there, unchanged.
 *
 * A second test scans the source for the storage names each module uses and
 * fails on one this file has not classified, so a store added later cannot
 * silently fall outside the wipe.
 *
 * Not reachable here: the real browser engines (the fakes implement what the
 * stores call, no more), the service worker's app-shell cache (code, no
 * account data), and the server-side half of Delete account.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { base64Encode, base64UrlEncodeNoPad } from "./account-id";
import {
  accountWipeSteps,
  STAGED_WIPE_KEEPS,
  stagedWipeSteps,
  wipeAccountLocalData,
  wipeAccountStaged,
} from "./wipe-account";
import { savePersistedAccount, loadPersistedAccount } from "./persisted-account";
import { markSignupPhraseOwed } from "./phrase-gate";
import {
  dismissBanner,
  markProtectStepOwed,
  setPhraseProtected,
} from "./protect/account-protection";
import { markOwnAction, startNoticeCursor } from "./protect/notices";
import { markFirstPasswordOwed } from "./username-claim";
import { writeRenameOwed } from "./username-change";
import { markThreeWordsSuspended, saveBudget } from "./recover/three-words";
import { writeKeyResetOwed } from "./key-reset-owed";
import { writeGenesisRecorded, writeKnownDeviceId } from "./session-device";
import { markLogoutOwed } from "./logout-owed";
import { newRemoval, writeRemoval } from "../devices/removal";
import { markRotatedNotice, newRotation, writeRotation } from "../devices/key-rotation";
import { markOwnerMoved } from "../channels/owner-moved";
import { openAccountStores, resetAccountStoreGateForTests } from "./account-store-gate";
import { guestPhraseKey, markGuestAccount } from "./guest-account";
import { upsertAccepted } from "../contacts/local-store";
import { rememberInviteLinkSecret } from "../contacts/invite-link-secrets";
import { writeKnockCursor } from "../chat/knock-cursor";
import { saveKnownGroupIds } from "../devices/state-bundle";
import {
  addRestoredKnownGroups,
  loadRestoredKnownGroups,
} from "../backup/restored-known-groups";
import { isBackupRestoreOwed, markBackupRestoreOwed } from "../backup/restore-owed";
import { markRestoreFinished } from "../backup/restored-marker";
import { localStorageAnswerLog } from "../devices/device-link";
import { saveAppointmentRecord } from "../ds/appointments";
import { recordRegistrarListEntry } from "../ds/registrar-list";
import { markHandoverOwed } from "../ds/root-handover";
import { markRootHeld } from "../ds/root-held";
import { selectTab } from "../chat/folders/selected-tab";
import { observeMembers } from "../profile/membership-baseline";
import { recordProfileRequest } from "../profile/profile-request";
import { recordGrantSignature } from "../profile/grant-signatures";
import { saveRecent, saveTonePrefs } from "../media/emoji/emoji-index";
import { saveCounter } from "../channels/publish-counter-store";
import { writeFeedSelection } from "../feed/feed-selection-store";

// Two accounts. A's bytes give `+` and `/` in standard base64, so its
// base64url form differs and both forms are searched for.
const A_BYTES = new Uint8Array(16).fill(0xfb);
const B_BYTES = new Uint8Array(16).fill(0x5a);
const A = base64Encode(A_BYTES);
const B = base64Encode(B_BYTES);
const A_URL = base64UrlEncodeNoPad(A_BYTES);
const B_URL = base64UrlEncodeNoPad(B_BYTES);
const hex = (b: Uint8Array): string => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
const A_HEX = hex(A_BYTES);
const G = "Z3JvdXAtb25lLWlkLWJ5dGVzLTMyLWxvbmctIQ=="; // a group id (base64)

let local: MemoryStorage;
let session: MemoryStorage;
let idb: FakeIndexedDb;

beforeEach(() => {
  local = new MemoryStorage();
  session = new MemoryStorage();
  idb = new FakeIndexedDb();
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = local;
  g["sessionStorage"] = session;
  g["indexedDB"] = idb;
  // A wipe seals the account's stores until it lands again
  // (`account-store-gate.ts`); every test starts with nothing sealed.
  resetAccountStoreGateForTests();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["sessionStorage"];
  delete g["indexedDB"];
});

/** The IndexedDB databases and stores the wipe covers, with a row shaped the
 *  way that store files it. */
function idbRows(acct: string, acctUrl: string): { db: string; store: string; row: object }[] {
  const ct = new Uint8Array([1, 2, 3]);
  const byId = (db: string, store: string): { db: string; store: string; row: object } => ({
    db,
    store,
    row: { accountId: acct, ciphertext: ct },
  });
  return [
    byId("lakko-auth", "device_keys"),
    // ADR-0074 section 1.1: the recall record of a device whose account keys
    // were reset elsewhere.
    byId("lakko-recall-record", "records"),
    // ADR-0074 part 3: the predecessor seeds, the pending key-rotation
    // envelopes and a prepared rotation, sealed under the device key.
    { db: "lakko-key-rotation", store: "state", row: { accountId: acct, ciphertext: ct } },
    byId("lakko-mls", "snapshots"),
    { db: "lakko-mls", store: "commit-outbox", row: { id: `${acct}#1`, accountId: acct, commit: ct } },
    byId("lakko-message-log", "logs"),
    byId("lakko-channel-log", "logs"),
    byId("lakko-channel-post-log", "logs"),
    byId("lakko-feed-state", "state"),
    byId("lakko-feed-marks", "marks"),
    { db: "lakko-discover", store: "rows", row: { key: `${acct}|prefs`, ciphertext: ct } },
    { db: "lakko-discover", store: "rows", row: { key: `${acct}|shared`, ciphertext: ct } },
    byId("lakko-profiles", "rows"),
    // ADR-0084: the decrypted-avatar cache, one row per blob.
    { db: "lakko-avatars", store: "blobs", row: { id: `${acct}|blob1`, ciphertext: ct } },
    byId("lakko-group-names", "rows"),
    // ADR-0087: the small sealed rows (group photo, reactions, handle, knock
    // seeds, requests sent).
    { db: "lakko-account-kv", store: "rows", row: { id: `${acct}|group-photos`, ciphertext: ct } },
    // ADR-0087 section 13a: the kept channel ban lists and the channel
    // reactions waiting for a token (`channels/ban-list.ts`,
    // `channels/post-feedback.ts`).
    { db: "lakko-account-kv", store: "rows", row: { id: `${acct}|channel-ban-lists`, ciphertext: ct } },
    { db: "lakko-account-kv", store: "rows", row: { id: `${acct}|channel-reactions-waiting`, ciphertext: ct } },
    byId("lakko-group-closed", "rows"),
    byId("lakko-membership-end", "rows"),
    byId("lakko-registrar-seeds", "rows"),
    byId("lakko-pending-root-seeds", "rows"),
    byId("lakko-pins", "rows"),
    // ADR-0082: one row per result, filed under a digest.
    {
      db: "lakko-translations",
      store: "rows",
      row: { id: `t-${acct}`, accountId: acct, cls: "result", ciphertext: ct },
    },
    { db: "lakko-reach", store: "rows", row: { key: `${acct}|book`, ciphertext: ct } },
    { db: "lakko-reach", store: "rows", row: { key: `${acct}|grants`, ciphertext: ct } },
    byId("lakko-read-watermarks", "rows"),
    byId("lakko-call-log", "rows"),
    // ADR-0089: the person's own background photos, sealed under the device key.
    { db: "lakko-call-backgrounds", store: "photos", row: { id: `${acct}|p1`, addedAt: 1, ciphertext: ct } },
    byId("lakko-device-sync", "rows"),
    { db: "lakko-minted-links", store: "rows", row: { key: `${acct}|contact|slug1`, ciphertext: ct } },
    { db: "lakko-minted-links", store: "rows", row: { key: `${acct}|call|slug2`, ciphertext: ct } },
    byId("lakko-sent-request-intros", "rows"),
    byId("lakko-pending-contacts", "rows"),
    byId("lakko-carried-copies", "rows"),
    // ADR-0073 R.8: the service worker's plaintext copy of the retired keys.
    { db: "lakko-superseded-keys", store: "rows", row: { accountId: acct, keys: ["a2V5"] } },
    {
      db: "lakko-push-routing",
      store: "secrets",
      row: { key: `${acct}|${G}`, accountId: acct, groupIdB64: G, ciphertext: ct },
    },
    {
      db: "lakko-push-routing",
      store: "account_routes",
      row: { key: `${acct}|ab12`, pushGidHex: "ab12", accountId: acct, groupIdB64: G },
    },
    {
      db: "lakko-anon-tokens",
      store: "pools",
      row: { account: acctUrl, context: "discovery-lookup", tokens: [] },
    },
    byId("lakko-session-tokens", "tokens"),
    byId("lakko.backup.phrase", "phrases"),
    // A guest account's phrase, under its own key (`guest-account.ts`).
    {
      db: "lakko.backup.phrase",
      store: "phrases",
      row: { accountId: guestPhraseKey(acct), ciphertext: new Uint8Array([1]), cachedAtMs: 1 },
    },
    byId("lakko.backup.floor", "floors"),
    byId("lakko-export-keys", "keys"),
    byId("lakko-device-identity", "seeds"),
    byId("lakko-account-identity-seed", "seeds"),
    byId("lakko-device-group-snapshots", "snapshots"),
  ];
}

/** Seed every account-scoped store for one account, through the store's own
 *  writer where it has a plain one and in its documented key format where
 *  not. */
function seedAccount(acct: string, acctUrl: string): void {
  for (const { db, store, row } of idbRows(acct, acctUrl)) idb.seed(db, store, row);

  const put = (k: string, v: string): void => local.setItem(k, v);
  upsertAccepted(
    { signaturePubkeyB64: "cGVlcg==", displayName: "Peer", acceptedAtMs: 1 },
    acct,
  );
  put(`lakko.contacts.inbox.v1.${acct}`, JSON.stringify({ cursor: 3, pending: [] }));
  put(`lakko.contacts.sent.v1:${acct}`, JSON.stringify([{ recipientUsername: "bob", recipientAccountIdHex: "00", sentAtMs: 1 }]));
  put(
    `lakko.channels.pending-publisher.v1:${acct}`,
    JSON.stringify([{ channelIdHex: "07", groupIdHex: "ab", joinerKeyB64: "k", name: "", description: "", requestedAtMs: 1 }]),
  );
  rememberInviteLinkSecret(acctUrl, "slug1", "fragmentfragment");
  put(`lakko.readmission.removed.v1:${acct}`, JSON.stringify({ [G]: ["k"] }));
  markSignupPhraseOwed(local, acct);
  // ADR-0075: how the account is protected as this device knows it, and
  // what it saw and did of the account's notices.
  setPhraseProtected(local, acct, true);
  dismissBanner(local, acct, 1);
  markProtectStepOwed(local, acct);
  startNoticeCursor(local, acct, 3);
  markOwnAction(local, acct, "recovery-email-added", 1);
  saveBudget(local, acct, { misses: 2, pausedUntilMs: null });
  markThreeWordsSuspended(local, acct);
  writeKeyResetOwed(local, acct, {
    v: 1,
    attestationB64Url: "AAAA",
    keyGeneration: "2",
    newSignaturePubkeyB64: "AAAA",
    signedAtMs: 1,
    done: { directory: false, deviceGroup: false },
  });
  // ADR-0074: the device id the sessions name, the "account key recorded at
  // auth" mark, a logout still owed, and a device removal still finishing.
  writeKnownDeviceId(local, acct, "B".repeat(43));
  writeGenesisRecorded(local, acct, "a2V5");
  markLogoutOwed(local, acct);
  writeRemoval(local, acct, "C".repeat(43), newRemoval("C".repeat(43), 1));
  // ADR-0074 part 3: a key rotation this device was finishing, the "keys
  // were changed" line, the predecessor mailbox's cursor and the channels
  // whose owner key moved.
  writeRotation(local, acct, newRotation("C".repeat(43), 1));
  markRotatedNotice(local, acct);
  put(`lakko.contacts.predecessor-mailbox.v2.${acct}`, "7");
  put(`lakko.contacts.predecessor-mailbox-adopted.v2.${acct}`, "1700000000000");
  put(
    `lakko.contacts.rotation-spend-owed.v1.${acct}`,
    JSON.stringify({ adoptedAtMs: 1700000000000, links: true, requests: true }),
  );
  markOwnerMoved(acct, "c0ffee", local);
  markGuestAccount(local, acct, 1);
  put(`lakko.pending-requests.v1.${acct}.${G}`, "{}");
  put(`lakko.unapplied-requests.v1.${acct}.${G}`, "[]");
  put(`lakko.own-requests.v1.${acct}.${G}`, "{}");
  put(`lakko.committer-beacons.v1.${acct}.${G}`, "{}");
  put(`lakko.last-observed-owner.v1.${acct}.${G}`, "\"owner\"");
  put(`lakko.leave-requested.v1.${acct}.${G}`, "5");
  put(`lakko.lineage.unsent.v1.${acct}`, "1");
  put(`lakko.quick-reaction.unsent.v1.${acct}`, "1");
  put(`lakko.channel-feedback.unsent.v1.${acct}`, "1");
  put(`lakko.unseen-reactions.v1:${acct}`, "{}");
  put(`lakko.emoji.recent-reactions:${acct}`, "[]");
  put(`lakko.device-cards.v1.${acct}`, "{}");
  put(`lakko.device-city.v1.${acct}`, "{}");
  put(`lakko.device-city-shown.v1.${acct}`, "0");
  put(`lakko.recovery-email.unsent.v1.${acct}`, "1");
  markFirstPasswordOwed(local, acct);
  put(`lakko.own-username.unsent.v1.${acct}`, "1");
  put(`lakko.device-ack.unsent.v1.${acct}`, "1");
  put(`lakko.link-records.unsent.v1.${acct}`, "1");
  put(`lakko.link-claims.v1.${acct}`, JSON.stringify({ v: 1, seen: {}, own: {} }));
  put(`lakko.blob-deletes-owed.v1.${acct}`, JSON.stringify(["00".repeat(32)]));
  put(`lakko.blob-keeps.v1.${acct}`, JSON.stringify({ v: 1, last: {} }));
  put(`lakko.link-row-deletions-owed.v1.${acct}`, JSON.stringify(["00"]));
  writeRenameOwed(local, acct, { v: 1, oldUsername: "alice", newUsername: "alicia", phase: "binding" });
  put(
    `lakko.contacts.verified-bindings.v1.${acct}`,
    JSON.stringify({ QUFB: { keyB64: "a2V5", keyGeneration: 1, verifiedAtMs: 1 } }),
  );
  put(`lakko.reset-continuity.v1.${acct}`, JSON.stringify({ moved: { G: 1 }, resent: {} }));
  put(`lakko.channels.orphaned-by-reset.v1.${acct}`, JSON.stringify(["ch1"]));
  put(`lakko.channels.claimed-handles.v1.${acct}`, JSON.stringify({ news: "ch2" }));
  put(`lakko.conversation-kind.v1.${acct}`, JSON.stringify({ [G]: "group" }));
  selectTab(acct, "folder-1");
  writeFeedSelection(acct, "custom:feed-1");
  put(`lakko.profile.own.v1.${acct}`, JSON.stringify({ displayName: "Me", updatedAtMs: 1 }));
  observeMembers(acct, G, new Set(["member-key"]));
  recordProfileRequest(acct, G, 1000);
  recordGrantSignature(acct, G, "member:1|group:1");
  localStorageAnswerLog(acct).save([1234]);
  put(
    `lakko/devices/added-notice/v1/${acct}`,
    JSON.stringify({ v: 1, known: ["aa"], letIn: [], pending: [{ key: "bb", atMs: 1 }] }),
  );
  saveAppointmentRecord(acct, G, new Map([["registrar", 1]]));
  recordRegistrarListEntry(acct, G, "YWNjb3VudA==", { publicKey: new Uint8Array(32).fill(9), rank: 1 });
  markHandoverOwed(acct, G);
  markRootHeld(acct, G);
  put(`lakko.backup.auto.${acct}`, JSON.stringify({ lastUploadAtMs: 1, dirtySinceMs: null, groupsKey: null }));
  writeKnockCursor(G, 7, acct);
  saveKnownGroupIds([G], acct);
  addRestoredKnownGroups(acct, [G]);
  markBackupRestoreOwed(acct);
  markRestoreFinished(acct, 1);
  put(`lakko.channels.rotation-state.v2.${acct}.c0ffee`, "{}");
  saveCounter("c0ffee", 42n, acct);
  put(`lakko/publisher-group-unclaimed/v2/${acct}/c0ffee`, "1");
  saveRecent(["\u{1F600}"], local, acct);
  saveTonePrefs({ "\u{1F44B}": "\u{1F44B}\u{1F3FF}" }, local, acct);
  session.setItem(`lakko:group-calls:${acct}:${G}`, "[]");
}

/** Genuine device settings, which no wipe may touch. */
function seedDeviceSettings(): void {
  local.setItem("lakko.theme", "dark");
  local.setItem("lakko.transparency.last-seen-root.v1", "{\"size\":3}");
  local.setItem("lakko.push.token.v1", "cHNldWRv");
  local.setItem("lakko.composer.recordMode", "voice");
  local.setItem("lakko.media.autoplayVideoNotes", "1");
  local.setItem("lakko.feed.device-banner-dismissed", "1");
}

/** Retired device-wide rows that may still hold the wiped account's data. */
function seedRetired(acct: string): string[] {
  const rows: [string, string][] = [
    ["lakko.contacts.device-mailbox.v1", "12"],
    ["lakko.channels.subscriber-keys.v1", JSON.stringify({ owner: acct })],
    ["lakko.contacts.v1", JSON.stringify({ owner: acct })],
    ["lakko.contacts.v2", JSON.stringify({ owner: acct })],
    [`lakko.ds.knock-cursor.v1:${G}`, "3"],
    ["lakko.pairing.known-groups.v1", JSON.stringify([G])],
    ["lakko.channels.rotation-state.v1.c0ffee", "{}"],
    ["lakko.channels.publish-counter.v1.c0ffee", "42"],
    ["lakko/publisher-group-unclaimed/v1/c0ffee", "1"],
    ["lakko.emoji.recent", JSON.stringify(["\u{1F600}"])],
    ["lakko.emoji.tones", "{}"],
  ];
  for (const [k, v] of rows) local.setItem(k, v);
  return rows.map(([k]) => k);
}

/** Every stored entry, as one searchable text per entry. */
function inventory(): { where: string; text: string }[] {
  return [
    ...local.entries().map(([k, v]) => ({ where: `localStorage/${k}`, text: `${k} ${v}` })),
    ...session.entries().map(([k, v]) => ({ where: `sessionStorage/${k}`, text: `${k} ${v}` })),
    ...idb.walk(),
  ];
}

const namesA = (text: string): boolean =>
  text.includes(A) || text.includes(A_URL) || text.includes(A_HEX);
const namesB = (text: string): boolean => text.includes(B) || text.includes(B_URL);

/** Create every IndexedDB schema through the stores' own upgrade code, by
 *  running the wipe once for an account nobody has. */
async function createSchemas(): Promise<void> {
  const report = await wipeAccountLocalData(base64Encode(new Uint8Array(16).fill(0x01)));
  expect(report.failed).toEqual([]);
}

describe("wipeAccountLocalData", () => {
  it("leaves nothing that names the wiped account, and everything of the other account", async () => {
    await createSchemas();
    seedAccount(A, A_URL);
    seedAccount(B, B_URL);
    seedDeviceSettings();
    const retired = seedRetired(A);

    const before = inventory();
    // Every IndexedDB store the wipe covers holds a row of each account, and
    // every localStorage / sessionStorage row above landed.
    for (const { db, store } of idbRows(A, A_URL)) {
      const where = `${db}/${store}`;
      expect(before.some((e) => e.where === where && namesA(e.text)), where).toBe(true);
      expect(before.some((e) => e.where === where && namesB(e.text)), where).toBe(true);
    }
    const aBefore = before.filter((e) => namesA(e.text));
    const bBefore = before.filter((e) => namesB(e.text) && !namesA(e.text));
    expect(aBefore.length).toBeGreaterThan(50);
    expect(bBefore.length).toBeGreaterThan(50);

    const report = await wipeAccountLocalData(A);
    expect(report.failed).toEqual([]);

    const after = inventory();
    const leftovers = after.filter((e) => namesA(e.text)).map((e) => e.where);
    expect(leftovers).toEqual([]);
    // The other account's data is untouched, entry for entry.
    const afterTexts = new Set(after.map((e) => `${e.where} ${e.text}`));
    const bMissing = bBefore
      .filter((e) => !afterTexts.has(`${e.where} ${e.text}`))
      .map((e) => e.where);
    expect(bMissing).toEqual([]);
    // Device settings stay; retired device-wide rows go.
    for (const k of [
      "lakko.theme",
      "lakko.transparency.last-seen-root.v1",
      "lakko.push.token.v1",
      "lakko.composer.recordMode",
      "lakko.media.autoplayVideoNotes",
    ]) {
      expect(local.getItem(k), k).not.toBeNull();
    }
    for (const k of retired) expect(local.getItem(k), k).toBeNull();
  });

  it("drops the restore marker only when it names the wiped account", async () => {
    await createSchemas();
    savePersistedAccount({ v: 1, accountIdB64: B, username: "bob" });
    await wipeAccountLocalData(A);
    expect(loadPersistedAccount()?.accountIdB64).toBe(B);

    // The account lands on this browser again (the first wipe sealed it).
    openAccountStores([A]);
    savePersistedAccount({ v: 1, accountIdB64: A, username: "alice" });
    await wipeAccountLocalData(A);
    expect(loadPersistedAccount()).toBeNull();
    expect(local.getItem("lakko.auth.v1")).toBeNull();
  });

  it("keeps going when a store fails, and names the failed step", async () => {
    await createSchemas();
    seedAccount(A, A_URL);
    // Every IndexedDB open now fails; the localStorage steps must still run.
    (globalThis as unknown as Record<string, unknown>)["indexedDB"] = {
      open(): never {
        throw new Error("blocked");
      },
    };
    const warn = console.warn;
    const logged: string[] = [];
    console.warn = (...args: unknown[]): void => {
      logged.push(args.map(String).join(" "));
    };
    let report;
    try {
      report = await wipeAccountLocalData(A);
    } finally {
      console.warn = warn;
    }
    expect(report.failed.length).toBeGreaterThan(0);
    for (const line of logged.filter((l) => l.startsWith("[wipe]"))) {
      // Plain ASCII, no account id.
      expect(/^[\x20-\x7e]*$/.test(line), line).toBe(true);
      expect(namesA(line), line).toBe(false);
    }
    // The one exception: the channel publish counter goes only after the
    // channel log holding the signing seeds it guards is deleted, so with
    // that delete failed it stays (a counter removed beside a surviving
    // seed could rewind under it).
    expect(report.failed).toContain("channel-log");
    const counterRow = `lakko.channels.publish-counter.v2.${A}.c0ffee`;
    expect(local.getItem(counterRow)).toBe("42");
    const localLeft = local
      .entries()
      .filter(([k, v]) => k !== counterRow && namesA(`${k} ${v}`));
    expect(localLeft).toEqual([]);
    expect(session.entries().filter(([k]) => namesA(k))).toEqual([]);
  });

  // One full wipe per step left out, each on freshly seeded stores: about a
  // hundred wipes. It took most of the suite's 20 s timeout on a quiet
  // machine and ran past it on a loaded one, and the run it left behind then
  // failed the next tests on the shared stores. Its own budget is generous.
  it("needs every step: leaving any one out leaves the account behind", async () => {
    await createSchemas();
    const steps = accountWipeSteps(A);
    const notLoadBearing: string[] = [];
    for (const skipped of steps.map((s) => s.name)) {
      idb.clearRows();
      local.clear();
      session.clear();
      seedAccount(A, A_URL);
      const retired = seedRetired(A);
      savePersistedAccount({ v: 1, accountIdB64: A, username: "alice" });
      for (const step of accountWipeSteps(A)) {
        if (step.name !== skipped) await step.run();
      }
      const left =
        inventory().some((e) => namesA(e.text)) || retired.some((k) => local.getItem(k) !== null);
      if (!left) notLoadBearing.push(skipped);
    }
    expect(notLoadBearing).toEqual([]);
  }, 180_000);

  // ADR-0074 section 1.1 (UX 18.4): the staged wipe of a device whose
  // account keys were reset on another device.
  it("staged: deletes what the full wipe deletes, minus the recall record and its device key", async () => {
    await createSchemas();
    seedAccount(A, A_URL);
    seedAccount(B, B_URL);
    seedRetired(A);
    savePersistedAccount({ v: 1, accountIdB64: A, username: "alice" });
    const before = inventory().filter((e) => namesB(e.text) && !namesA(e.text));

    const report = await wipeAccountStaged(A);
    expect(report.failed).toEqual([]);

    // All that still names the account: the two kept rows.
    const left = inventory()
      .filter((e) => namesA(e.text))
      .map((e) => e.where)
      .sort();
    expect(left).toEqual(["lakko-auth/device_keys", "lakko-recall-record/records"]);
    // The other account is untouched.
    const after = inventory().filter((e) => namesB(e.text) && !namesA(e.text));
    expect(after).toEqual(before);

    // The rest of the wipe (the record destroyed) then leaves nothing.
    await wipeAccountLocalData(A);
    expect(inventory().filter((e) => namesA(e.text))).toEqual([]);
  });

  it("staged: the step list is the full list minus the kept steps, in the same order", () => {
    const full = accountWipeSteps(A).map((s) => s.name);
    const staged = stagedWipeSteps(A).map((s) => s.name);
    expect(STAGED_WIPE_KEEPS).toEqual(["device-key", "recall-record"]);
    expect(staged).toEqual(full.filter((name) => !STAGED_WIPE_KEEPS.includes(name)));
    expect(full.length - staged.length).toBe(STAGED_WIPE_KEEPS.length);
    for (const kept of STAGED_WIPE_KEEPS) expect(full).toContain(kept);
  });

  it("full: a removed device (`device-revoked`) is left no recall record", async () => {
    await createSchemas();
    seedAccount(A, A_URL);
    expect(idb.walk().some((e) => e.where === "lakko-recall-record/records" && namesA(e.text))).toBe(true);
    await wipeAccountLocalData(A);
    expect(idb.walk().filter((e) => e.where === "lakko-recall-record/records" && namesA(e.text))).toEqual([]);
  });

  it("drops the restored known groups and the owed-restore flag; another account keeps its own", async () => {
    await createSchemas();
    addRestoredKnownGroups(A, [G]);
    addRestoredKnownGroups(B, [G]);
    markBackupRestoreOwed(A);
    markBackupRestoreOwed(B);
    await wipeAccountLocalData(A);
    expect(loadRestoredKnownGroups(A)).toEqual([]);
    expect(isBackupRestoreOwed(A)).toBe(false);
    expect(loadRestoredKnownGroups(B)).toEqual([G]);
    expect(isBackupRestoreOwed(B)).toBe(true);
  });

  it("names every step once", () => {
    const names = accountWipeSteps(A).map((s) => s.name);
    expect(new Set(names).size).toBe(names.length);
    expect(names[0]).toBe("device-key");
    expect(names[names.length - 1]).toBe("restore-marker");
  });
});

// ---------------------------------------------------------------------------
// Completeness: every storage name in the source is classified here.
// ---------------------------------------------------------------------------

/** What each storage name found in the source is. `account` names are seeded
 *  by the inventory test above and must be gone after the wipe; `device`
 *  names must stay; `retired` device-wide names are dropped by the wipe;
 *  `not-storage` literals merely sit in a file that also uses storage. */
const CLASSIFIED: Record<string, "account" | "device" | "retired" | "not-storage"> = {
  // IndexedDB
  "lakko-auth": "account",
  "lakko-recall-record": "account",
  "lakko-key-rotation": "account",
  // The Web Lock name of a recall action (`navigator.locks`), not a store.
  "lakko-recall:": "not-storage",
  "lakko-mls": "account",
  "lakko-message-log": "account",
  "lakko-channel-log": "account",
  "lakko-channel-post-log": "account",
  "lakko-feed-state": "account",
  "lakko-feed-marks": "account",
  "lakko-discover": "account",
  "lakko-profiles": "account",
  "lakko-avatars": "account",
  "lakko-group-names": "account",
  "lakko-group-closed": "account",
  "lakko-membership-end": "account",
  "lakko-registrar-seeds": "account",
  "lakko-pending-root-seeds": "account",
  "lakko-pins": "account",
  "lakko-translations": "account",
  // The domain of the digest a translation row is filed under, not a store.
  "lakko/translation/row/v1": "not-storage",
  // The domain of the keyed conversation tag on a translation row.
  "lakko/translation/conversation-tag/v1": "not-storage",
  "lakko-reach": "account",
  "lakko-read-watermarks": "account",
  "lakko-call-log": "account",
  "lakko-call-backgrounds": "account",
  "lakko-device-sync": "account",
  "lakko-minted-links": "account",
  "lakko-sent-request-intros": "account",
  "lakko-pending-contacts": "account",
  "lakko-carried-copies": "account",
  "lakko-superseded-keys": "account",
  "lakko-push-routing": "account",
  "lakko-anon-tokens": "account",
  "lakko-session-tokens": "account",
  "lakko.backup.phrase": "account",
  "lakko.backup.floor": "account",
  "lakko-export-keys": "account",
  "lakko-device-identity": "account",
  "lakko-account-identity-seed": "account",
  "lakko-device-group-snapshots": "account",
  // localStorage / sessionStorage, keyed by the account
  "lakko.auth.v1": "account",
  "lakko.contacts.v3.": "account",
  "lakko.contacts.inbox.v1.": "account",
  "lakko.contacts.sent.v1:": "account",
  "lakko.channels.pending-publisher.v1:": "account",
  "lakko/invite-link-secrets/v1/": "account",
  "lakko.readmission.removed.v1:": "account",
  "lakko:signup-phrase-unacknowledged:": "account",
  "lakko:phrase-protected:": "account",
  "lakko:protect-banner-dismissed:": "account",
  "lakko:protect-step-owed:": "account",
  "lakko:account-notices-seen:": "account",
  "lakko:account-notices-self:": "account",
  "lakko:three-words-budget:": "account",
  "lakko:three-words-suspended:": "account",
  "lakko:key-reset-owed:": "account",
  "lakko:device-id:": "account",
  "lakko:genesis-recorded:": "account",
  "lakko:logout-owed:": "account",
  "lakko:device-removal-owed:": "account",
  "lakko:key-rotation-owed:": "account",
  "lakko:key-rotation-notice:": "account",
  "lakko.contacts.predecessor-mailbox.v2.": "account",
  "lakko.contacts.predecessor-mailbox-adopted.v2.": "account",
  "lakko.contacts.rotation-spend-owed.v1.": "account",
  "lakko:channel-owner-moved:": "account",
  "lakko:guest-account:": "account",
  "lakko.pending-requests.v1": "account",
  "lakko.unapplied-requests.v1": "account",
  "lakko.own-requests.v1": "account",
  "lakko.committer-beacons.v1": "account",
  "lakko.last-observed-owner.v1": "account",
  "lakko.leave-requested.v1": "account",
  "lakko.lineage.unsent.v1.": "account",
  "lakko.device-cards.v1.": "account",
  "lakko.device-city.v1.": "account",
  "lakko.device-city-shown.v1.": "account",
  "lakko.recovery-email.unsent.v1.": "account",
  "lakko:first-password-owed:": "account",
  "lakko.own-username.unsent.v1.": "account",
  "lakko.device-ack.unsent.v1.": "account",
  "lakko.link-records.unsent.v1.": "account",
  "lakko.link-claims.v1.": "account",
  "lakko.blob-deletes-owed.v1.": "account",
  "lakko.blob-keeps.v1.": "account",
  "lakko.link-row-deletions-owed.v1.": "account",
  "lakko:username-change-owed:": "account",
  "lakko.contacts.verified-bindings.v1.": "account",
  "lakko.reset-continuity.v1.": "account",
  "lakko.channels.orphaned-by-reset.v1": "account",
  "lakko.channels.claimed-handles.v1.": "account",
  "lakko.conversation-kind.v1.": "account",
  "lakko.folderTab.": "account",
  "lakko.feed.selection.": "account",
  "lakko.profile.own.v1.": "account",
  "lakko.member-baseline.v1": "account",
  "lakko.profile-requests.v1": "account",
  "lakko.reach.profile-grants.v1.": "account",
  "lakko/device-link/answers/v1/": "account",
  "lakko/devices/added-notice/v1/": "account",
  "lakko/ds-appointments/v2/": "account",
  "lakko/ds-registrar-list/v1/": "account",
  "lakko/ds-root-handover/v1/": "account",
  "lakko/ds-root-held/v1/": "account",
  "lakko.backup.auto.": "account",
  "lakko.backup.restored-groups.v1.": "account",
  "lakko.backup.restore-owed.v1.": "account",
  "lakko.backup.restored-marker.v1.": "account",
  "lakko.ds.knock-cursor.v2:": "account",
  "lakko.pairing.known-groups.v2.": "account",
  "lakko.channels.rotation-state.v2.": "account",
  "lakko.channels.publish-counter.v2.": "account",
  "lakko/publisher-group-unclaimed/v2/": "account",
  "lakko:group-calls:": "account",
  "lakko.emoji.recent:": "account",
  "lakko.emoji.recent-reactions:": "account",
  "lakko.quick-reaction.unsent.v1.": "account",
  "lakko.channel-feedback.unsent.v1.": "account",
  "lakko.unseen-reactions.v1:": "account",
  "lakko-account-kv": "account",
  // Reactions section 12: this device's notification filter.
  "lakko.notify.reactions.off": "device",
  "lakko.emoji.tones:": "account",
  // retired device-wide rows
  "lakko.contacts.v1": "retired",
  "lakko.contacts.v2": "retired",
  "lakko.contacts.device-mailbox.v1": "retired",
  "lakko.channels.subscriber-keys.v1": "retired",
  "lakko.ds.knock-cursor.v1:": "retired",
  "lakko.pairing.known-groups.v1": "retired",
  "lakko.channels.rotation-state.v1.": "retired",
  "lakko.channels.publish-counter.v1.": "retired",
  "lakko/publisher-group-unclaimed/v1/": "retired",
  "lakko.emoji.recent": "retired",
  "lakko.emoji.tones": "retired",
  // device settings (see the module doc of wipe-account.ts)
  "lakko.theme": "device",
  // docs/ux/text-size.md: the message text size is chosen per device.
  "lakko.textSize": "device",
  // ADR-0061: the interface language is chosen per device.
  "lakko.language": "device",
  // ADR-0082 / UX section 6: the translation settings are per device.
  "lakko.translation.settings.v1": "device",
  "lakko.transparency.last-seen-root.v1": "device",
  // The store gate's cross-tab marker: digests of the accounts wiped in the
  // last minute, never an account id; it prunes itself.
  "lakko.account-store-gate.wiped.v1": "device",
  "lakko.push.token.v1": "device",
  "lakko.composer.recordMode": "device",
  // ADR-0089: the camera background chosen on this device, not synced.
  "lakko.call.background": "device",
  "lakko.media.autoplayVideoNotes": "device",
  "lakko.feed.device-banner-dismissed": "device",
  // A diagnostic switch of this device (`calls/av-align.ts`): `off` makes a
  // call without audio/video alignment. Names no account.
  "lakko.call.av-align": "device",
  // ADR-0088 / docs/ux/calls.md 4.1.3: the noise suppression switch is
  // chosen per device and names no account.
  "lakko.call.noise-suppression": "device",
  // not storage names
  // The notification tag of identity-recovery 17.5 (`added-notice.ts`).
  "lakko-device-added:": "not-storage",
  "lakko/state-bundle-row/v1": "not-storage",
  "lakko/state-bundle/v1": "not-storage",
  "lakko/feed-marks/v1": "not-storage",
  "lakko/channel-post-log/v1": "not-storage",
};

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (name === "test") continue;
      out.push(...sourceFiles(p));
    } else if (/\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name)) {
      out.push(p);
    }
  }
  return out;
}

describe("storage names in the source", () => {
  it("are all classified", () => {
    const root = join(__dirname, "..");
    const unclassified: string[] = [];
    for (const file of sourceFiles(root)) {
      const code = readFileSync(file, "utf8")
        .split("\n")
        .filter((l) => !/^\s*(\*|\/\/|\/\*)/.test(l))
        .join("\n");
      // A module that touches storage directly, or takes an injected one.
      if (!/\b(localStorage|sessionStorage|indexedDB)\b|\.getItem\(/.test(code)) continue;
      for (const m of code.matchAll(/["'`](lakko[.:/_-][^"'`$\\]*)/g)) {
        const lit = m[1] as string;
        if (!(lit in CLASSIFIED)) unclassified.push(`${relative(root, file)}: ${lit}`);
      }
    }
    expect(unclassified).toEqual([]);
  });

  it("every IndexedDB database classified as account data is seeded above", () => {
    const seeded = new Set(idbRows(A, A_URL).map((r) => r.db));
    const accountDbs = Object.entries(CLASSIFIED)
      .filter(([name, kind]) => kind === "account" && /^lakko[-.][a-z.-]+$/.test(name) && !name.includes(".v"))
      .map(([name]) => name);
    // Standard localStorage prefixes end in `.`, `:` or `/`, or carry a
    // version; what remains are database names.
    for (const db of accountDbs) {
      if (db.endsWith(".") || db.endsWith(":") || db.endsWith("/")) continue;
      expect(seeded.has(db), db).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// Order inside Forget device: the session's writers stop before the wipe.
// ---------------------------------------------------------------------------

describe("Forget device", () => {
  // Seen live 2026-10-01: the wipe ran while the session was still live, the
  // one-to-one contact adoption reacted to the profiles store being cleared
  // and wrote the peer back into the contact list the wipe had just emptied.
  // AuthContext needs a Solid runtime this suite does not have, so the order
  // is pinned on the source of `forgetDevice`.
  it("unbinds the contact list and drops the live groups before wiping", () => {
    const source = readFileSync(join(__dirname, "AuthContext.tsx"), "utf8");
    const start = source.indexOf("async forgetDevice()");
    expect(start).toBeGreaterThan(-1);
    const body = source.slice(start);
    const wipe = body.indexOf("await wipeAccountLocalData(");
    expect(wipe).toBeGreaterThan(-1);
    const unbind = body.indexOf("bindContactsAccount(null)");
    const clear = body.indexOf("chats.clear()");
    expect(unbind).toBeGreaterThan(-1);
    expect(unbind).toBeLessThan(wipe);
    expect(clear).toBeGreaterThan(-1);
    expect(clear).toBeLessThan(wipe);
  });
});
