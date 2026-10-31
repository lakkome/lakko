// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Open in the app" on the invite, call-link and group-invite pages
 * (`docs/ux/desktop.md` section 7). On macOS, in the browsers that stay quiet
 * with no app installed, the page also tries the app by itself, once per link.
 * The page keeps working as it was whether or not the app opens: a page cannot
 * tell which happened.
 */
import { useLocation } from "@solidjs/router";
import { createEffect, createMemo, createSignal, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { publicOrigin } from "./index";
import { mayHaveDesktopApp, openInAppAction, opensAppByItself, parseLakkoLink } from "./lakko-link";
import { isMacPlatform } from "./shortcuts";

/** Links this page already tried by itself: going back to one asks no more.
 *  Held in memory, since a link's secret stays out of browser storage. */
const triedByItself = new Set<string>();

/** Through a link click: in Chrome a script setting `location` was not seen to
 *  leave the page working with no app installed; a click was, in Chrome,
 *  Firefox and Safari. */
function launch(url: string): void {
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.hidden = true;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
}

export function OpenInApp(): JSX.Element {
  const location = useLocation();
  const [copied, setCopied] = createSignal(false);
  const link = createMemo(() =>
    parseLakkoLink(`${publicOrigin()}${location.pathname}${location.hash}`, publicOrigin()),
  );
  const offered = mayHaveDesktopApp(navigator.userAgent, navigator.maxTouchPoints);

  createEffect(() => {
    const current = link();
    if (current === null || !offered) return;
    const mac = isMacPlatform();
    if (!opensAppByItself(navigator.userAgent, mac)) return;
    const key = `${current.kind}/${current.id}`;
    if (triedByItself.has(key)) return;
    triedByItself.add(key);
    launch(openInAppAction(current, mac).url);
  });

  const open = async (): Promise<void> => {
    const current = link();
    if (current === null) return;
    const action = openInAppAction(current, isMacPlatform());
    if (action.copyFirst) {
      try {
        await navigator.clipboard.writeText(`${publicOrigin()}${location.pathname}${location.hash}`);
        setCopied(true);
      } catch {
        // Nothing copied: the app still opens, and the person can copy the
        // address bar.
      }
    }
    launch(action.url);
  };

  return (
    <Show when={offered && link() !== null}>
      <div class="lk-open-in-app">
        <button type="button" onClick={() => void open()}>
          {t("open_in_app.button")}
        </button>
        <Show when={copied()}>
          <p class="lk-row-sub" role="status">{t("open_in_app.copied")}</p>
        </Show>
      </div>
    </Show>
  );
}
