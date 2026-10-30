// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §14 — the in-page confirmation host.
 *
 * Mounted ONCE, at the top level in `App.tsx`, beside `SwUpdateBanner` and for
 * the same reason: it must overlay every route (auth shell, main app shell,
 * invite landing) and no route should know it exists. Callers reach it through
 * `requestConfirm` in `confirm-dialog.ts`, which is also where every rule that
 * decides anything lives — this file holds markup and nothing else, because
 * `vitest.config.ts` runs a plain `node` environment with no jsdom and no
 * Solid JSX transform, so a `.tsx` cannot be tested at all.
 *
 * ## Why a native `<dialog>` rather than the div overlay used in `Call.tsx`
 *
 * `Call.tsx`'s `SettingsPanel` is a positioned div with `role="dialog"`, and
 * that is fine for a settings panel. It is not fine here: `showModal()` gives
 * the focus trap, the inert background and the Escape handling that a
 * confirmation for an irreversible action actually needs, and hand-rolling
 * those is how they end up subtly wrong. The visual vocabulary is unchanged —
 * the same `--surface-*` / `--radius-*` CSS variables the members panel's exit
 * block and transfer step already use — so this is that convention in a
 * correct modal container, not a third convention.
 *
 * ## Escape and cancel are the same answer
 *
 * `resolvePendingConfirm(false)` on the dialog's own `close` event covers
 * Escape, the cancel button and any browser-initiated dismissal in one place.
 * The confirm and cancel buttons resolve BEFORE the effect closes the dialog,
 * so by the time `close` fires nothing is pending and the handler is a no-op —
 * which is exactly why `resolvePendingConfirm` is a no-op when the slot is
 * empty rather than a second answer.
 *
 * ## Focus order, per UX §13
 *
 * The irreversibility statement precedes the action button in DOM order, so a
 * screen-reader user reaches *"this cannot be undone"* before the control that
 * does it. Initial focus goes to **Cancel**: the safe answer should be the one
 * a stray Return key takes. Irreversibility is carried by the body TEXT, never
 * by the action button's colour, so a greyscale reader loses nothing.
 */

import { type Component, Show, createEffect, onCleanup, onMount } from "solid-js";

import {
  pendingConfirm,
  resolvePendingConfirm,
  setConfirmHostMounted,
} from "./confirm-dialog";

export const ConfirmDialog: Component = () => {
  let el: HTMLDialogElement | undefined;
  let cancelButton: HTMLButtonElement | undefined;

  onMount(() => setConfirmHostMounted(true));
  // Unmounting with a question open resolves it `false` — see
  // `setConfirmHostMounted`. Without this the caller's `await` would hang and
  // the action would sit permanently half-started.
  onCleanup(() => setConfirmHostMounted(false));

  // Open / close follows the pending slot rather than a local flag, so there
  // is one source of truth for "is a question on screen".
  createEffect(() => {
    const open = pendingConfirm();
    if (el === undefined) return;
    if (open === null) {
      if (el.open) el.close();
      return;
    }
    if (!el.open) {
      el.showModal();
      cancelButton?.focus();
    }
  });

  return (
    <dialog
      ref={el}
      aria-labelledby="lakko-confirm-title"
      aria-describedby="lakko-confirm-body"
      onClose={() => resolvePendingConfirm(false)}
      onClick={(e) => {
        // Click-out to dismiss — only when the backdrop itself is the target.
        // A native `<dialog>` reports a backdrop click as a click on the
        // dialog ELEMENT, so the padding must live on the inner wrapper: with
        // padding here, a click on the card's own margin would read as a
        // backdrop click and dismiss.
        if (e.target === e.currentTarget) resolvePendingConfirm(false);
      }}
      style={{
        border: "1px solid var(--surface-border)",
        "border-radius": "var(--radius-md)",
        background: "var(--surface-elevated)",
        color: "var(--surface-fg)",
        "box-shadow": "var(--shadow-1)",
        padding: "0",
        width: "min(28rem, calc(100vw - 2rem))",
        "font-size": "0.85rem",
      }}
    >
      <Show when={pendingConfirm()}>
        {(open) => (
          <div
            style={{
              display: "flex",
              "flex-direction": "column",
              gap: "0.6rem",
              padding: "1rem 1.1rem",
            }}
          >
            <strong
              id="lakko-confirm-title"
              style={{ "font-size": "0.95rem", color: "var(--surface-fg)" }}
            >
              {open().request.title}
            </strong>

            {/* The body precedes the actions in DOM order, which is focus
                order — UX §13's rule for the transfer step, and it applies to
                every confirmation for the same reason. */}
            <Show when={open().request.body !== null}>
              <span
                id="lakko-confirm-body"
                style={{
                  "font-size": "0.8rem",
                  color: "var(--surface-muted)",
                  "line-height": "1.45",
                }}
              >
                {open().request.body}
              </span>
            </Show>

            <div
              style={{
                display: "flex",
                gap: "0.5rem",
                "justify-content": "flex-end",
                "margin-top": "0.2rem",
                "flex-wrap": "wrap",
              }}
            >
              {/* Cancel first in DOM order so it takes initial focus and so a
                  keyboard user reaches the safe answer first. */}
              <Show when={open().request.notice !== true}>
              <button
                ref={(el) => {
                  cancelButton = el;
                }}
                type="button"
                onClick={() => resolvePendingConfirm(false)}
                style={{
                  "font-size": "0.8rem",
                  padding: "0.35rem 0.75rem",
                  "border-radius": "var(--radius-sm)",
                  border: "1px solid var(--surface-border)",
                  background: "var(--surface-bg)",
                  color: "var(--surface-fg)",
                  cursor: "pointer",
                }}
              >
                {open().request.cancelLabel}
              </button>
              </Show>
              <Show when={open().request.extra}>
                {(extra) => (
                  <button
                    type="button"
                    onClick={() => extra().onPress()}
                    style={{
                      "font-size": "0.8rem",
                      padding: "0.35rem 0.75rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--surface-border)",
                      background: "var(--surface-bg)",
                      color: "var(--surface-fg)",
                      cursor: "pointer",
                    }}
                  >
                    {extra().label}
                  </button>
                )}
              </Show>
              <button
                ref={(el) => {
                  // A notice's only button takes the initial focus.
                  if (open().request.notice === true) cancelButton = el;
                }}
                type="button"
                onClick={() => resolvePendingConfirm(true)}
                style={{
                  "font-size": "0.8rem",
                  padding: "0.35rem 0.75rem",
                  "border-radius": "var(--radius-sm)",
                  border: "1px solid transparent",
                  background:
                    open().request.tone === "danger"
                      ? "var(--danger)"
                      : "var(--accent)",
                  color:
                    open().request.tone === "danger"
                      ? "var(--danger-fg)"
                      : "var(--accent-fg)",
                  "font-weight": "600",
                  cursor: "pointer",
                }}
              >
                {open().request.confirmLabel}
              </button>
            </div>
          </div>
        )}
      </Show>
    </dialog>
  );
};
