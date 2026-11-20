// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The panel's small shared pieces: the dialog, notices, a loading line. */

import { type JSX, Show, createEffect, onCleanup } from "solid-js";

export { Icon } from "../../web/src/ui/icons";
export { LakkoPicker, type PickerOption } from "../../web/src/ui/LakkoPicker";

/**
 * The design system's dialog (`LakkoDialog`) as a native `<dialog>`: a
 * centred card over a dimmed screen; Escape, the dim and Cancel all mean
 * cancel; initial focus on Cancel. Never the browser's own `confirm()`.
 */
export function Dialog(props: {
  readonly open: boolean;
  readonly title: string;
  readonly onCancel: () => void;
  readonly children: JSX.Element;
}): JSX.Element {
  let el: HTMLDialogElement | undefined;
  createEffect(() => {
    if (el === undefined) return;
    if (props.open && !el.open) {
      el.showModal();
      const cancel = el.querySelector<HTMLButtonElement>("button[data-cancel]");
      cancel?.focus();
    } else if (!props.open && el.open) {
      el.close();
    }
  });
  onCleanup(() => {
    if (el?.open === true) el.close();
  });
  return (
    <dialog
      ref={el}
      class="mp-dialog"
      aria-label={props.title}
      onCancel={(e) => {
        e.preventDefault();
        props.onCancel();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onCancel();
      }}
    >
      <Show when={props.open}>
        <div class="mp-dialog-card">
          <h2>{props.title}</h2>
          {props.children}
        </div>
      </Show>
    </dialog>
  );
}

export function Notice(props: { readonly tone?: "danger" | "warning" | undefined; readonly children: JSX.Element }): JSX.Element {
  return (
    <div class="mp-notice" data-tone={props.tone} role={props.tone === "danger" ? "alert" : "status"}>
      {props.children}
    </div>
  );
}

export function Loading(props: { readonly label?: string }): JSX.Element {
  return (
    <p class="form-info" style={{ display: "flex", "align-items": "center", gap: "8px" }}>
      <span class="mp-spinner" aria-hidden="true" />
      {props.label ?? "Loading..."}
    </p>
  );
}

/** A button that shows a spinner instead of its label while busy. */
export function BusyButton(props: {
  readonly busy: boolean;
  readonly class?: string;
  readonly disabled?: boolean;
  readonly onClick: () => void;
  readonly children: JSX.Element;
}): JSX.Element {
  return (
    <button
      type="button"
      class={props.class}
      disabled={props.disabled === true || props.busy}
      aria-busy={props.busy}
      onClick={() => props.onClick()}
    >
      <Show when={props.busy} fallback={props.children}>
        <span class="mp-spinner" aria-hidden="true" />
        <span class="lk-visually-hidden" style={{ position: "absolute", width: "1px", height: "1px", overflow: "hidden" }}>
          Working
        </span>
      </Show>
    </button>
  );
}
