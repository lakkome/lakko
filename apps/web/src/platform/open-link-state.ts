// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Whether the Open a link dialog is up, and why (`docs/ux/desktop.md` 7). */
import { createSignal } from "solid-js";

/** `menu`: the person chose it. `copied`: a web page copied the link. */
export type OpenLinkReason = "menu" | "copied";

export const [openLinkReason, setOpenLinkReason] = createSignal<OpenLinkReason | null>(null);
