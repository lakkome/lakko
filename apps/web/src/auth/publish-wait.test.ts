// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 15.4 "Waiting for the directory": a reset inside the hour after the
 * account's keys last changed is refused by the directory with `429
 * identity-changed-too-recently` and how long is left; the screen says so,
 * offers no Try again, and retries by itself when the time is up.
 */
import { describe, expect, it } from "vitest";

import { DiscoveryHttpError, retryAfterSecsOf } from "../discovery/client";
import {
  directoryCooldownSecs,
  publishOwedView,
  readKeyResetOwed,
  withDirectoryWait,
  withKeyResetStepDone,
  writeKeyResetOwed,
  type KeyResetOwed,
} from "./key-reset-owed";
import type { MarkerStorage } from "./phrase-gate";
import { directoryCooldownOf } from "./publish-wait-clock";
import { t } from "../l10n";

const COOLDOWN_BODY = '{"error":"identity-changed-too-recently","retry_after_secs":2400}';

describe("retryAfterSecsOf", () => {
  it("reads retry_after_secs from the body first", () => {
    expect(retryAfterSecsOf(COOLDOWN_BODY, "60")).toBe(2400);
  });
  it("falls back to a Retry-After header in seconds", () => {
    expect(retryAfterSecsOf('{"error":"identity-changed-too-recently"}', " 90 ")).toBe(90);
    expect(retryAfterSecsOf("not json", "5")).toBe(5);
  });
  it("is null when neither is a positive number of seconds", () => {
    expect(retryAfterSecsOf('{"error":"identity-changed-too-recently"}', null)).toBeNull();
    expect(retryAfterSecsOf('{"retry_after_secs":0}', null)).toBeNull();
    expect(retryAfterSecsOf('{"retry_after_secs":"40"}', null)).toBeNull();
    expect(retryAfterSecsOf("", "Wed, 21 Oct 2015 07:28:00 GMT")).toBeNull();
  });
  it("rounds a fraction up", () => {
    expect(retryAfterSecsOf('{"retry_after_secs":1.2}', null)).toBe(2);
  });
  it("is what DiscoveryHttpError carries", () => {
    const err = new DiscoveryHttpError(429, "/discovery/identity", COOLDOWN_BODY, "2400");
    expect(err.errorCode).toBe("identity-changed-too-recently");
    expect(err.retryAfterSecs).toBe(2400);
    expect(new DiscoveryHttpError(429, "/x", "", "30").retryAfterSecs).toBe(30);
  });
});

describe("directoryCooldownSecs", () => {
  it("is the seconds of the cool-down refusal", () => {
    expect(
      directoryCooldownSecs({ status: 429, errorCode: "identity-changed-too-recently", retryAfterSecs: 2400 }),
    ).toBe(2400);
    expect(directoryCooldownOf(new DiscoveryHttpError(429, "/discovery/identity", COOLDOWN_BODY))).toBe(2400);
  });
  it("is null for a cool-down with no readable time (the could-not-finish copy stays)", () => {
    expect(
      directoryCooldownSecs({ status: 429, errorCode: "identity-changed-too-recently", retryAfterSecs: null }),
    ).toBeNull();
  });
  it("is null for any other refusal", () => {
    expect(directoryCooldownSecs({ status: 429, errorCode: "too-many-attempts", retryAfterSecs: 30 })).toBeNull();
    expect(directoryCooldownSecs({ status: 429, errorCode: null, retryAfterSecs: 30 })).toBeNull();
    expect(
      directoryCooldownSecs({ status: 409, errorCode: "identity-changed-too-recently", retryAfterSecs: 30 }),
    ).toBeNull();
    expect(directoryCooldownOf(new Error("offline"))).toBeNull();
  });
});

describe("publishOwedView", () => {
  const base = { nowMs: 1_000_000, retrying: false, wordsShown: true, signInNeeded: false };

  it("with no wait: the could-not-finish copy and Try again", () => {
    expect(publishOwedView({ ...base, waitUntilMs: null })).toEqual({
      textKey: "recovery_phrase.fresh.publish_owed",
      minutes: null,
      tryAgain: true,
      waiting: false,
    });
  });

  it("while waiting: the waiting copy, minutes rounded up, no Try again", () => {
    expect(publishOwedView({ ...base, waitUntilMs: base.nowMs + 2400 * 1000 })).toEqual({
      textKey: "recovery_phrase.fresh.publish_waiting",
      minutes: 40,
      tryAgain: false,
      waiting: true,
    });
    expect(publishOwedView({ ...base, waitUntilMs: base.nowMs + 61_000 }).minutes).toBe(2);
    expect(publishOwedView({ ...base, waitUntilMs: base.nowMs + 60_000 }).minutes).toBe(1);
  });

  it("never says less than one minute", () => {
    expect(publishOwedView({ ...base, waitUntilMs: base.nowMs + 1_000 }).minutes).toBe(1);
  });

  it("keeps the waiting copy while the automatic retry at the end is out", () => {
    const view = publishOwedView({ ...base, waitUntilMs: base.nowMs - 500, retrying: true });
    expect(view.textKey).toBe("recovery_phrase.fresh.publish_waiting");
    expect(view.minutes).toBe(1);
    expect(view.tryAgain).toBe(false);
  });

  it("a wait that ended with no retry out offers Try again again", () => {
    expect(publishOwedView({ ...base, waitUntilMs: base.nowMs - 1 }).tryAgain).toBe(true);
  });

  it("an account never shown words: its wordless copies, the same minutes, no Try again while waiting", () => {
    const wordless = { ...base, wordsShown: false };
    expect(publishOwedView({ ...wordless, waitUntilMs: null })).toEqual({
      textKey: "account.keys_changed.finishing",
      minutes: null,
      tryAgain: true,
      waiting: false,
    });
    expect(publishOwedView({ ...wordless, waitUntilMs: base.nowMs + 2400 * 1000 })).toEqual({
      textKey: "account.keys_changed.waiting",
      minutes: 40,
      tryAgain: false,
      waiting: true,
    });
  });

  it("the wordless copies speak of no words (alignment P)", () => {
    expect(t("account.keys_changed.finishing")).toBe("Your account has new keys. We are finishing the change.");
    const waiting = t("account.keys_changed.waiting", { minutes: 40 });
    expect(waiting).toContain("40 minutes");
    for (const text of [t("account.keys_changed.finishing"), waiting]) {
      expect(/phrase|word/i.test(text)).toBe(false);
    }
    expect(t("recovery_phrase.fresh.publish_waiting", { minutes: 1 })).toContain("1 minute");
  });

  it("sign-in-needed outranks a wait", () => {
    expect(publishOwedView({ ...base, signInNeeded: true, waitUntilMs: base.nowMs + 60_000 })).toEqual({
      textKey: "recovery_phrase.fresh.sign_in_to_finish",
      minutes: null,
      tryAgain: false,
      waiting: false,
    });
  });
});

class MemoryStorage implements MarkerStorage {
  private readonly map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
}

describe("the recorded wait", () => {
  const record: KeyResetOwed = {
    v: 1,
    attestationB64Url: "AA",
    keyGeneration: "4",
    newSignaturePubkeyB64: "BB",
    signedAtMs: 1,
    done: { directory: false, deviceGroup: false },
  };

  it("survives a reload and is cleared by a landed directory step", () => {
    const storage = new MemoryStorage();
    writeKeyResetOwed(storage, "acc", withDirectoryWait(record, 5_000));
    const read = readKeyResetOwed(storage, "acc");
    expect(read?.waitUntilMs).toBe(5_000);
    expect(read === null ? null : withKeyResetStepDone(read, "directory").waitUntilMs).toBeUndefined();
  });

  it("is cleared by withDirectoryWait(null) and absent from an old record", () => {
    expect(withDirectoryWait(withDirectoryWait(record, 5_000), null)).toEqual(record);
    const storage = new MemoryStorage();
    writeKeyResetOwed(storage, "acc", record);
    expect(readKeyResetOwed(storage, "acc")?.waitUntilMs).toBeUndefined();
  });
});
