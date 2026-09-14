// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/calls` — create a call link, or join one. There is no call history store
 * yet, so the list pane says so instead of pretending.
 */
import { createSignal, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import CreateCallLinkPanel from "../calls/CreateCallLinkPanel";
import { callLinkSlug } from "../calls/call-link-slug";
import { Icon } from "../ui/icons";
import { paths } from "./paths";

export default function Calls(): JSX.Element {
  const navigate = useNavigate();
  const [link, setLink] = createSignal("");
  const slug = (): string | null => callLinkSlug(link());

  return (
    <div class="lk-split" data-open="true">
      <aside class="lk-pane" aria-label="Calls">
        <div class="lk-pane-head">
          <h1>Calls</h1>
        </div>
        <div class="lk-empty">
          <Icon name="calls" size={40} strokeWidth={1.4} />
          <strong>No recent calls</strong>
          <span>Calls you start from a chat or a link will appear here.</span>
        </div>
      </aside>
      <div class="lk-main">
        <article>
          <div class="lk-page lk-stack" style={{ "max-width": "520px" }}>
            <CreateCallLinkPanel />
            <section class="lk-card">
              <h2>Join a call</h2>
              <form
                class="lk-actions"
                style={{ "flex-wrap": "nowrap" }}
                onSubmit={(e) => {
                  e.preventDefault();
                  const s = slug();
                  if (s !== null) navigate(paths.joinCall(s));
                }}
              >
                <input
                  type="text"
                  placeholder="Paste a lakko.me/c/ link"
                  aria-label="Call link"
                  value={link()}
                  onInput={(e) => setLink(e.currentTarget.value)}
                  style={{ flex: 1 }}
                />
                <button type="submit" disabled={slug() === null}>
                  Join
                </button>
              </form>
            </section>
          </div>
        </article>
      </div>
    </div>
  );
}
