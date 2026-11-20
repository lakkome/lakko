// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The frame every creation screen of `docs/ux/attachments.md` shares: a top
 * bar with a close icon and the heading, a scrolling body and a footer with
 * the primary action. Full screen at phone width, a dialog-sized sheet at
 * wide widths; `compact` is the poll / checklist sheet of UX 7.1a (420 px
 * wide, the title centred with the close at the start, one full-width button). `Esc` closes (through `onClose`, which may ask first) unless a
 * dialog of its own is open over it.
 */
import { onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import "./editors.css";

let nextId = 0;

export function AttachScreen(props: {
  readonly title: string;
  readonly onClose: () => void;
  readonly children: JSX.Element;
  readonly footer?: JSX.Element;
  readonly bodyClass?: string;
  readonly compact?: boolean;
}): JSX.Element {
  nextId += 1;
  const titleId = `lk-attach-title-${nextId}`;
  let close: HTMLButtonElement | undefined;
  onMount(() => {
    queueMicrotask(() => close?.focus());
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (document.querySelector("dialog[open]") !== null) return;
      e.preventDefault();
      props.onClose();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  return (
    <div class="lk-attach-layer">
      <section
        class="lk-attach-screen"
        classList={{ compact: props.compact === true }}
        role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <header class="lk-attach-head">
          <button
            ref={close}
            type="button"
            class="lk-icon-button"
            aria-label={t("common.close")}
            title={t("common.close")}
            onClick={() => props.onClose()}
          >
            <Icon name="xmark" />
          </button>
          <h2 id={titleId}>{props.title}</h2>
        </header>
        <div class={`lk-attach-body ${props.bodyClass ?? ""}`.trim()}>{props.children}</div>
        <Show when={props.footer}>
          <footer class="lk-attach-foot">{props.footer}</footer>
        </Show>
      </section>
    </div>
  );
}

/** The card that groups an editor's switches (UX 7.1a). */
export function AttachSwitchGroup(props: { readonly children: JSX.Element }): JSX.Element {
  return <div class="lk-attach-switches">{props.children}</div>;
}

/**
 * A switch row (UX 7.1a): the label at the start, a real switch (track and
 * knob) at the end, the explanation under the row, 13 px muted. The whole row
 * is the control, so a click anywhere on it toggles; `Space` / `Enter` toggle
 * from the keyboard. Never an `On` / `Off` button.
 */
export function AttachSwitch(props: {
  readonly title: string;
  readonly sub?: string;
  readonly on: boolean;
  readonly onChange: (on: boolean) => void;
  readonly disabled?: boolean;
}): JSX.Element {
  nextId += 1;
  const subId = `lk-attach-switch-sub-${nextId}`;
  return (
    <div class="lk-attach-switch-item">
      <button
        type="button"
        role="switch"
        class="lk-attach-switch"
        aria-checked={props.on}
        aria-describedby={props.sub === undefined ? undefined : subId}
        disabled={props.disabled === true}
        onClick={() => props.onChange(!props.on)}
      >
        <span class="lk-attach-switch-title">{props.title}</span>
        <span class="lk-switch-track" aria-hidden="true">
          <span class="lk-switch-knob" />
        </span>
      </button>
      <Show when={props.sub}>
        {(s) => (
          <p class="lk-attach-switch-sub" id={subId}>
            {s()}
          </p>
        )}
      </Show>
    </div>
  );
}
