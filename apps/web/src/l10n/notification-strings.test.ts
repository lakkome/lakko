// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  NOTIFICATION_STRINGS_CACHE as WORKER_CACHE,
  NOTIFICATION_STRINGS_URL as WORKER_URL,
  NOTIFICATION_ATTACHMENT_COUNTS as WORKER_ATTACHMENT_COUNTS,
  attachmentsLine,
  notificationStrings,
  // @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
} from "../../public/sw-call-notification.js";
import {
  NOTIFICATION_ATTACHMENT_COUNTS,
  NOTIFICATION_STRINGS_CACHE,
  NOTIFICATION_STRINGS_URL,
  renderNotificationStrings,
} from "./notification-strings";

describe("notification words for the service worker", () => {
  it("are stored where the worker reads them", () => {
    expect(NOTIFICATION_STRINGS_CACHE).toBe(WORKER_CACHE);
    expect(NOTIFICATION_STRINGS_URL).toBe(WORKER_URL);
    expect(NOTIFICATION_ATTACHMENT_COUNTS).toBe(WORKER_ATTACHMENT_COUNTS);
  });

  it("cover every word the worker asks for, in English before a language loads", () => {
    const words = renderNotificationStrings();
    expect(notificationStrings(words)).toEqual(words);
    const { attachments, ...rest } = words;
    expect(rest).toEqual({
      voiceCall: "Voice call",
      videoCall: "Video call",
      accept: "Accept",
      decline: "Decline",
      incomingCall: "Incoming call",
      newMessage: "New message",
      unsupportedMessage: "This message needs a newer version of Lakko.",
      reactedToYourMessage: expect.stringContaining("reacted"),
      reactionsOff: false,
    });
    expect(words.reactedToYourMessage).toContain("\u0001");
    expect(words.reactedToYourMessage).toContain("\u0002");
    expect(attachments).toHaveLength(NOTIFICATION_ATTACHMENT_COUNTS);
    expect(attachmentsLine(words, 1)).toBe("Sent an attachment");
    expect(attachmentsLine(words, 3)).toBe("Sent 3 attachments");
  });

  it("read the attachment line in English past the rendered counts or for a bad entry", () => {
    const stored = notificationStrings({ attachments: ["Una", 7, "Tres"] });
    expect(attachmentsLine(stored, 1)).toBe("Una");
    expect(attachmentsLine(stored, 2)).toBe("Sent 2 attachments");
    expect(attachmentsLine(stored, 3)).toBe("Tres");
    expect(attachmentsLine(stored, 150)).toBe("Sent 150 attachments");
    expect(notificationStrings({ attachments: "nope" }).attachments).toEqual([]);
    expect(notificationStrings({ newMessage: "Nuevo mensaje" }).newMessage).toBe("Nuevo mensaje");
  });

  it("fall back to English word by word when storage holds something else", () => {
    expect(notificationStrings(null).accept).toBe("Accept");
    expect(notificationStrings({ accept: 7, decline: "Ablehnen" })).toMatchObject({
      accept: "Accept",
      decline: "Ablehnen",
    });
    expect(notificationStrings({ voiceCall: "x".repeat(201) }).voiceCall).toBe("Voice call");
  });
});
