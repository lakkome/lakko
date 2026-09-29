// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Poll cadences of the camera-less another-device route (ADR-0049 section 2
 * as amended 2026-09-28). The exchange itself lives in `device-link.ts`; the
 * comparison code that used to live here is gone (see that module).
 */

/** How often the signed-in device asks for pending requests while a
 *  session is open. */
export const DEVICE_REQUEST_POLL_MS = 8_000;

/** How often the fresh device checks where its request stands. */
export const DEVICE_REQUEST_STATUS_POLL_MS = 3_000;
