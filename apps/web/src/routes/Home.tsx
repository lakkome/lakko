// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Authenticated home — placeholder that 1.16e replaces with the
 * conversation list / inbox view.
 */
import type { JSX } from "solid-js";
import { A } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import PasskeysSection from "../auth/PasskeysSection";

export default function Home(): JSX.Element {
  const auth = useAuth();
  const greeting = (): string => {
    const s = auth.state();
    return s.kind === "session" ? `Hi, @${s.account.username}` : "Hi";
  };
  return (
    <article>
      <h1>{greeting()}</h1>
      <p>
        This is the Phase 1.16b shell. The conversation list, search, and
        unread state arrive in 1.16e together with the 1×1 chat surface.
      </p>
      <ul>
        <li>
          <A href={paths.contacts}>Contacts</A> — add and find people. Real
          discovery (OPRF lookup) ships in 1.16d.
        </li>
        <li>
          <A href={paths.dev}>Dev probe</A> — verify the WASM core is
          loaded in this browser.
        </li>
      </ul>
      <hr style={{ margin: "1.5rem 0" }} />
      <PasskeysSection />
    </article>
  );
}
