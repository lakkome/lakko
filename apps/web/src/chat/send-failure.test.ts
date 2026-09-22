// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { SEND_FAILED_NOTICE, sendFailureNotice, withSendAck } from "./send-failure";
import type { TimelineRow } from "./timeline-row";

describe("sendFailureNotice", () => {
  it("says one sentence when the core named no reason", () => {
    // A dropped connection, a 500, an envelope that would not compose. The
    // person cannot act on the difference, and the action is on the bubble.
    expect(sendFailureNotice(null)).toBe(SEND_FAILED_NOTICE);
  });

  it("keeps a membership refusal's own words", () => {
    // "You are no longer a member of this group" is the difference between
    // retrying usefully and retrying forever, so it must not be generalised.
    expect(
      sendFailureNotice({ message: "You are no longer a member of this group." }),
    ).toBe("You are no longer a member of this group.");
  });

  it("falls back rather than showing an empty bar", () => {
    expect(sendFailureNotice({ message: "   " })).toBe(SEND_FAILED_NOTICE);
  });

  it("never shows a JavaScript type name", () => {
    // The regression guard. What this replaced was
    // `ds append failed: network error: TypeError: Failed to fetch`, shown
    // to someone who was writing to a friend.
    expect(SEND_FAILED_NOTICE).not.toMatch(/Error|TypeError|undefined|null/);
  });

  it("is one sentence, with the action left to the bubble", () => {
    // docs/ux/client-redesign.md section 8. The bubble says "Not sent -
    // Retry" next to the message it belongs to; a second Retry in a bar at
    // the top would not say WHICH message.
    expect(SEND_FAILED_NOTICE.split(".").filter((p) => p.trim() !== "")).toHaveLength(1);
    expect(SEND_FAILED_NOTICE).not.toMatch(/Retry/);
  });
});

describe("withSendAck", () => {
  const outgoing = (tempId: number, ack: "pending" | "sent" | "failed"): TimelineRow => ({
    kind: "outgoing",
    tempId,
    plaintext: "the words",
    attachments: [],
    ack,
  });

  it("KEEPS the row when a send fails, with its text", () => {
    // The defect. A failed send used to remove the row, and because the
    // composer had already been cleared the words were gone with it - a
    // message typed on a bad connection simply vanished.
    const after = withSendAck([outgoing(1, "pending")], 1, "failed");
    expect(after).toHaveLength(1);
    const row = after[0];
    expect(row?.kind === "outgoing" && row.ack).toBe("failed");
    expect(row?.kind === "outgoing" && row.plaintext).toBe("the words");
  });

  it("keeps the attachments on the row rather than pushing them back", () => {
    // They used to go back to the composer while the text went nowhere, so
    // one half of the message was recoverable and the other was not.
    const withFile: TimelineRow = {
      kind: "outgoing",
      tempId: 1,
      plaintext: "the words",
      ack: "pending",
      attachments: [
        {
          blobId: new Uint8Array(32),
          key: new Uint8Array(32),
          nonce: new Uint8Array(12),
          mime: "image/png",
          size: 1,
          name: "a.png",
        },
      ],
    };
    const row = withSendAck([withFile], 1, "failed")[0];
    expect(row?.kind === "outgoing" && row.attachments).toHaveLength(1);
  });

  it("puts a retried row back to pending", () => {
    const row = withSendAck([outgoing(1, "failed")], 1, "pending")[0];
    expect(row?.kind === "outgoing" && row.ack).toBe("pending");
  });

  it("touches no other row", () => {
    const other = outgoing(2, "sent");
    const after = withSendAck([outgoing(1, "pending"), other], 1, "failed");
    expect(after[1]).toBe(other);
  });

  it("returns the input itself when the row is gone", () => {
    // The chat was reset while the send was in flight; settling an ack for a
    // row nobody holds must not re-render the timeline.
    const rows = [outgoing(1, "pending")];
    expect(withSendAck(rows, 99, "failed")).toBe(rows);
  });

  it("returns the input itself when the ack already says that", () => {
    const rows = [outgoing(1, "failed")];
    expect(withSendAck(rows, 1, "failed")[0]).toBe(rows[0]);
  });
});
