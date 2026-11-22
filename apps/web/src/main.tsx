// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Solid.js entry point — mounts <App /> into the #root div in
 * `index.html`. This file is intentionally tiny: anything that needs
 * to evolve (routing, providers, error boundaries) belongs in
 * `App.tsx` or its descendants, not here.
 *
 * Phase 1.16a smoke goal: confirm Solid mounts, the WASM loader runs
 * once, and the Rust core round-trip renders. Phase 1.16b added
 * routing, signup/login, contacts and 1×1 chat. Phase 1.16f2 wires
 * the service worker registration *after* the app mounts so a
 * registration failure / network blip can't block the first paint.
 */
import { render } from "solid-js/web";
import "./styles.css";
import { registerServiceWorker } from "./sw/register";
import { runBootUpdateCheck } from "./update/release-check";
import { refreshSupportersList } from "./supporters/Supporters";
import { bootstrapTheme } from "./ui/theme";
import { bootstrapTextSize } from "./ui/text-size";
import { bootstrapVideoSurfaces } from "./calls/video-surface";
import { bootstrapL10n, LANGUAGE_STORAGE_KEY } from "./l10n";
import { publishNotificationStrings } from "./l10n/notification-strings";
import { installOriginHistory } from "./routes/origin-history";
import { browserActiveTabDeps, claimActiveTab, type ActiveTabClaim } from "./tab/active-tab";
import { freezeTab } from "./tab/tab-state";
import { runBeforeYield } from "./tab/before-yield";
import { OtherTabNotice } from "./tab/OtherTabNotice";
import { staleReaction } from "./tab/stale-reload";

/** Longest the first paint waits for a non-English catalog. Chosen, not measured. */
const FIRST_PAINT_WAIT_MS = 2000;

/**
 * A cheap guess, before the core has loaded, whether the resolved language
 * is not English. It only decides whether the first paint waits; the
 * language itself is still resolved by the core.
 */
function likelyNonEnglish(): boolean {
  let stored: string | null = null;
  try {
    stored = localStorage.getItem(LANGUAGE_STORAGE_KEY);
  } catch {
    stored = null;
  }
  if (stored !== null && stored !== "") return stored !== "en";
  const first = navigator.languages?.[0] ?? navigator.language ?? "en";
  return !first.toLowerCase().startsWith("en");
}

const root = document.getElementById("root");
if (root === null) {
  // index.html is shipped by us; if #root is missing the page is
  // broken at the source-tree level, not at runtime — fail loud.
  throw new Error("apps/web: #root element missing from index.html");
}

// 1.18f — apply the persisted `data-theme` BEFORE the Solid
// app renders so the very first frame doesn't flash the wrong
// theme. Synchronous reads of localStorage + a single
// setAttribute; ~0.5 ms typical.
bootstrapTheme();
// docs/ux/text-size.md — the message text size, before the first paint.
bootstrapTextSize();
// Call pictures kept off Firefox's compositor video surfaces
// (calls/video-surface.ts).
bootstrapVideoSurfaces(document.documentElement, navigator);

// The address bar shows the origin only: read the entry path (an invite
// link, a notification's `/?wake=`) and clear the URL before any screen
// renders. `routes/origin-history.ts`.
installOriginHistory();

// ADR-0061: resolve the interface language (stored choice, else the system
// list, else English) and load its catalog. Until the core has loaded every
// string renders from the English source. When the language is likely not
// English, the first paint waits for the catalog so the screen does not show
// English for a moment and then switch; the wait is capped so a slow or
// failed load never keeps the app from rendering. Never rejects.
const l10nReady = bootstrapL10n();
// ADR-0061: the service worker's notification words follow the language.
void l10nReady.then(publishNotificationStrings);
// docs/ux/one-tab.md — one Lakko tab per browser runs. The app is imported
// only once this tab runs, so a tab showing the notice has none of it loaded
// (no poll, no send, no save). A takeover by another tab freezes every writer
// here at once and reloads, which releases the lock (`tab/active-tab.ts`).
// On a takeover the open composer's draft and the synced state holding it are
// saved first, for at most a second (`tab/before-yield.ts`); the new tab
// waits on the lock until this page goes. A stolen lock or a stale write
// stops at once.
const stopAndReload = (): void => {
  freezeTab();
  location.reload();
};
const onLost = (reason: "takeover" | "stolen"): void => {
  if (reason === "stolen") {
    stopAndReload();
    return;
  }
  void runBeforeYield().finally(stopAndReload);
};
const mountApp = async (): Promise<void> => {
  const [{ default: App }, { mlsSnapshotWriter }] = await Promise.all([
    import("./App"),
    import("./chat/mls-store"),
  ]);
  // The store refused a write because the row on disk is newer than this
  // page's copy (something else wrote it): stop, and reload once. A refusal
  // again soon after that reload means a writer that keeps going (an older
  // build open elsewhere in this browser); the page then stays stopped and
  // shows the notice instead of reloading in a loop (`tab/stale-reload.ts`).
  let disposeApp: (() => void) | null = null;
  mlsSnapshotWriter.onStale(() => {
    freezeTab();
    let storage: Storage | null = null;
    try {
      storage = sessionStorage;
    } catch {
      storage = null;
    }
    if (staleReaction(storage, Date.now()) === "reload") {
      location.reload();
      return;
    }
    if (disposeApp === null) return;
    const stop = disposeApp;
    disposeApp = null;
    // eslint-disable-next-line no-console
    console.warn("one tab: the store keeps being written elsewhere; stopped, not reloading");
    stop();
    render(
      () => (
        <OtherTabNotice
          closeOther
          onUseHere={async () => {
            location.reload();
          }}
        />
      ),
      root,
    );
  });
  disposeApp = render(() => <App />, root);
};
const mount = (): void => {
  void (async () => {
    let claim: ActiveTabClaim;
    try {
      claim = await claimActiveTab(browserActiveTabDeps(onLost));
    } catch {
      claim = { kind: "active" };
    }
    if (claim.kind === "active") {
      await mountApp();
      return;
    }
    const takeOver = claim.takeOver;
    const dispose = render(
      () => (
        <OtherTabNotice
          onUseHere={async () => {
            await takeOver();
            dispose();
            await mountApp();
          }}
        />
      ),
      root,
    );
  })();
};
if (likelyNonEnglish()) {
  void Promise.race([l10nReady, new Promise((done) => setTimeout(done, FIRST_PAINT_WAIT_MS))]).then(
    mount,
  );
} else {
  mount();
}

// Fire-and-forget — the helper logs and swallows its own errors.
// Skipped in dev builds (Vite HMR + SW conflict) and in browsers
// without `navigator.serviceWorker`.
void registerServiceWorker();

// ADR-0038 S5 — the shared "update available" verify core. Fetches +
// verifies the signed release manifest and, if a newer release exists,
// emits a single console.info. Web renders NO banner (it defers to the
// SW-driven SwUpdateBanner, which is the web's real update path); this
// is the shared-core landing + a dormant future hook. Dormant unless
// VITE_LAKKO_RELEASE_KEY is pinned; swallows its own errors.
void runBootUpdateCheck();
// docs/ux/supporters.md 2.1: the list at most once a day outside About.
void refreshSupportersList(false);
