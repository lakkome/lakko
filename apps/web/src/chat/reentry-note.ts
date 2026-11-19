// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 / UX `conversation-rejoin.md` 2.2 - the second line of the
 * waiting note: who can bring this device back. Pure apart from the
 * catalog lookup.
 */
import { t } from "../l10n";
import type { ReentryRouteName } from "./reentry-store";

/** No second line in the first 30 seconds (UX 2.2). */
export const REENTRY_SECOND_LINE_MS = 30_000;

/**
 * The second line for the route the request took, or `null` while no
 * request was recorded (the device does not know yet who can help).
 * `peerName` is read only for a one-to-one.
 */
export function reentrySecondLineText(route: ReentryRouteName | undefined, peerName: () => string): string | null {
  switch (route) {
    case "own-device":
      return t("chat.reentry.second.own_device");
    case "mailbox":
      return t("chat.reentry.second.person", { name: peerName() });
    case "knock":
      return t("chat.reentry.second.group");
    default:
      return null;
  }
}
