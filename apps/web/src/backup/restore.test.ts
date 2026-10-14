// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one account-backup restore every phrase route runs (2026-10-02).
 *
 * Driven through its injected effects, so the rule is what is asserted:
 *
 *   - a restore applies the contacts, the synced state and the list of
 *     conversations, and has no way to touch MLS state;
 *   - a missing backup (404) is not an error;
 *   - a fetch failure does not fail the sign-in (the function never throws),
 *     leaves the owed flag set, and the next session landing retries once;
 *   - a backup the head gate refuses, or one the phrase does not open, is
 *     not applied and is not retried;
 *   - the phrase-prompt landing runs it with the phrase in hand; any other
 *     landing only when a restore is owed, from the device's phrase cache.
 *
 * `AuthContext.tsx` needs a Solid runtime this suite does not have, so that
 * both phrase routes reach this function is pinned on its source.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { MemoryStorage } from "../test/fake-storage";
import type { AcceptedContact } from "../contacts/local-store";
import type { RestoreGateContext, RestoreResult } from "./client";
import {
  restoreAccountBackup,
  restoreOwedAccountBackup,
  type AccountBackupRestoreDeps,
  type AccountBackupRestoreInputs,
  type AccountBackupRestoreOutcome,
} from "./restore";
import {
  clearBackupRestoreOwed,
  isBackupRestoreOwed,
  markBackupRestoreOwed,
} from "./restore-owed";
import {
  addRestoredKnownGroups,
  backupKnownGroupIds,
  loadRestoredKnownGroups,
} from "./restored-known-groups";

const ACCOUNT_BYTES = new Uint8Array(16).fill(0x42);
const ACCOUNT = "QkJCQkJCQkJCQkJCQkJCQg==";
const DEVICE_KEY = { fake: "device-key" } as unknown as CryptoKey;
const PHRASE = "abandon ability able about above absent absorb abstract";
const G1 = "ZzEtZ3JvdXAtaWQ=";
const G2 = "ZzItZ3JvdXAtaWQ=";
const OWN = "b3duLWdyb3VwLWlk";

const CONTACT: AcceptedContact = {
  signaturePubkeyB64: "cGVlci1rZXk=",
  displayName: "Peer",
  acceptedAtMs: 5,
};
const SYNCED = new Uint8Array([9, 8, 7]);

function restored(overrides: Partial<Extract<RestoreResult, { kind: "restored" }>> = {}): RestoreResult {
  return {
    kind: "restored",
    accountId: ACCOUNT_BYTES,
    createdAtMs: 1,
    ed25519Seed: new Uint8Array(32),
    acceptedContacts: [CONTACT],
    knownGroupIds: [G1, G2],
    syncedState: SYNCED,
    ...overrides,
  };
}

const INPUTS: AccountBackupRestoreInputs = {
  accountIdBytes: ACCOUNT_BYTES,
  accountIdB64: ACCOUNT,
  deviceKey: DEVICE_KEY,
  sessionToken: "v1.session",
  phrase: PHRASE,
};

interface Harness {
  readonly storage: MemoryStorage;
  readonly deps: AccountBackupRestoreDeps;
  readonly calls: string[];
  readonly fetched: { phrase: string; token: string; gate: RestoreGateContext }[];
  readonly contacts: AcceptedContact[];
  readonly synced: Uint8Array[];
  alerts: number;
}

/** The real owed flag and the real restored-groups store over one in-memory
 *  `Storage`; everything that would need a network, wasm or IndexedDB is a
 *  recorder. */
