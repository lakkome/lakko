// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { inviteContactToCall, type InviteRouteDeps } from "./invite-route";
import { inviteRowLine, rowIsDone } from "./InviteContactsPicker";

function deps(over: Partial<InviteRouteDeps<string>>): InviteRouteDeps<string> & {
  sendLink: ReturnType<typeof vi.fn>;
  sendInGroup: ReturnType<typeof vi.fn>;
  found: ReturnType<typeof vi.fn>;
} {
  return {
    existing: () => undefined,
    accountKnown: true,
    grantReaches: async () => false,
    requestWaiting: () => false,
    sharedGroups: async () => [],
    found: vi.fn(async () => "new-chat"),
    sendLink: vi.fn(async () => undefined),
    sendInGroup: vi.fn(async () => undefined),
    ...over,
  } as never;
}

describe("Invite contacts takes the same way as Message (who-can-find-me 6.4)", () => {
  it("a one-to-one here: the call link goes into it", async () => {
    const d = deps({ existing: () => "chat" });
    expect(await inviteContactToCall(d)).toBe("invited");
    expect(d.sendLink).toHaveBeenCalledWith("chat");
    expect(d.found).not.toHaveBeenCalled();
  });

  it("a grant that reaches: the one-to-one is founded and the link goes into it", async () => {
    const d = deps({ grantReaches: async () => true });
    expect(await inviteContactToCall(d)).toBe("invited");
    expect(d.sendLink).toHaveBeenCalledWith("new-chat");
  });

  it("no grant, a shared group: the contact request goes inside it and no call link is sent", async () => {
    const d = deps({ sharedGroups: async () => [{ groupIdB64: "walk", newestMs: 1 }] });
    expect(await inviteContactToCall(d)).toBe("request-sent");
    expect(d.sendInGroup).toHaveBeenCalledWith("walk");
    expect(d.sendLink).not.toHaveBeenCalled();
    expect(d.found).not.toHaveBeenCalled();
  });

  it("a request already standing in Sent: nothing is sent again", async () => {
    const d = deps({ requestWaiting: () => true, sharedGroups: async () => [{ groupIdB64: "walk", newestMs: 1 }] });
    expect(await inviteContactToCall(d)).toBe("request-sent");
    expect(d.sendInGroup).not.toHaveBeenCalled();
  });

  it("nothing reaches them: nothing is sent", async () => {
    const d = deps({});
    expect(await inviteContactToCall(d)).toBe("cannot-message");
    expect(d.sendInGroup).not.toHaveBeenCalled();
    expect(d.sendLink).not.toHaveBeenCalled();
  });

  it("a failed group send rejects, so the picker shows the error and the row stays pressable", async () => {
    const d = deps({
      sharedGroups: async () => [{ groupIdB64: "walk", newestMs: 1 }],
      sendInGroup: vi.fn().mockRejectedValue(new Error("offline")),
    } as never);
    await expect(inviteContactToCall(d)).rejects.toThrow("offline");
  });

  it("the row: Request sent and Invited cannot be pressed again, cannot-message can", () => {
    expect(rowIsDone("invited")).toBe(true);
    expect(rowIsDone("request-sent")).toBe(true);
    expect(rowIsDone("cannot-message")).toBe(false);
    expect(rowIsDone(undefined)).toBe(false);
    expect(inviteRowLine(undefined, "Ann")).toBeNull();
    expect(inviteRowLine("request-sent", "Ann")).toBe("Request sent");
    expect(inviteRowLine("cannot-message", "Ann")).toContain("Ann");
  });
});
