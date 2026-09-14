// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Catch-all 404. Routed to from the `*` pattern in `routes.tsx`.
 */
import type { JSX } from "solid-js";
import { A } from "@solidjs/router";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";

export default function NotFound(): JSX.Element {
  return (
    <PublicLayout>
      <h1>Page not found</h1>
      <p style={{ margin: 0, color: "var(--surface-muted)" }}>
        There is nothing at this address.
      </p>
      <p style={{ margin: 0 }}>
        <A href={paths.home}>Open Lakko</A>
      </p>
    </PublicLayout>
  );
}
