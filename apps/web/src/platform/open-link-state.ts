// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Whether the Open a link dialog is up, and why (`docs/ux/desktop.md` 7). */
import { createSignal } from "solid-js";

/** `copied`: a web page copied the link, on the OSes where the app cannot be
 *  handed it (`docs/ux/desktop.md` section 7). */
export type OpenLinkReason = "copied";

export const [openLinkReason, setOpenLinkReason] = createSignal<OpenLinkReason | null>(null);
