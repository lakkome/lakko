// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Contacts route — placeholder. 1.16d wires the OPRF discovery
 * client (via `lakko-ffi-wasm`) and the add-contact / pending /
 * accepted UI flows.
 */
import type { JSX } from "solid-js";

export default function Contacts(): JSX.Element {
  return (
    <article>
      <h1>Contacts</h1>
      <p>
        Phase 1.16b: routing only. The real contacts UI — username
        search via OPRF, ContactRequest send / accept / decline, pending
        inbox — lands in 1.16d.
      </p>
    </article>
  );
}
