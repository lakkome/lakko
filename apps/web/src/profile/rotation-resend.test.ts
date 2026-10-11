// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0066 section 3.5a rule 3 - the profile re-send after a key rotation is
 * owed until every conversation has it.
 */
import { afterEach, describe, expect, it } from "vitest";

import { ProfilePublishScheduler } from "./publish-scheduler";
import {
  installProfileResend,
  owedProfileConversations,
  profileResendVerdict,
  resendProfileAfterRotation,
} from "./rotation-resend";

const OLD = "key:aaaaaaaaaaaaaaaa|member:0|contact:0";
const NEW = "key:bbbbbbbbbbbbbbbb|member:0|contact:0";

describe("what is owed", () => {
  it("after a re-key every active conversation is owed, with unchanged versions", () => {
    expect(
      owedProfileConversations([
        { groupIdB64: "g1", active: true, publishedSignature: OLD, currentSignature: NEW },
        { groupIdB64: "g2", active: true, publishedSignature: NEW, currentSignature: NEW },
        { groupIdB64: "g3", active: true, publishedSignature: null, currentSignature: NEW },
        { groupIdB64: "g4", active: false, publishedSignature: OLD, currentSignature: NEW },
      ]),
    ).toEqual(["g1", "g3"]);
  });

  it("the verdict is done only when nothing is owed", () => {
    expect(profileResendVerdict(0)).toBe("done");
    expect(profileResendVerdict(2)).toBe("later");
    expect(profileResendVerdict(null)).toBe("later");
  });
});

describe("the rotation step", () => {
  afterEach(() => installProfileResend(null));

  it("is owed with no publisher, on a failure, and while a conversation is left", async () => {
    expect(await resendProfileAfterRotation()).toBe("later");
    installProfileResend(() => Promise.reject(new Error("offline")));
    expect(await resendProfileAfterRotation()).toBe("later");
    installProfileResend(() => Promise.resolve(1));
    expect(await resendProfileAfterRotation()).toBe("later");
    installProfileResend(() => Promise.resolve(null));
    expect(await resendProfileAfterRotation()).toBe("later");
    installProfileResend(() => Promise.resolve(0));
    expect(await resendProfileAfterRotation()).toBe("done");
  });

  it("a publish that failed leaves its conversation owed, through the real publisher", async () => {
    // The publisher records a conversation as published only when the
    // publish landed (as `ProfileContext.publishInto` does).
    const published = new Map<string, string>();
    let failing = new Set(["g2"]);
    const scheduler = new ProfilePublishScheduler<number>({
      now: () => 1,
      setTimer: () => 0,
      clearTimer: () => undefined,
      random: () => 0,
      publish: async (g) => {
        if (failing.has(g)) throw new Error("offline");
        published.set(g, NEW);
      },
    });
    const owed = (): string[] =>
      owedProfileConversations(
        ["g1", "g2"].map((g) => ({
          groupIdB64: g,
          active: true,
          publishedSignature: published.get(g) ?? OLD,
          currentSignature: NEW,
        })),
      );
    installProfileResend(async () => {
      const due = owed();
      if (due.length > 0) await scheduler.publishAll(due);
      return owed().length;
    });
    expect(await scheduler.publishAll([])).toEqual([]);
    expect(await resendProfileAfterRotation()).toBe("later");
    expect(owed()).toEqual(["g2"]);
    // The retry sends only what is still owed, and then the step is done.
    failing = new Set();
    expect(await resendProfileAfterRotation()).toBe("done");
    expect(owed()).toEqual([]);
  });
});