function harness(answer: () => Promise<RestoreResult>): Harness {
  const storage = new MemoryStorage();
  const h: Harness = {
    storage,
    calls: [],
    fetched: [],
    contacts: [],
    synced: [],
    alerts: 0,
    deps: {
      fetch: async (phraseBytes, sessionToken, gate) => {
        h.calls.push("fetch");
        h.fetched.push({ phrase: new TextDecoder().decode(phraseBytes), token: sessionToken, gate });
        return answer();
      },
      applyContacts: (contacts, accountIdB64) => {
        h.calls.push(`applyContacts ${accountIdB64}`);
        h.contacts.push(...contacts);
      },
      applySyncedState: async (accountIdB64, deviceKey, synced) => {
        h.calls.push(`applySyncedState ${accountIdB64} ${deviceKey === DEVICE_KEY}`);
        h.synced.push(synced);
      },
      rememberKnownGroups: (accountIdB64, ids) => {
        h.calls.push(`rememberKnownGroups ${accountIdB64}`);
        addRestoredKnownGroups(accountIdB64, ids, storage);
      },
      markOwed: (accountIdB64) => markBackupRestoreOwed(accountIdB64, storage),
      clearOwed: (accountIdB64) => {
        h.calls.push("clearOwed");
        clearBackupRestoreOwed(accountIdB64, storage);
      },
      alertRollbackRefused: () => {
        h.alerts += 1;
      },
    },
  };
  return h;
}

/** Silence the expected `console.warn` / `console.error` of a refusal path
 *  and return what was logged. */
async function quietly<T>(run: () => Promise<T>): Promise<{ value: T; logged: string[] }> {
  const warn = console.warn;
  const error = console.error;
  const logged: string[] = [];
  const record = (...args: unknown[]): void => {
    logged.push(args.map(String).join(" "));
  };
  console.warn = record;
  console.error = record;
  try {
    return { value: await run(), logged };
  } finally {
    console.warn = warn;
    console.error = error;
  }
}

