// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Design system "Avatar": every avatar of a conversation that has a photo
 * shows it. Base walk 2026-11-04: Feed and Calls drew a group with a photo as
 * its initials, because they looked up a person's photo only.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { conversationAvatarKeys } from "./conversation-avatar";

describe("conversationAvatarKeys", () => {
  it("a group looks up its own photo, whatever its size", () => {
    expect(conversationAvatarKeys({ kind: "group", groupIdB64: "Zw==", peerKeyB64: "cA==" })).toEqual({
      conversationKey: "Zw==",
    });
  });

  it("a one-to-one looks up the other person's photo", () => {
    expect(conversationAvatarKeys({ kind: "chat", groupIdB64: "Zw==", peerKeyB64: "cA==" })).toEqual({
      personKey: "cA==",
    });
    expect(conversationAvatarKeys({ kind: "chat", groupIdB64: "Zw==" })).toEqual({});
  });
});

describe("the surfaces that draw a conversation use it", () => {
  const source = (file: string): string => readFileSync(resolve(process.cwd(), "src", file), "utf8");

  it.each(["routes/Calls.tsx", "chat/ForwardPicker.tsx", "routes/Feed.tsx"])("%s", (file) => {
    expect(source(file)).toContain("conversationAvatarKeys(");
  });

  it("the Feed's source avatar passes the group's photo key", () => {
    expect(source("routes/Feed.tsx")).toContain("conversationKey={sourceAvatarKeyOf(item.ref.source)}");
  });

  it("Calls no longer guesses a one-to-one from the member count", () => {
    expect(source("routes/Calls.tsx")).not.toContain("memberKeysB64.length > 2");
  });
});
