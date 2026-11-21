// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import type { Component } from "solid-js";

/** Indeterminate spinner glyph. Self-contained SVG (SMIL rotation, no global
 *  keyframes); takes the surrounding text colour. */
export const Spinner: Component<{ size?: number }> = (props) => {
  const size = (): number => props.size ?? 22;
  return (
    <svg
      class="lk-spinner"
      width={size()}
      height={size()}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        cx="12"
        cy="12"
        r="9"
        stroke="currentColor"
        stroke-width="3"
        stroke-linecap="round"
        stroke-dasharray="40 60"
      >
        <animateTransform
          attributeName="transform"
          attributeType="XML"
          type="rotate"
          from="0 12 12"
          to="360 12 12"
          dur="0.8s"
          repeatCount="indefinite"
        />
      </circle>
    </svg>
  );
};
