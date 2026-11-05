// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { withSharedGroupFallback } from "./send-request";

const message = () => () => "failed";

describe("Add contact falls back to a shared group (who-can-find-me 6.4)", () => {
  it("a sent or failed request is left as it is", async () => {
    const sendInGroup = vi.fn();
    const deps = { sharedGroups: async () => [{ groupIdB64: "g", newestMs: 1 }], sendInGroup, errorMessage: message };
    expect((await withSharedGroupFallback({ kind: "sent" }, deps)).outcome).toEqual({ kind: "sent" });
    expect((await withSharedGroupFallback({ kind: "failed", message: () => "x" }, deps)).groupIdB64).toBeNull();
    expect(sendInGroup).not.toHaveBeenCalled();
  });

  it("a refused request goes inside the shared group with the newest message", async () => {
    const sendInGroup = vi.fn(async () => undefined);
    const out = await withSharedGroupFallback(
      { kind: "refused", byReachCheck: true },
      {
        sharedGroups: async () => [
          { groupIdB64: "old", newestMs: 1 },
          { groupIdB64: "new", newestMs: 5 },
        ],
        sendInGroup,
        errorMessage: message,
      },
    );
    expect(sendInGroup).toHaveBeenCalledWith("new");
    expect(out).toEqual({ outcome: { kind: "sent" }, groupIdB64: "new" });
  });

  it("with no shared group the refusal stands; a failed group send is a failure", async () => {
    const none = await withSharedGroupFallback(
      { kind: "refused" },
      { sharedGroups: async () => [], sendInGroup: vi.fn(), errorMessage: message },
    );
    expect(none.outcome).toEqual({ kind: "refused" });
    const failed = await withSharedGroupFallback(
      { kind: "refused" },
      {
        sharedGroups: async () => [{ groupIdB64: "g", newestMs: 1 }],
        sendInGroup: vi.fn().mockRejectedValue(new Error("offline")),
        errorMessage: message,
      },
    );
    expect(failed.outcome.kind).toBe("failed");
    expect(failed.groupIdB64).toBeNull();
  });
});
