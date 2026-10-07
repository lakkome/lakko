// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A password input with a show / hide toggle (docs/ux/client-redesign.md
 * section 5.6). The toggle is local: it flips the input's type and nothing
 * else, so the value never leaves the field.
 */
import { createSignal, type JSX } from "solid-js";
import { Icon } from "./icons";
import { t } from "../l10n";

export function PasswordField(props: {
  readonly label: string;
  readonly value: () => string;
  readonly setValue: (v: string) => void;
  readonly autocomplete: "current-password" | "new-password";
  readonly disabled?: () => boolean;
  readonly required?: boolean;
  readonly minLength?: number;
  /** The input element, for a surface that keeps focus in the field. */
  readonly inputRef?: (el: HTMLInputElement) => void;
  /** The id of the helper line that describes the field. */
  readonly describedBy?: string;
}): JSX.Element {
  const [shown, setShown] = createSignal(false);
  return (
    <label>
      {props.label}
      <div class="lk-password">
        <input
          ref={(el) => props.inputRef?.(el)}
          aria-describedby={props.describedBy}
          type={shown() ? "text" : "password"}
          autocomplete={props.autocomplete}
          required={props.required ?? true}
          minLength={props.minLength}
          value={props.value()}
          onInput={(e) => props.setValue(e.currentTarget.value)}
          disabled={props.disabled?.() === true}
        />
        <button
          type="button"
          class="lk-icon-button"
          aria-label={shown() ? t("auth.password.hide") : t("auth.password.show")}
          aria-pressed={shown()}
          disabled={props.disabled?.() === true}
          onClick={() => setShown((v) => !v)}
        >
          <Icon name={shown() ? "eyeOff" : "eye"} size={18} />
        </button>
      </div>
    </label>
  );
}
