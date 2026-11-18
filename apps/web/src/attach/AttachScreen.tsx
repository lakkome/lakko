// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The frame every creation screen of `docs/ux/attachments.md` shares: a top
 * bar with a close icon and the heading, a scrolling body and a footer with
 * the primary action. Full screen at phone width, a dialog-sized sheet at
 * wide widths. `Esc` closes (through `onClose`, which may ask first) unless a
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
      <section class="lk-attach-screen" role="dialog" aria-modal="true" aria-labelledby={titleId}>
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

/** A switch row: title, optional explanation, and an On / Off button. */
export function AttachSwitch(props: {
  readonly title: string;
  readonly sub?: string;
  readonly on: boolean;
  readonly onChange: (on: boolean) => void;
  readonly disabled?: boolean;
}): JSX.Element {
  return (
    <div class="lk-attach-switch">
      <div class="lk-attach-switch-body">
        <span class="lk-attach-switch-title">{props.title}</span>
        <Show when={props.sub}>{(s) => <span class="lk-attach-switch-sub">{s()}</span>}</Show>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={props.on}
        aria-label={props.title}
        classList={{ primary: props.on }}
        disabled={props.disabled === true}
        onClick={() => props.onChange(!props.on)}
      >
        {props.on ? t("common.on") : t("common.off")}
      </button>
    </div>
  );
}
