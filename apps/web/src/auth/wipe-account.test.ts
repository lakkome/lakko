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
import { accountWipeSteps, wipeAccountLocalData } from "./wipe-account";
import { savePersistedAccount, loadPersistedAccount } from "./persisted-account";
import { markSignupPhraseOwed } from "./phrase-gate";
import { upsertAccepted } from "../contacts/local-store";
import { rememberInviteLinkSecret } from "../contacts/invite-link-secrets";
import { writeKnockCursor } from "../chat/knock-cursor";
import { saveKnownGroupIds } from "../devices/state-bundle";
import { localStorageAnswerLog } from "../devices/device-link";
import { saveAppointmentRecord } from "../ds/appointments";
import { markHandoverOwed } from "../ds/root-handover";
import { markRootHeld } from "../ds/root-held";
import { selectTab } from "../chat/folders/selected-tab";
import { observeMembers } from "../profile/membership-baseline";
import { recordProfileRequest } from "../profile/profile-request";
import { recordGrantSignature } from "../profile/grant-signatures";
import { saveRecent, saveTonePrefs } from "../media/emoji/emoji-index";
import { saveCounter } from "../channels/publish-counter-store";

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
    byId("lakko-mls", "snapshots"),
    { db: "lakko-mls", store: "commit-outbox", row: { id: `${acct}#1`, accountId: acct, commit: ct } },
    byId("lakko-message-log", "logs"),
    byId("lakko-channel-log", "logs"),
    byId("lakko-profiles", "rows"),
    byId("lakko-group-names", "rows"),
    byId("lakko-registrar-seeds", "rows"),
    byId("lakko-pending-root-seeds", "rows"),
    byId("lakko-pins", "rows"),
    { db: "lakko-reach", store: "rows", row: { key: `${acct}|book`, ciphertext: ct } },
    { db: "lakko-reach", store: "rows", row: { key: `${acct}|grants`, ciphertext: ct } },
    byId("lakko-read-watermarks", "rows"),
    byId("lakko-call-log", "rows"),
    byId("lakko-device-sync", "rows"),
    { db: "lakko-minted-links", store: "rows", row: { key: `${acct}|contact|slug1`, ciphertext: ct } },
    { db: "lakko-minted-links", store: "rows", row: { key: `${acct}|call|slug2`, ciphertext: ct } },
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
  rememberInviteLinkSecret(acctUrl, "slug1", "fragmentfragment");
  put(`lakko.readmission.removed.v1:${acct}`, JSON.stringify({ [G]: ["k"] }));
  markSignupPhraseOwed(local, acct);
  put(`lakko.pending-requests.v1.${acct}.${G}`, "{}");
  put(`lakko.unapplied-requests.v1.${acct}.${G}`, "[]");
  put(`lakko.committer-beacons.v1.${acct}.${G}`, "{}");
  put(`lakko.last-observed-owner.v1.${acct}.${G}`, "\"owner\"");
  put(`lakko.leave-requested.v1.${acct}.${G}`, "5");
  put(`lakko.conversation-kind.v1.${acct}`, JSON.stringify({ [G]: "group" }));
  selectTab(acct, "folder-1");
  put(`lakko.profile.own.v1.${acct}`, JSON.stringify({ displayName: "Me", updatedAtMs: 1 }));
  observeMembers(acct, G, new Set(["member-key"]));
  recordProfileRequest(acct, G, 1000);
  recordGrantSignature(acct, G, "member:1|group:1");
  localStorageAnswerLog(acct).save([1234]);
  saveAppointmentRecord(acct, G, new Map([["registrar", 1]]));
  markHandoverOwed(acct, G);
  markRootHeld(acct, G);
  put(`lakko.backup.auto.${acct}`, JSON.stringify({ lastUploadAtMs: 1, dirtySinceMs: null, groupsKey: null }));
  writeKnockCursor(G, 7, acct);
  saveKnownGroupIds([G], acct);
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
  local.setItem("lakko.push.device.v1", "cHNldWRv");
  local.setItem("lakko.composer.recordMode", "voice");
  local.setItem("lakko.media.autoplayVideoNotes", "1");
  local.setItem("lakko.gif.privacyNoteDismissed", "1");
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
      "lakko.push.device.v1",
      "lakko.composer.recordMode",
      "lakko.media.autoplayVideoNotes",
      "lakko.gif.privacyNoteDismissed",
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
  "lakko-mls": "account",
  "lakko-message-log": "account",
  "lakko-channel-log": "account",
  "lakko-profiles": "account",
  "lakko-group-names": "account",
  "lakko-registrar-seeds": "account",
  "lakko-pending-root-seeds": "account",
  "lakko-pins": "account",
  "lakko-reach": "account",
  "lakko-read-watermarks": "account",
  "lakko-call-log": "account",
  "lakko-device-sync": "account",
  "lakko-minted-links": "account",
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
  "lakko/invite-link-secrets/v1/": "account",
  "lakko.readmission.removed.v1:": "account",
  "lakko:signup-phrase-unacknowledged:": "account",
  "lakko.pending-requests.v1": "account",
  "lakko.unapplied-requests.v1": "account",
  "lakko.committer-beacons.v1": "account",
  "lakko.last-observed-owner.v1": "account",
  "lakko.leave-requested.v1": "account",
  "lakko.conversation-kind.v1.": "account",
  "lakko.folderTab.": "account",
  "lakko.profile.own.v1.": "account",
  "lakko.member-baseline.v1": "account",
  "lakko.profile-requests.v1": "account",
  "lakko.reach.profile-grants.v1.": "account",
  "lakko/device-link/answers/v1/": "account",
  "lakko/ds-appointments/v1/": "account",
  "lakko/ds-root-handover/v1/": "account",
  "lakko/ds-root-held/v1/": "account",
  "lakko.backup.auto.": "account",
  "lakko.ds.knock-cursor.v2:": "account",
  "lakko.pairing.known-groups.v2.": "account",
  "lakko.channels.rotation-state.v2.": "account",
  "lakko.channels.publish-counter.v2.": "account",
  "lakko/publisher-group-unclaimed/v2/": "account",
  "lakko:group-calls:": "account",
  "lakko.emoji.recent:": "account",
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
  // ADR-0061: the interface language is chosen per device.
  "lakko.language": "device",
  "lakko.transparency.last-seen-root.v1": "device",
  "lakko.push.device.v1": "device",
  "lakko.composer.recordMode": "device",
  "lakko.media.autoplayVideoNotes": "device",
  // sessionStorage, tab-wide, holds no account id ("device" / "phrase")
  "lakko.restored-identity": "device",
  // not storage names
  "lakko/state-bundle-row/v1": "not-storage",
  "lakko/state-bundle/v1": "not-storage",
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
