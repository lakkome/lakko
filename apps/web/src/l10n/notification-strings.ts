// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words the service worker puts on a call notification, kept in the
 * interface language (ADR-0058, `docs/ux/localization.md` section 10: "the web
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
import { currentLanguage, t } from "./index";

/** Cache name; must equal `NOTIFICATION_STRINGS_CACHE` in the worker. */
export const NOTIFICATION_STRINGS_CACHE = "lakko-l10n";

/** Request key; must equal `NOTIFICATION_STRINGS_URL` in the worker. */
export const NOTIFICATION_STRINGS_URL = "/__lakko/l10n/notification-strings.json";

/** The words, rendered now. The field names are the worker's. */
export function renderNotificationStrings(): Record<string, string> {
  return {
    voiceCall: t("call.kind.voice"),
    videoCall: t("call.kind.video"),
    accept: t("call.incoming.accept"),
    decline: t("call.incoming.decline"),
    incomingCall: t("call.notification.incoming"),
  };
}

async function store(words: Record<string, string>): Promise<void> {
  if (typeof caches === "undefined") return;
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
      void store(renderNotificationStrings());
    });
  });
}
