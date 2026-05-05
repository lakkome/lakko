// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Chat route — `/chat/:id`. Placeholder; 1.16e plumbs the MLS
 * group state, message timeline, draft input, send / decrypt flow,
 * and the DS subscription.
 */
import type { JSX } from "solid-js";
import { useParams } from "@solidjs/router";

export default function Chat(): JSX.Element {
  const params = useParams<{ id: string }>();
  return (
    <article>
      <h1>Chat</h1>
      <p>
        Phase 1.16b: routing only. Conversation id (decoded):{" "}
        <code>{decodeURIComponent(params.id)}</code>
      </p>
      <p>
        The actual 1×1 chat surface — MLS group plumbed through
        <code> lakko-ffi-wasm</code>, message timeline, draft input,
        send / decrypt, DS subscription — lands in 1.16e.
      </p>
    </article>
  );
}
