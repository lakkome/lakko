// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Open in the app" on the invite, call-link and group-invite pages
 * (`docs/ux/desktop.md` section 7). On macOS, in the browsers that stay quiet
 * with no app installed, the page also tries the app by itself, once per link.
 * The page keeps working as it was whether or not the app opens: a page cannot
 * tell which happened.
 *
 * On a phone or tablet (section 7.1) the same button opens the Android app
 * through an `intent:` address and the iOS app through its `lakko:` scheme,
 * on the same pages and on a shared feed; it never tries by itself there.
 */
import { useLocation } from "@solidjs/router";
import { createEffect, createMemo, onCleanup, Show, type JSX } from "solid-js";

import { useCallHost } from "../calls/CallHostContext";
import { t } from "../l10n";
import { publicOrigin } from "./index";
import {
  androidIntentUrl,
  androidPackageFromEnv,
  iosAppUrl,
  mayHaveDesktopApp,
  openInAppUrl,
  opensAppByItself,
  parseLakkoLink,
  phoneAppLinkOf,
  phoneAppPlatform,
} from "./lakko-link";
import { isMacPlatform } from "./shortcuts";

/** Links this page already tried by itself: going back to one asks no more.
 *  Held in memory, since a link's secret stays out of browser storage. */
const triedByItself = new Set<string>();

/** The Android app this build opens (`VITE_LAKKO_ANDROID_PACKAGE`). */
const ANDROID_PACKAGE = androidPackageFromEnv(
  import.meta.env.VITE_LAKKO_ANDROID_PACKAGE as string | undefined,
);

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
  return (
    <Show
      when={mayHaveDesktopApp(navigator.userAgent, navigator.maxTouchPoints)}
      fallback={<OpenInPhoneApp />}
    >
      <OpenInDesktopApp />
    </Show>
  );
}

function OpenInDesktopApp(): JSX.Element {
  const location = useLocation();
  const link = createMemo(() =>
    parseLakkoLink(`${publicOrigin()}${location.pathname}${location.hash}`, publicOrigin()),
  );
  const offered = mayHaveDesktopApp(navigator.userAgent, navigator.maxTouchPoints);

  createEffect(() => {
    const current = link();
    if (current === null || !offered) return;
    if (!opensAppByItself(navigator.userAgent, isMacPlatform())) return;
    const key = `${current.kind}/${current.id}`;
    if (triedByItself.has(key)) return;
    triedByItself.add(key);
    launch(openInAppUrl(current));
  });

  const open = (): void => {
    const current = link();
    if (current !== null) launch(openInAppUrl(current));
  };

  return (
    <Show when={offered && link() !== null}>
      <div class="lk-open-in-app">
        <button type="button" class="primary" onClick={open}>
          {t("open_in_app.button")}
        </button>
      </div>
    </Show>
  );
}

/** Section 7.1: the button on a phone or tablet. */
function OpenInPhoneApp(): JSX.Element {
  const location = useLocation();
  const callHost = useCallHost();
  const platform = phoneAppPlatform(navigator.userAgent, navigator.maxTouchPoints);
  const target = createMemo((): string | null => {
    if (platform === null) return null;
    const origin = publicOrigin();
    const link = phoneAppLinkOf(location.pathname, location.hash, origin);
    if (link === null) return null;
    if (platform === "ios") return iosAppUrl(link);
    return ANDROID_PACKAGE === null ? null : androidIntentUrl(link, origin, ANDROID_PACKAGE);
  });
  // A call from the link covers the page; the button would sit on top of it.
  const shown = (): boolean => target() !== null && !callHost.anyCallInProgress();

  // The pages leave room at the bottom for the button, so it never covers
  // their own (styles.css `html[data-open-in-app]`).
  createEffect(() => {
    const root = document.documentElement;
    if (shown()) root.dataset["openInApp"] = "";
    else delete root.dataset["openInApp"];
  });
  onCleanup(() => {
    delete document.documentElement.dataset["openInApp"];
  });

  const open = (): void => {
    const url = target();
    if (url !== null) launch(url);
  };

  return (
    <Show when={shown()}>
      <div class="lk-open-in-app">
        <button type="button" class="primary" onClick={open}>
          {t("open_in_app.button")}
        </button>
      </div>
    </Show>
  );
}