describe("restoreAccountBackup", () => {
  it("applies the contacts, the synced state and the known conversations", async () => {
    const h = harness(async () => restored());
    const outcome = await restoreAccountBackup(INPUTS, h.deps);
    expect(outcome).toEqual({ kind: "applied", contacts: 1, knownGroups: 2 });
    expect(h.contacts).toEqual([CONTACT]);
    expect(h.synced).toEqual([SYNCED]);
    expect(loadRestoredKnownGroups(ACCOUNT, h.storage)).toEqual([G1, G2]);
    // The known conversations are recorded BEFORE the owed flag is cleared:
    // once it is, the automatic backup may upload, and must already list them.
    expect(h.calls).toEqual([
      "fetch",
      `applyContacts ${ACCOUNT}`,
      `applySyncedState ${ACCOUNT} true`,
      `rememberKnownGroups ${ACCOUNT}`,
      "clearOwed",
    ]);
    expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(false);
  });

  it("fetches with the phrase and arms the head gate for this account and device", async () => {
    const h = harness(async () => restored());
    await restoreAccountBackup(INPUTS, h.deps);
    expect(h.fetched).toHaveLength(1);
    expect(h.fetched[0]?.phrase).toBe(PHRASE);
    expect(h.fetched[0]?.token).toBe("v1.session");
    expect(h.fetched[0]?.gate).toEqual({
      accountId: ACCOUNT_BYTES,
      accountIdB64: ACCOUNT,
      deviceKey: DEVICE_KEY,
    });
  });

  it("has no effect that could carry MLS state", async () => {
    // The whole effect surface: nothing here takes or returns a provider
    // snapshot, and a restored result has no such field to hand over.
    const h = harness(async () => restored());
    expect(Object.keys(h.deps).sort()).toEqual([
      "alertRollbackRefused",
      "applyContacts",
      "applySyncedState",
      "clearOwed",
      "fetch",
      "markOwed",
      "rememberKnownGroups",
    ]);
    expect(Object.keys(restored()).sort()).toEqual([
      "acceptedContacts",
      "accountId",
      "createdAtMs",
      "ed25519Seed",
      "kind",
      "knownGroupIds",
      "syncedState",
    ]);
  });

  it("the next upload after a restore lists the restored groups plus its own", async () => {
    const h = harness(async () => restored());
    await restoreAccountBackup(INPUTS, h.deps);
    expect(backupKnownGroupIds(ACCOUNT, [OWN], h.storage).sort()).toEqual([G1, G2, OWN].sort());
    // Joining G1 takes it out of the restored set; it stays listed as held.
    expect(backupKnownGroupIds(ACCOUNT, [OWN, G1], h.storage).sort()).toEqual([G1, G2, OWN].sort());
    expect(loadRestoredKnownGroups(ACCOUNT, h.storage)).toEqual([G2]);
  });

  it("a missing backup is not an error and is not retried", async () => {
    const h = harness(async () => ({ kind: "no-backup" }));
    const { value, logged } = await quietly(() => restoreAccountBackup(INPUTS, h.deps));
    expect(value).toEqual({ kind: "no-backup" });
    expect(logged).toEqual([]);
    expect(h.contacts).toEqual([]);
    expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(false);
  });

  it("a fetch failure resolves, applies nothing and leaves the restore owed", async () => {
    const h = harness(async () => ({ kind: "transport-failed", cause: new Error("offline") }));
    const { value, logged } = await quietly(() => restoreAccountBackup(INPUTS, h.deps));
    expect(value).toEqual({ kind: "deferred" });
    expect(h.calls).toEqual(["fetch"]);
    expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(true);
    expect(logged).toHaveLength(1);
    for (const line of logged) expect(/^[\x20-\x7e]*$/.test(line), line).toBe(true);
  });

  it("a fetch that throws is the same as one that failed", async () => {
    const h = harness(async () => {
      throw new Error("boom");
    });
    const { value } = await quietly(() => restoreAccountBackup(INPUTS, h.deps));
    expect(value).toEqual({ kind: "deferred" });
    expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(true);
  });

  it("a tampered backup is refused with the alert, not applied, not retried", async () => {
    const h = harness(async () => ({ kind: "rollback-blocked", hard: true, reason: "bad head" }));
    const { value } = await quietly(() => restoreAccountBackup(INPUTS, h.deps));
    expect(value).toEqual({ kind: "not-applied", reason: "rollback-refused" });
    expect(h.alerts).toBe(1);
    expect(h.contacts).toEqual([]);
    expect(loadRestoredKnownGroups(ACCOUNT, h.storage)).toEqual([]);
    expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(false);
  });

  it("a stale backup the person did not accept is not applied, with no alert", async () => {
    const h = harness(async () => ({ kind: "rollback-blocked", hard: false, reason: "stale" }));
    const { value } = await quietly(() => restoreAccountBackup(INPUTS, h.deps));
    expect(value).toEqual({ kind: "not-applied", reason: "stale-declined" });
    expect(h.alerts).toBe(0);
    expect(h.contacts).toEqual([]);
    expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(false);
  });

  it.each([["wrong-phrase-or-tampered" as const], ["unsupported-payload" as const]])(
    "%s is not applied and not retried",
    async (kind) => {
      const h = harness(async () => ({ kind }));
      const { value } = await quietly(() => restoreAccountBackup(INPUTS, h.deps));
      expect(value).toEqual({ kind: "not-applied", reason: kind });
      expect(h.contacts).toEqual([]);
      expect(isBackupRestoreOwed(ACCOUNT, h.storage)).toBe(false);
    },
  );

  it("a failed synced-state merge still applies the rest", async () => {
    const h = harness(async () => restored());
    const deps: AccountBackupRestoreDeps = {
      ...h.deps,
      applySyncedState: async () => {
        throw new Error("idb blocked");
      },
    };
    const { value } = await quietly(() => restoreAccountBackup(INPUTS, deps));
    expect(value.kind).toBe("applied");
    expect(h.contacts).toEqual([CONTACT]);
    expect(loadRestoredKnownGroups(ACCOUNT, h.storage)).toEqual([G1, G2]);
  });

  it("two callers at once share one fetch", async () => {
    let release: (r: RestoreResult) => void = () => {};
    const h = harness(
      () =>
        new Promise<RestoreResult>((resolve) => {
          release = resolve;
        }),
    );
    const first = restoreAccountBackup(INPUTS, h.deps);
    const second = restoreAccountBackup(INPUTS, h.deps);
    // Let the first reach its fetch.
    await Promise.resolve();
    release(restored());
    expect(await first).toEqual(await second);
    expect(h.fetched).toHaveLength(1);
    expect(h.contacts).toEqual([CONTACT]);
  });
});

