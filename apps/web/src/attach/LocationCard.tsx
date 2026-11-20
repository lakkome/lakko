// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 9.3 — the location bubble. No map image:
 * a pin, the place name, the coordinates and the accuracy. A press asks the
 * conversation to open it (on web, the message menu with OpenStreetMap).
 */
import { Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { Icon } from "../ui/icons";
import { formatAccuracy, formatCoordinates } from "./location-format";
import type { SharedLocation } from "./types";
import "./bubbles.css";

export function LocationCard(props: {
  readonly location: SharedLocation;
  readonly onOpen: () => void;
}): JSX.Element {
  const coords = (): string =>
    formatCoordinates(props.location.latE7, props.location.lonE7, displayLocale());
  const accuracy = (): string => formatAccuracy(props.location.accuracyM, displayLocale());
  const name = (): string => (props.location.name ?? "").trim();
  return (
    <button
      type="button"
      class="lk-location-card"
      aria-label={[
        name(),
        t("location.a11y", { coordinates: coords(), distance: accuracy() }),
      ]
        .filter((s) => s.length > 0)
        .join(", ")}
      onClick={() => props.onOpen()}
    >
      <span class="lk-location-pin" aria-hidden="true">
        <Icon name="pin" size={22} />
      </span>
      <span class="lk-location-lines" aria-hidden="true">
        <Show when={name().length > 0}>
          <span class="lk-location-name">{name()}</span>
        </Show>
        <span class="lk-ak-muted">{coords()}</span>
        <span class="lk-ak-small lk-ak-muted">{t("location.accuracy", { distance: accuracy() })}</span>
      </span>
    </button>
  );
}
