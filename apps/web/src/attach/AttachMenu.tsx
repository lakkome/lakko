// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 2 — the paperclip and its `LakkoMenu`:
 * a popover above the paperclip at wide widths, a bottom sheet of 52-point
 * rows at phone width (the CSS decides). No heading row, no Cancel row:
 * dismissing is Cancel. `Enter` / `Space` on the paperclip opens it, arrows
 * move, `Enter` picks, `Escape` closes. A disabled row reads its reason.
 */
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { MenuBackdrop } from "../ui/MenuBackdrop";
import { DISABLED_KEY, ITEM_ICON, ITEM_LABEL_KEY, nextEnabled, separatorBefore } from "./attach-menu";
import type { AttachItem, AttachMenuEntry } from "./types";
import "./attach-menu.css";

export function AttachMenu(props: {
  readonly entries: () => readonly AttachMenuEntry[];
  readonly disabled?: boolean;
  readonly onPick: (item: AttachItem) => void;
}): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const [focus, setFocus] = createSignal(0);
  let button: HTMLButtonElement | undefined;
  const rows: HTMLButtonElement[] = [];

  const openMenu = (): void => {
    const entries = props.entries();
    if (entries.length === 0) return;
    setFocus(nextEnabled(entries, -1, 1));
    setOpen(true);
    queueMicrotask(() => rows[focus()]?.focus());
  };
  const close = (refocus = true): void => {
    setOpen(false);
    if (refocus) button?.focus();
  };
  const pick = (entry: AttachMenuEntry): void => {
    if (entry.disabled !== undefined) return;
    close(false);
    props.onPick(entry.item);
  };
  const move = (step: 1 | -1): void => {
    const next = nextEnabled(props.entries(), focus(), step);
    if (next < 0) return;
    setFocus(next);
    rows[next]?.focus();
  };

  onMount(() => {
    const onResize = (): void => {
      if (open()) close(false);
    };
    window.addEventListener("resize", onResize);
    onCleanup(() => window.removeEventListener("resize", onResize));
  });

  return (
    <Show when={props.entries().length > 0}>
      <div class="lk-attach-anchor">
        <button
          ref={button}
          type="button"
          class="lk-icon-button"
          aria-haspopup="menu"
          aria-expanded={open()}
          aria-label={t("attach.menu.label")}
          title={t("attach.menu.label")}
          disabled={props.disabled === true}
          onClick={() => (open() ? close() : openMenu())}
        >
          <Icon name="paperclip" size={20} />
        </button>
        <Show when={open()}>
          <MenuBackdrop onClose={() => close()} closeOnContextMenu />
          <div
            class="lk-attach-menu"
            role="menu"
            aria-label={t("attach.menu.label")}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                move(1);
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                move(-1);
              } else if (e.key === "Escape") {
                e.preventDefault();
                close();
              } else if (e.key === "Tab") {
                close(false);
              }
            }}
          >
            <For each={props.entries()}>
              {(entry, i) => (
                <>
                  <Show when={separatorBefore(props.entries(), i())}>
                    <div class="lk-attach-menu-sep" role="separator" />
                  </Show>
                  <button
                    ref={(el) => (rows[i()] = el)}
                    type="button"
                    role="menuitem"
                    class="lk-attach-menu-row"
                    tabindex={focus() === i() ? 0 : -1}
                    aria-disabled={entry.disabled !== undefined ? "true" : undefined}
                    aria-label={
                      entry.disabled === undefined
                        ? t(ITEM_LABEL_KEY[entry.item])
                        : `${t(ITEM_LABEL_KEY[entry.item])}, ${t(DISABLED_KEY[entry.disabled])}`
                    }
                    classList={{ disabled: entry.disabled !== undefined }}
                    onClick={() => pick(entry)}
                  >
                    <span class="lk-attach-menu-icon" aria-hidden="true">
                      <Icon name={ITEM_ICON[entry.item]} size={20} />
                    </span>
                    <span class="lk-attach-menu-text">
                      <span class="lk-attach-menu-label">{t(ITEM_LABEL_KEY[entry.item])}</span>
                      <Show when={entry.disabled}>
                        {(d) => <span class="lk-attach-menu-reason">{t(DISABLED_KEY[d()])}</span>}
                      </Show>
                    </span>
                  </button>
                </>
              )}
            </For>
          </div>
        </Show>
      </div>
    </Show>
  );
}
