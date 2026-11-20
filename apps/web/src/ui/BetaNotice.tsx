// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The beta notice (`docs/ux/beta-notice.md`): Lakko is a beta and its
 * security has not been independently audited.
 *
 * {@link BetaNoticeBanner} is the design-system Banner that only informs:
 * the `info` icon, a title and a body, no action, no close button, nothing
 * to focus. It is drawn above the sign-in form, the sign-up form (and its
 * "Lakko is full for now" state) and the guest's `Create account` form.
 * Screen readers read it as one element, title then body; the icon is
 * decorative. It has no state and never depends on the network.
 *
 * {@link BetaNoticeRow} is the same words as the first row of Settings >
 * About, in the style of the other rows there.
 */
import type { JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "./icons";

/** The informing banner above an account-creating or sign-in form. */
export function BetaNoticeBanner(): JSX.Element {
  return (
    <div class="lk-info-banner" role="note">
      <span class="lk-info-banner-icon">
        <Icon name="info-circle" size={20} />
      </span>
      <p class="lk-info-banner-text">
        <strong class="lk-info-banner-title">{t("beta.notice.title")}</strong>
        {/* The full stop and space keep the title and body apart when the
            element is read as one ("Beta. Lakko is in beta, ..."); the
            title is a block, so the space is not seen. */}
        <span class="lk-visually-hidden">. </span>
        <span class="lk-info-banner-body">{t("beta.notice.body")}</span>
      </p>
    </div>
  );
}

/** The beta notice as a Settings > About row. */
export function BetaNoticeRow(): JSX.Element {
  return (
    <div class="lk-card-row">
      <span class="lk-card-row-icon">
        <Icon name="info-circle" size={18} />
      </span>
      <div class="lk-card-row-body">
        <span class="lk-card-row-title">{t("beta.notice.title")}</span>
        <span class="lk-card-row-sub">{t("beta.notice.body")}</span>
      </div>
    </div>
  );
}