describe("restoreOwedAccountBackup (the session-landing step)", () => {
  interface Landing {
    readonly storage: MemoryStorage;
    readonly restores: AccountBackupRestoreInputs[];
    cached: string | null;
    next: AccountBackupRestoreOutcome;
    run(phrase?: string): Promise<AccountBackupRestoreOutcome | null>;
  }

  function landing(): Landing {
    const storage = new MemoryStorage();
    const l: Landing = {
      storage,
      restores: [],
      cached: PHRASE,
      next: { kind: "applied", contacts: 1, knownGroups: 2 },
      run: (phrase) =>
        restoreOwedAccountBackup(
          {
            accountIdBytes: ACCOUNT_BYTES,
            accountIdB64: ACCOUNT,
            sessionToken: "v1.session",
            deviceKey: async () => DEVICE_KEY,
            ...(phrase !== undefined ? { phrase } : {}),
          },
          {
            isOwed: (a) => isBackupRestoreOwed(a, storage),
            clearOwed: (a) => clearBackupRestoreOwed(a, storage),
            cachedPhrase: async () => l.cached,
            restore: async (inputs) => {
              l.restores.push(inputs);
              // What the real restore does to the flag.
              if (l.next.kind !== "deferred") clearBackupRestoreOwed(inputs.accountIdB64, storage);
              return l.next;
            },
          },
        ),
    };
    return l;
  }

  it("does nothing on an ordinary landing", async () => {
    const l = landing();
    expect(await l.run()).toBeNull();
    expect(l.restores).toEqual([]);
  });

  it("the phrase prompt's landing fetches and applies with the phrase in hand", async () => {
    const l = landing();
    // No phrase cache is needed on this landing.
    l.cached = null;
    markBackupRestoreOwed(ACCOUNT, l.storage);
    const outcome = await l.run(PHRASE);
    expect(outcome).toEqual({ kind: "applied", contacts: 1, knownGroups: 2 });
    expect(l.restores).toHaveLength(1);
    expect(l.restores[0]?.phrase).toBe(PHRASE);
    expect(l.restores[0]?.deviceKey).toBe(DEVICE_KEY);
    expect(l.restores[0]?.accountIdB64).toBe(ACCOUNT);
  });

  it("the phrase prompt's landing runs even when the flag could not be stored", async () => {
    // Blocked site data: `markBackupRestoreOwed` was a no-op. The route
    // still has the phrase in hand and still owes the restore.
    const l = landing();
    expect(isBackupRestoreOwed(ACCOUNT, l.storage)).toBe(false);
    expect(await l.run(PHRASE)).toEqual({ kind: "applied", contacts: 1, knownGroups: 2 });
    expect(l.restores).toHaveLength(1);
  });

  it("a failed fetch is retried once on the next landing, and not again after it succeeds", async () => {
    const l = landing();
    markBackupRestoreOwed(ACCOUNT, l.storage);
    // The sign-in whose fetch failed: it resolves, the flag stays.
    l.next = { kind: "deferred" };
    expect(await l.run(PHRASE)).toEqual({ kind: "deferred" });
    expect(isBackupRestoreOwed(ACCOUNT, l.storage)).toBe(true);
    // The next landing (a reload, no phrase in hand): one retry, from the
    // phrase cache.
    l.next = { kind: "applied", contacts: 1, knownGroups: 2 };
    expect(await l.run()).toEqual({ kind: "applied", contacts: 1, knownGroups: 2 });
    expect(l.restores).toHaveLength(2);
    expect(l.restores[1]?.phrase).toBe(PHRASE);
    // The landing after that: nothing is owed, nothing is fetched.
    expect(await l.run()).toBeNull();
    expect(l.restores).toHaveLength(2);
  });

  it("one landing makes one attempt, even when it fails again", async () => {
    const l = landing();
    markBackupRestoreOwed(ACCOUNT, l.storage);
    l.next = { kind: "deferred" };
    await l.run();
    expect(l.restores).toHaveLength(1);
    expect(isBackupRestoreOwed(ACCOUNT, l.storage)).toBe(true);
  });

  it("with no phrase on the device it gives the flag up instead of logging forever", async () => {
    const l = landing();
    l.cached = null;
    markBackupRestoreOwed(ACCOUNT, l.storage);
    const { value } = await quietly(() => l.run());
    expect(value).toBeNull();
    expect(l.restores).toEqual([]);
    expect(isBackupRestoreOwed(ACCOUNT, l.storage)).toBe(false);
  });

  it("with no device key yet it waits for a later landing", async () => {
    const storage = new MemoryStorage();
    markBackupRestoreOwed(ACCOUNT, storage);
    let ran = 0;
    const { value } = await quietly(() =>
      restoreOwedAccountBackup(
        {
          accountIdBytes: ACCOUNT_BYTES,
          accountIdB64: ACCOUNT,
          sessionToken: "v1.session",
          deviceKey: async () => null,
        },
        {
          isOwed: (a) => isBackupRestoreOwed(a, storage),
          clearOwed: (a) => clearBackupRestoreOwed(a, storage),
          cachedPhrase: async () => PHRASE,
          restore: async () => {
            ran += 1;
            return { kind: "no-backup" };
          },
        },
      ),
    );
    expect(value).toBeNull();
    expect(ran).toBe(0);
    expect(isBackupRestoreOwed(ACCOUNT, storage)).toBe(true);
  });

  it("never throws into the sign-in", async () => {
    const storage = new MemoryStorage();
    markBackupRestoreOwed(ACCOUNT, storage);
    const { value } = await quietly(() =>
      restoreOwedAccountBackup(
        {
          accountIdBytes: ACCOUNT_BYTES,
          accountIdB64: ACCOUNT,
          sessionToken: "v1.session",
          deviceKey: async () => {
            throw new Error("idb blocked");
          },
        },
        { isOwed: (a) => isBackupRestoreOwed(a, storage) },
      ),
    );
    expect(value).toBeNull();
  });
});

