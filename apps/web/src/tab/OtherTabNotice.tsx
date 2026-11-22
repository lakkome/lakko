// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The notice a tab shows while Lakko runs in another tab of the browser
 * (docs/ux/one-tab.md section 3). Rendered by `main.tsx` instead of the app,
 * which this tab has not loaded.
 */
import { createSignal, type JSX } from "solid-js";
import { t } from "../l10n";
import { PublicLayout } from "../ui/PublicLayout";

export function OtherTabNotice(props: {
  onUseHere: () => Promise<void>;
  /** An older page keeps writing and cannot be stopped from here: ask the
   *  person to close it (`stale-reload.ts`). */
  closeOther?: boolean;
}): JSX.Element {
  const [opening, setOpening] = createSignal(false);
  const useHere = async (): Promise<void> => {
    if (opening()) return;
    setOpening(true);
    try {
      await props.onUseHere();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("one tab: take over failed", err instanceof Error ? err.name : "unknown");
      setOpening(false);
    }
  };
  return (
    <PublicLayout>
      <h1>{t("tab.other.title")}</h1>
      <p style={{ margin: 0, color: "var(--surface-muted)" }}>{props.closeOther === true ? t("tab.other.close_body") : t("tab.other.body")}</p>
      <button type="button" class="primary" disabled={opening()} onClick={() => void useHere()}>
        {opening() ? t("tab.other.opening") : t("tab.other.use_here")}
      </button>
    </PublicLayout>
  );
}
