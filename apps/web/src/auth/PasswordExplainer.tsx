// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "How Lakko protects your password": the one small link on the login and
 * signup cards that opens a plain-language explanation inline
 * (docs/ux/client-redesign.md section 5.6). Nothing about the protocol; only
 * what it means for the user.
 */
import type { JSX } from "solid-js";
import { Icon } from "../ui/icons";
import { t } from "../l10n";

export function PasswordExplainer(): JSX.Element {
  return (
    <details class="lk-disclosure link">
      <summary>
        <Icon name="lock" size={14} />
        {t("auth.explainer.title")}
      </summary>
      <div class="lk-disclosure-body">
        <p>{t("auth.explainer.never_leaves")}</p>
        <p>{t("auth.explainer.same_answer")}</p>
        <p>{t("auth.explainer.ways_back")}</p>
      </div>
    </details>
  );
}