describe("the owed flag", () => {
  let storage: MemoryStorage;
  beforeEach(() => {
    storage = new MemoryStorage();
  });

  it("is per account and survives until cleared", () => {
    const other = "b3RoZXItYWNjb3VudA==";
    markBackupRestoreOwed(ACCOUNT, storage);
    expect(isBackupRestoreOwed(ACCOUNT, storage)).toBe(true);
    expect(isBackupRestoreOwed(other, storage)).toBe(false);
    clearBackupRestoreOwed(ACCOUNT, storage);
    expect(isBackupRestoreOwed(ACCOUNT, storage)).toBe(false);
  });

  it("holds no secret", () => {
    markBackupRestoreOwed(ACCOUNT, storage);
    expect(storage.entries()).toEqual([[`lakko.backup.restore-owed.v1.${ACCOUNT}`, "1"]]);
  });

  it("answers not owed with no storage at all", () => {
    expect(isBackupRestoreOwed(ACCOUNT, null)).toBe(false);
    expect(() => markBackupRestoreOwed(ACCOUNT, null)).not.toThrow();
  });
});

describe("the phrase routes in AuthContext", () => {
  const source = readFileSync(
    fileURLToPath(new URL("../auth/AuthContext.tsx", import.meta.url)),
    "utf8",
  );
  /** The body of one `AuthApi` method, up to the next method at its depth. */
  function method(name: string, next: string): string {
    const start = source.indexOf(`    async ${name}(`);
    const end = source.indexOf(`    async ${next}(`, start);
    expect(start, name).toBeGreaterThan(-1);
    expect(end, next).toBeGreaterThan(start);
    return source.slice(start, end);
  }
  /** Code only: comment lines out. */
  const code = (text: string): string =>
    text
      .split("\n")
      .filter((l) => !/^\s*(\*|\/\/|\/\*)/.test(l))
      .join("\n");

  it("the restore screen runs the shared restore, marked owed before the session lands", () => {
    const body = code(method("recover", "recoverWithTotp"));
    const owed = body.indexOf("markBackupRestoreOwed(accountIdB64)");
    const landed = body.indexOf("setIdentitySecrets(");
    const restore = body.indexOf("await restoreAccountBackup(");
    const baseline = body.indexOf("bootstrapBackupFromPhraseString(");
    expect(owed).toBeGreaterThan(-1);
    expect(owed).toBeLessThan(landed);
    expect(restore).toBeGreaterThan(landed);
    // The baseline upload comes after the restore, lists the union, and is
    // skipped while the restore is still owed.
    expect(baseline).toBeGreaterThan(restore);
    expect(body).toMatch(/restoreOutcome\.kind === "deferred"/);
    expect(body.slice(baseline)).toMatch(/knownGroupIds: backupKnownGroupIds\(/);
  });

  it("the phrase prompt marks the restore owed and lands with the phrase", () => {
    const body = code(method("provideRecoveryPhrase", "completeFromPairing"));
    const owed = body.indexOf("markBackupRestoreOwed(pending.accountIdB64)");
    const finish = body.indexOf("await finishCanonicalSession(");
    expect(owed).toBeGreaterThan(-1);
    expect(finish).toBeGreaterThan(owed);
    expect(body.slice(finish)).toMatch(/restorePhrase: canonicalPhrase/);
    // The phrase is stored for the retry before the session lands.
    const cache = body.indexOf("await storeDevicePhrase(");
    expect(cache).toBeGreaterThan(-1);
    expect(cache).toBeLessThan(finish);
  });

  it("a passkey that carries the keys and a mailed code go through the two phrase routes", () => {
    // A passkey's keys are opened to the phrase and handed to the phrase
    // prompt's route, on the sign-in and from the restore prompt alike.
    const start = source.indexOf("async function keysFromPasskey(");
    expect(start).toBeGreaterThan(-1);
    const passkey = code(source.slice(start, source.indexOf("\n  }\n", start)));
    expect(passkey).toMatch(/await api\.provideRecoveryPhrase\(phrase\)/);
    // The email vault's phrase goes to the restore screen's route.
    const email = readFileSync(
      fileURLToPath(new URL("../auth/recover/EmailRecover.tsx", import.meta.url)),
      "utf8",
    );
    expect(email).toMatch(/await auth\.recover\(phrase, password\(\), canonicalUsername\)/);
    // Three words are checked on a device that already holds the phrase and
    // the keys (ADR-0072): nothing is restored, so no backup is fetched.
    const words = code(method("recoverWithThreeWords", "verifyPassword"));
    expect(words).not.toMatch(/markBackupRestoreOwed|restoreAccountBackup|restorePhrase/);
  });

  it("every route that ends with new keys under a new phrase uploads the backup again", () => {
    // Sign-up without the phrase and a kept guest: a baseline at once. A key
    // reset, a recall (the same path) and a key rotation: the automatic
    // backup, marked dirty where the new phrase is stored.
    expect(code(method("signup", "signupGuest"))).toMatch(/bootstrapBackupFromPhraseString\(/);
    const slice = (from: string): string => {
      const start = source.indexOf(from);
      expect(start, from).toBeGreaterThan(-1);
      return code(source.slice(start, source.indexOf("\n  /**\n", start + 1)));
    };
    expect(slice("async function keepGuestAccount(")).toMatch(/void uploadBackupBaseline\(\{/);
    expect(slice("async function adoptKeyResetOutcome(")).toMatch(
      /"fresh-identity"\);\s*markBackupDirty\(ctx\.accountIdB64\);/,
    );
    expect(slice("async function adoptRotatedKeys(")).toMatch(
      /"key-rotation"\);\s*markBackupDirty\(ctx\.accountIdB64\);/,
    );
    // A guest account that was not kept leaves nothing on a server: its
    // phrase sits under the guest's own key, which no backup reads.
    expect(code(method("signupGuest", "discardGuestAccount"))).not.toMatch(
      /bootstrapBackupFromPhraseString|uploadBackupBaseline/,
    );
  });

  it("every session landing runs the owed-restore step", () => {
    const start = source.indexOf("async function finishCanonicalSession(");
    const end = source.indexOf("async function landAfterAccessRecovery(", start);
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const body = code(source.slice(start, end));
    // Once on the silent (reload) branch, once on the explicit one.
    expect(body.match(/restoreOwedBackupOnLanding\(args\)/g)).toHaveLength(2);
    expect(body).toMatch(/restoreOwedAccountBackup\(\{/);
  });

  it("the another-device routes do not fetch the backup", () => {
    for (const [name, next] of [
      ["completeFromPairing", "completeFromDeviceRestore"],
      ["completeFromDeviceRestore", "mintFreshIdentity"],
    ] as const) {
      const body = code(method(name, next));
      expect(body, name).not.toMatch(/markBackupRestoreOwed|restoreAccountBackup|restorePhrase/);
    }
  });
});
