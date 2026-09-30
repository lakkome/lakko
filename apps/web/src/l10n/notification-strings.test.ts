// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  NOTIFICATION_STRINGS_CACHE as WORKER_CACHE,
  NOTIFICATION_STRINGS_URL as WORKER_URL,
  notificationStrings,
  // @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
} from "../../public/sw-call-notification.js";
import {
  NOTIFICATION_STRINGS_CACHE,
  NOTIFICATION_STRINGS_URL,
  renderNotificationStrings,
} from "./notification-strings";

describe("notification words for the service worker", () => {
  it("are stored where the worker reads them", () => {
    expect(NOTIFICATION_STRINGS_CACHE).toBe(WORKER_CACHE);
    expect(NOTIFICATION_STRINGS_URL).toBe(WORKER_URL);
  });

  it("cover every word the worker asks for, in English before a language loads", () => {
    const words = renderNotificationStrings();
    expect(notificationStrings(words)).toEqual(words);
    expect(words).toEqual({
      voiceCall: "Voice call",
      videoCall: "Video call",
      accept: "Accept",
      decline: "Decline",
      incomingCall: "Incoming call",
    });
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
