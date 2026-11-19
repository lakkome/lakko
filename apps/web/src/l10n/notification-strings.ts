// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words the service worker puts on a call notification, kept in the
 * interface language (ADR-0061, `docs/ux/localization.md` section 10: "the web
 * service worker renders notification text in the chosen language").
 *
 * The worker cannot reach the catalog: `public/sw.js` is copied verbatim and
 * is not part of the build graph. So the page renders the few words a
 * notification needs and stores them in Cache Storage whenever the language
 * changes; the worker reads them at push time and falls back to English for
 * anything missing (`public/sw-call-notification.js`, `notificationStrings`).
 * Nothing here leaves the device.
 */
import { createEffect, createRoot } from "solid-js";
import { isDesktop } from "../platform";
import { currentLanguage, t } from "./index";
import { reactionNotificationsOn } from "../chat/reaction-notify-pref";

/** Cache name; must equal `NOTIFICATION_STRINGS_CACHE` in the worker. */
export const NOTIFICATION_STRINGS_CACHE = "lakko-l10n";

/** Request key; must equal `NOTIFICATION_STRINGS_URL` in the worker. */
export const NOTIFICATION_STRINGS_URL = "/__lakko/l10n/notification-strings.json";

/**
 * How many attachment counts are rendered ahead of time; must equal
 * `NOTIFICATION_ATTACHMENT_COUNTS` in the worker. The attachment line is a
 * plural message and the worker cannot run plural rules, so each count is
 * stored already rendered; a larger count reads in English.
 */
export const NOTIFICATION_ATTACHMENT_COUNTS = 99;

/** How many counts of the section 11 words are rendered (one message carries
 *  at most ten photos and videos, and ten files); must equal
 *  `NOTIFICATION_MEDIA_COUNTS` in the worker. */
export const NOTIFICATION_MEDIA_COUNTS = 10;

/** The words the worker reads. The field names are the worker's. */
export interface NotificationWords {
  voiceCall: string;
  videoCall: string;
  accept: string;
  decline: string;
  incomingCall: string;
  newMessage: string;
  /** ADR-0061 section 6 — the body for a message from a newer Lakko. */
  unsupportedMessage: string;
  /** Entry `i` is the attachment-only preview line for `i + 1` attachments. */
  attachments: string[];
  /** `docs/ux/media-messages.md` section 11 — entry `i` is the line for
   *  `i + 1` photos, videos, photos and videos together, and files. */
  photos: string[];
  videos: string[];
  photosAndVideos: string[];
  files: string[];
  /** Reactions section 12 — "{name} reacted {emoji} to your message", with
   *  U+0001 where the name goes and U+0002 where the emoji goes (the worker
   *  cannot run the formatter). */
  reactedToYourMessage: string;
  /** A group message's body, "{name}: {text}" (the list preview's rule),
   *  U+0001 = sender name, U+0002 = text. */
  senderPrefixed: string;
  /** ADR-0087 section 14 — a channel invitation card's body,
   *  "Invitation to {channel}", U+0001 = the channel's name; never the link. */
  channelInvite: string;
  /** UX 15.2 — a publisher invitation card's body, U+0001 = the channel. */
  publisherInvite: string;
  /** The name of a channel whose invitation names none. */
  channelFallbackName: string;
  /** The `Reactions` switch is off on this device. */
  reactionsOff: boolean;
  /** ADR-0092 — content previews, U+0001 = the question, title or name. */
  pollPreview: string;
  checklistPreview: string;
  locationPreview: string;
  contactPreview: string;
  articlePreview: string;
}

/** The words, rendered now. */
export function renderNotificationStrings(): NotificationWords {
  const attachments: string[] = [];
  for (let count = 1; count <= NOTIFICATION_ATTACHMENT_COUNTS; count++) {
    attachments.push(t("notification.message.attachments", { count }));
  }
  const counted = (key: string, from: number): string[] => {
    const out: string[] = [];
    for (let count = 1; count <= NOTIFICATION_MEDIA_COUNTS; count++) {
      out.push(count < from ? "" : t(key, { count }));
    }
    return out;
  };
  return {
    photos: counted("chat.media.photos", 1),
    videos: counted("chat.media.videos", 1),
    // Photos and videos together are always at least two.
    photosAndVideos: counted("chat.media.photos_and_videos", 2),
    files: counted("chat.media.files", 1),
    voiceCall: t("call.kind.voice"),
    videoCall: t("call.kind.video"),
    accept: t("call.incoming.accept"),
    decline: t("call.incoming.decline"),
    incomingCall: t("call.notification.incoming"),
    newMessage: t("notification.message.new"),
    unsupportedMessage: t("chat.message.unsupported"),
    attachments,
    reactedToYourMessage: t("notification.reaction.body", { name: "\u0001", emoji: "\u0002" }),
    senderPrefixed: t("chats.preview.sender", { name: "\u0001", text: "\u0002" }),
    channelInvite: t("chats.preview.channel_invite", { channel: "\u0001" }),
    publisherInvite: t("chats.preview.publisher_invite", { channel: "\u0001" }),
    channelFallbackName: t("channels.fallback_name"),
    reactionsOff: !reactionNotificationsOn(),
    pollPreview: t("preview.poll", { question: "\u0001" }),
    checklistPreview: t("preview.checklist", { title: "\u0001" }),
    locationPreview: t("preview.location"),
    contactPreview: t("preview.contact", { name: "\u0001" }),
    articlePreview: t("preview.article", { title: "\u0001" }),
  };
}

async function store(words: NotificationWords): Promise<void> {
  if (typeof caches === "undefined") return;
  // The desktop app has no service worker to read them (ADR-0085), and its
  // webview refuses Cache Storage for the app's own URL scheme.
  if (isDesktop()) return;
  try {
    const cache = await caches.open(NOTIFICATION_STRINGS_CACHE);
    await cache.put(
      NOTIFICATION_STRINGS_URL,
      new Response(JSON.stringify(words), { headers: { "content-type": "application/json" } }),
    );
  } catch (err) {
    // Storage refused (private mode, quota): notifications stay in English.
    console.warn("[l10n] could not store the notification words", err);
  }
}

let started = false;

/** Keep the stored words in the current language for as long as the page runs. */
export function publishNotificationStrings(): void {
  if (started || typeof window === "undefined") return;
  started = true;
  createRoot(() => {
    createEffect(() => {
      // Re-run on a language switch: `t` reads the current dictionary.
      currentLanguage();
      reactionNotificationsOn();
      void store(renderNotificationStrings());
    });
  });
}
