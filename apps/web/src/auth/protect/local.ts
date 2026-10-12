// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The plain storage the protection and notice stores live in
 * (`account-protection.ts`, `notices.ts`), or `undefined` where there is
 * none. One accessor, so the screens that use those stores name no storage
 * themselves.
 */
import type { MarkerStorage } from "../phrase-gate";

export function plainStore(): MarkerStorage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}
