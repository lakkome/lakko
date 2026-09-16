// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Shared handle types for the owner's manage panel.
 *
 * The wasm `PublisherGroup` handle `openPublisherGroup` returns is shared by
 * the rotation cron AND the co-publisher panel so a `rotate()` on one is seen
 * by the other: two handles would commit divergent epochs (see
 * `ManagePanel.tsx` for the full argument).
 */

import type { openPublisherGroup } from "./copublisher";
import type { Wasm } from "../wasm";

/** The wasm `PublisherGroup` handle `openPublisherGroup` returns. */
export type PublisherGroupHandle = Awaited<ReturnType<typeof openPublisherGroup>>;

/** wasm-bindgen rotation policy — `{ intervalMs: bigint, maxPosts: number }`. */
export type ChannelRotationPolicy = ReturnType<Wasm["channelRotationPolicyDefault"]>;
