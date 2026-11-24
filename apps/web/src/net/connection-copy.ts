// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The words for the connection state (`docs/ux/connection-state.md` sections
 * 1, 4 and 5), kept apart from `connection-state.ts` so the request layers
 * that feed the state do not load the localization runtime.
 */

import { t } from "../l10n";
import type { ConnectionPhase } from "./connection-state";

/** The header text for a phase, or `null` when the normal text stands. */
export function connectionHeaderText(phase: ConnectionPhase): string | null {
  switch (phase) {
    case "online":
      return null;
    case "waiting":
      return t("connection.waiting");
    case "connecting":
      return t("connection.connecting");
  }
}

/** What the screen reader says on entering a phase (section 5). */
export function connectionAnnouncementText(phase: ConnectionPhase): string {
  switch (phase) {
    case "online":
      return t("connection.restored_a11y");
    case "waiting":
      return t("connection.waiting");
    case "connecting":
      return t("connection.connecting");
  }
}
