// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Rows whose control is named by the text a person sees on them.
 *
 * A `<label>` that wraps an input names it only through the label's whole
 * text content, and assistive tools that read the tree without computing
 * that (base walk 2026-11-13: a checkbox read as "on", a row button with no
 * name at all) are left with nothing. Each control here points at its
 * visible title with `aria-labelledby`, and at the line under it with
 * `aria-describedby`, so its name is that title whatever reads it.
 */
import { createUniqueId, Show, type JSX } from "solid-js";
import { Icon } from "./icons";

/** A list row with a check box: `label` is its name. */
export function PickRow(props: {
  readonly label: string;
  readonly checked: boolean;
  readonly disabled?: boolean;
  readonly onToggle: () => void;
  /** Drawn before the title (an avatar). */
  readonly children?: JSX.Element;
}): JSX.Element {
  const titleId = createUniqueId();
  return (
    <label class="lk-row pick">
      <input
        type="checkbox"
        aria-labelledby={titleId}
        checked={props.checked}
        disabled={props.disabled === true}
        onChange={() => props.onToggle()}
      />
      {props.children}
      <div class="lk-row-body">
        <span class="lk-row-title" id={titleId}>
          {props.label}
        </span>
      </div>
    </label>
  );
}

/** A settings row with a radio or a check box, its title and an optional
 *  line under it. */
export function ChoiceRow(props: {
  readonly type: "radio" | "checkbox";
  readonly name?: string;
  readonly title: string;
  readonly sub?: string | undefined;
  readonly checked: boolean;
  readonly onChange: (checked: boolean) => void;
  /** The class of the `<label>`; a row that carries an action beside its
   *  choice wraps this in its own row. */
  readonly class?: string;
  /** A choice that cannot be taken now: drawn and announced as disabled. */
  readonly disabled?: boolean;
}): JSX.Element {
  const titleId = createUniqueId();
  const subId = createUniqueId();
  return (
    <label class={props.class ?? "lk-card-row lk-privacy-choice"} classList={{ disabled: props.disabled === true }}>
      <input
        type={props.type}
        disabled={props.disabled === true}
        name={props.name}
        aria-labelledby={titleId}
        aria-describedby={props.sub !== undefined && props.sub.length > 0 ? subId : undefined}
        checked={props.checked}
        onChange={(e) => props.onChange(e.currentTarget.checked)}
      />
      <span class="lk-card-row-body">
        <span class="lk-card-row-title" id={titleId}>
          {props.title}
        </span>
        <Show when={props.sub !== undefined && props.sub.length > 0}>
          <span class="lk-card-row-sub" id={subId}>
            {props.sub}
          </span>
        </Show>
      </span>
    </label>
  );
}

/** One row of an info screen's actions: icon, title, the value at the end,
 *  opens a pushed screen. Its name is the title, then the value. */
export function InfoRowButton(props: {
  readonly icon: Parameters<typeof Icon>[0]["name"];
  readonly title: string;
  readonly value?: string | undefined;
  readonly onOpen: () => void;
}): JSX.Element {
  const titleId = createUniqueId();
  const valueId = createUniqueId();
  const hasValue = (): boolean => props.value !== undefined && props.value.length > 0;
  return (
    <button
      type="button"
      class="lk-card-row lk-card-row-button"
      aria-labelledby={hasValue() ? `${titleId} ${valueId}` : titleId}
      onClick={() => props.onOpen()}
    >
      <span class="lk-card-row-icon">
        <Icon name={props.icon} size={20} />
      </span>
      <span class="lk-card-row-body">
        <span class="lk-card-row-title" id={titleId}>
          {props.title}
        </span>
      </span>
      <Show when={hasValue()}>
        <span class="lk-card-row-sub" id={valueId}>
          {props.value}
        </span>
      </Show>
      <Icon name="chevron-forward" size={14} />
    </button>
  );
}
