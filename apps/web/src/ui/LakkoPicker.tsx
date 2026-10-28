// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `LakkoPicker` — one value from a short list (design system section 3,
 * "Picker"; the same shape as `LakkoPicker` on iOS and Android).
 *
 * A field-shaped button shows the current value and a chevron; activating it
 * opens the app's own list of rows with a check mark on the current value;
 * picking a row closes it. It replaces the browser's `<select>`, which opens
 * the operating system's picker on a phone (design system section 6). The
 * guard `ui/no-native-select.test.ts` keeps `<select>` from coming back.
 *
 * The list is positioned `fixed` from the button's rectangle, so a scrolling
 * or clipped container (the feed editor's conversation list) cannot cut it
 * off. On a phone-width screen the stylesheet turns it into a bottom sheet.
 *
 * The keyboard rules live in `picker.ts`, which has tests; this file holds
 * markup and focus handling only, since `vitest` has no Solid JSX transform.
 */

import {
  type Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  onCleanup,
} from "solid-js";

import { Icon } from "./icons";
import { initialPickerIndex, pickerKey } from "./picker";

/** One row. */
export interface PickerOption<V extends string = string> {
  readonly value: V;
  readonly label: string;
}

export interface LakkoPickerProps<V extends string = string> {
  readonly value: V;
  readonly options: readonly PickerOption<V>[];
  readonly onChange: (value: V) => void;
  readonly disabled?: boolean;
  /** The id of the visible label that names the field. */
  readonly labelledBy?: string;
  /** A name for the field when no visible label exists. */
  readonly ariaLabel?: string;
  /** Shown when the value is not one of the options. */
  readonly placeholder?: string;
  readonly class?: string;
}

/** Below this width the list opens as a bottom sheet (matches styles.css). */
const SHEET_MAX_WIDTH = 560;
const GAP = 4;
const EDGE = 8;

export function LakkoPicker<V extends string = string>(props: LakkoPickerProps<V>): ReturnType<Component> {
  const id = createUniqueId();
  const valueId = `lk-picker-value-${id}`;
  const hiddenLabelId = `lk-picker-label-${id}`;
  const listId = `lk-picker-list-${id}`;
  const optionId = (i: number): string => `lk-picker-opt-${id}-${i}`;

  const [open, setOpen] = createSignal(false);
  const [active, setActive] = createSignal(-1);
  const [place, setPlace] = createSignal<Record<string, string>>({});

  let button: HTMLButtonElement | undefined;
  let list: HTMLDivElement | undefined;

  const selectedIndex = createMemo(() => props.options.findIndex((o) => o.value === props.value));
  const currentLabel = (): string => {
    const i = selectedIndex();
    return i >= 0 ? (props.options[i]?.label ?? "") : (props.placeholder ?? "");
  };

  const labelledBy = (): string => {
    const name = props.labelledBy ?? (props.ariaLabel !== undefined ? hiddenLabelId : undefined);
    return name === undefined ? valueId : `${name} ${valueId}`;
  };

  const position = (): void => {
    if (button === undefined) return;
    if (window.innerWidth <= SHEET_MAX_WIDTH) {
      setPlace({});
      return;
    }
    const r = button.getBoundingClientRect();
    const below = window.innerHeight - r.bottom - GAP - EDGE;
    const above = r.top - GAP - EDGE;
    const style: Record<string, string> = {
      left: `${Math.max(EDGE, Math.min(r.left, window.innerWidth - r.width - EDGE))}px`,
      "min-width": `${r.width}px`,
    };
    if (below >= 160 || below >= above) {
      style.top = `${r.bottom + GAP}px`;
      style["max-height"] = `${Math.max(120, below)}px`;
    } else {
      style.bottom = `${window.innerHeight - r.top + GAP}px`;
      style["max-height"] = `${Math.max(120, above)}px`;
    }
    setPlace(style);
  };

  const openList = (index: number): void => {
    if (props.disabled === true) return;
    position();
    setActive(index);
    setOpen(true);
  };

  const close = (returnFocus: boolean): void => {
    if (!open()) return;
    setOpen(false);
    if (returnFocus) button?.focus();
  };

  const pick = (index: number): void => {
    const option = props.options[index];
    close(true);
    if (option !== undefined && option.value !== props.value) props.onChange(option.value);
  };

  // While open: focus the list, keep it placed, close on an outside press.
  createEffect(() => {
    if (!open()) return;
    queueMicrotask(() => list?.focus());
    const onPointerDown = (e: PointerEvent): void => {
      const target = e.target as Node | null;
      if (target !== null && (list?.contains(target) === true || button?.contains(target) === true)) return;
      close(false);
    };
    const onReflow = (e: Event): void => {
      if (e.target instanceof Node && list?.contains(e.target) === true) return;
      position();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("resize", onReflow);
    window.addEventListener("scroll", onReflow, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("resize", onReflow);
      window.removeEventListener("scroll", onReflow, true);
    });
  });

  // Keep the active row in view while moving with the keyboard.
  createEffect(() => {
    if (!open()) return;
    const i = active();
    if (i < 0) return;
    document.getElementById(optionId(i))?.scrollIntoView({ block: "nearest" });
  });

  // A disabled field never stays open.
  createEffect(() => {
    if (props.disabled === true) close(false);
  });

  const onButtonKey = (e: KeyboardEvent): void => {
    if (props.disabled === true) return;
    const r = pickerKey(e.key, false, selectedIndex(), props.options.length);
    if (r.prevent) e.preventDefault();
    if (r.action === "open") openList(r.index);
  };

  const onListKey = (e: KeyboardEvent): void => {
    const r = pickerKey(e.key, true, active(), props.options.length);
    if (r.prevent) e.preventDefault();
    // Escape closes the list only, never the dialog or layer around it.
    if (r.action === "close" && e.key === "Escape") e.stopPropagation();
    switch (r.action) {
      case "move":
        setActive(r.index);
        break;
      case "pick":
        pick(r.index);
        break;
      case "close":
        close(e.key === "Escape");
        break;
      default:
        break;
    }
  };

  return (
    <div class={`lk-picker${props.class !== undefined ? ` ${props.class}` : ""}`}>
      <Show when={props.labelledBy === undefined && props.ariaLabel !== undefined}>
        <span id={hiddenLabelId} class="lk-visually-hidden">
          {props.ariaLabel}
        </span>
      </Show>
      <button
        ref={button}
        type="button"
        class="lk-picker-btn"
        aria-haspopup="listbox"
        aria-expanded={open()}
        aria-controls={open() ? listId : undefined}
        aria-labelledby={labelledBy()}
        disabled={props.disabled === true}
        onClick={() => {
          // Never place the picker inside a `<label>`: the label would forward
          // a click on a row back to this button and reopen the list. Name it
          // with `labelledBy` instead.
          if (open()) close(true);
          else openList(initialPickerIndex(selectedIndex(), props.options.length));
        }}
        onKeyDown={onButtonKey}
      >
        <span id={valueId} class="lk-picker-value lk-ellipsis">
          {currentLabel()}
        </span>
        <span class="lk-picker-chevron" aria-hidden="true">
          <Icon name="chevron" size={16} />
        </span>
      </button>
      <Show when={open()}>
        <div class="lk-picker-scrim" aria-hidden="true" />
        <div
          ref={list}
          id={listId}
          class="lk-picker-list"
          role="listbox"
          tabindex="-1"
          aria-labelledby={props.labelledBy ?? (props.ariaLabel !== undefined ? hiddenLabelId : valueId)}
          aria-activedescendant={active() >= 0 ? optionId(active()) : undefined}
          style={place()}
          onKeyDown={onListKey}
          onFocusOut={(e) => {
            const next = e.relatedTarget as Node | null;
            if (next !== null && list?.contains(next) === true) return;
            if (next !== null && next === button) return;
            close(false);
          }}
        >
          <For each={props.options}>
            {(option, i) => (
              <div
                id={optionId(i())}
                class="lk-picker-option"
                classList={{ active: active() === i() }}
                role="option"
                aria-selected={option.value === props.value}
                onPointerEnter={() => setActive(i())}
                onClick={() => pick(i())}
              >
                <span class="lk-ellipsis">{option.label}</span>
                <span class="lk-picker-check" aria-hidden="true">
                  <Show when={option.value === props.value}>
                    <Icon name="check" size={18} />
                  </Show>
                </span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
