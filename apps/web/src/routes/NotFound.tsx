// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Catch-all 404. Routed to from the `*` pattern in `routes.tsx`.
 */
import type { JSX } from "solid-js";
import { A } from "@solidjs/router";
import { paths } from "./paths";

export default function NotFound(): JSX.Element {
  return (
    <article>
      <h1>Not found</h1>
      <p>
        The page you tried to open does not exist. Try the{" "}
        <A href={paths.home}>home page</A>.
      </p>
    </article>
  );
}
