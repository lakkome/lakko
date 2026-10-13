// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - the publish guard: nothing is sealed or sent into a
 * superseded one-to-one, by any `ChatClient`, except the successor claim.
 *
 * The refusal happens BEFORE the identity is resolved and before anything
 * touches the MLS group, so a bare object stands in for the group here: a
 * refused call must not read it at all.
 */
import { afterEach, describe, expect, it } from "vitest";

import { ChatClient, HeldForSuccessor, installDeadSessionGuard } from "./client";

type Group = Parameters<ChatClient["sendMessage"]>[1];

/** A group whose every property read throws: touching it fails the test. */
function untouchable(): Group {
  return new Proxy(
    {},
    {
      get() {
        throw new Error("the group was read");
      },
    },
  ) as unknown as Group;
}

afterEach(() => installDeadSessionGuard(null));

describe("the dead-session publish guard", () => {
  it("refuses a message before anything is sealed", async () => {
    installDeadSessionGuard(() => true);
    await expect(
      new ChatClient().sendMessage(null, untouchable(), new Uint8Array([1])),
    ).rejects.toBeInstanceOf(HeldForSuccessor);
  });

  it("refuses a control frame the same way", async () => {
    installDeadSessionGuard(() => true);
    await expect(
      new ChatClient().broadcastControl(null, untouchable(), new Uint8Array([1])),
    ).rejects.toBeInstanceOf(HeldForSuccessor);
  });

  it("lets the successor claim through (it then proceeds to the ordinary path)", async () => {
    installDeadSessionGuard(() => true);
    // No identity is available here, so the ordinary path fails further on:
    // what matters is that the guard was not what stopped it.
    await expect(
      new ChatClient().broadcastControl(null, untouchable(), new Uint8Array([1]), {
        intoSupersededSession: true,
      }),
    ).rejects.not.toBeInstanceOf(HeldForSuccessor);
  });

  it("is consulted by every client in the tab, and a guard that throws blocks nothing", async () => {
    installDeadSessionGuard(() => {
      throw new Error("guard failed");
    });
    await expect(
      new ChatClient().sendMessage(null, untouchable(), new Uint8Array([1])),
    ).rejects.not.toBeInstanceOf(HeldForSuccessor);
    installDeadSessionGuard(() => false);
    await expect(
      new ChatClient().sendMessage(null, untouchable(), new Uint8Array([1])),
    ).rejects.not.toBeInstanceOf(HeldForSuccessor);
  });
});
