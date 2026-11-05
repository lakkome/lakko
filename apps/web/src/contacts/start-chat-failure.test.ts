// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { DsHttpError, DsNetworkError } from "../ds/client";
import { JournalClaimFailedError } from "../ds/ownership";
import { MailboxUnreachableError } from "../reach/mailbox-write";
import { ContactsClient, ContactsError } from "./client";
import { startChatFailureReason } from "./start-chat-failure";

const JOURNAL = "/ds/group/e6fab45e00112233445566778899aabbccddeeff00112233445566778899aabb/claim";

describe("startChatFailureReason (seen live 2026-11-05: the failure was swallowed)", () => {
  it("names a Welcome that has nothing to be signed with", () => {
    const reason = startChatFailureReason(ContactsError.wrap(new MailboxUnreachableError(0)));
    expect(reason).toBe("no reach grant is held for this person, so the Welcome cannot be written");
    expect(startChatFailureReason(new MailboxUnreachableError(2))).toContain("refused (2)");
  });

  it("names a refused or unreachable journal claim and never prints the journal id", () => {
    const refused = ContactsError.wrap(new JournalClaimFailedError(new DsHttpError(409, JOURNAL, "")));
    expect(startChatFailureReason(refused)).toBe("the new chat's journal claim answered 409");
    const offline = ContactsError.wrap(new DsNetworkError(JOURNAL, new TypeError("Failed to fetch")));
    const reason = startChatFailureReason(offline);
    expect(reason).toBe("the delivery service could not be reached");
    expect(reason).not.toContain("e6fab45e");
  });

  it("an unknown failure is logged by its type, never its message", () => {
    const reason = startChatFailureReason(ContactsError.wrap(new RangeError("group e6fab45e...")));
    expect(reason).toBe("failed with RangeError");
  });
});

describe("startGroupWith founds nothing when the Welcome could not be written", () => {
  it("throws before founding or claiming a journal", async () => {
    const client = Object.create(ContactsClient.prototype) as ContactsClient;
    const prepare = vi.fn();
    Object.defineProperty(client, "welcomeCanReach", { value: async () => false });
    Object.defineProperty(client, "prepareGroupWith", { value: prepare });
    const err = await client
      .startGroupWith(null, new Uint8Array(16), new Uint8Array(32), "chat")
      .catch((e: unknown) => e);
    expect(startChatFailureReason(err)).toContain("no reach grant");
    expect(prepare).not.toHaveBeenCalled();
  });

  it("an answer to a request carries its own grant and is not held back", async () => {
    const client = Object.create(ContactsClient.prototype) as ContactsClient;
    const reach = vi.fn(async () => false);
    Object.defineProperty(client, "welcomeCanReach", { value: reach });
    Object.defineProperty(client, "prepareGroupWith", {
      value: async () => {
        throw new Error("prepared");
      },
    });
    await expect(
      client.startGroupWith(null, new Uint8Array(16), new Uint8Array(32), "chat", { replyGrantB64: "grant" }),
    ).rejects.toThrow("prepared");
    expect(reach).not.toHaveBeenCalled();
  });
});
