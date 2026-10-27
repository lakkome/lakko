// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §14 — the in-page confirmation that replaces `window.confirm`.
 *
 * ## What this is for
 *
 * §14: *"Web's `window.confirm` is replaced by an in-page dialog so the body
 * line in §6 reads as part of the app rather than as a browser alert, and so
 * the destructive action can carry destructive styling."* It was recorded and
 * deferred because it applied to more than one flow, and doing it for one
 * would have left the inconsistency in a worse place than it found it.
 *
 * ## Why the widget change is not cosmetic
 *
 * Three properties, of which only the first is about looks.
 *
 * 1. **The irreversibility statement has to be the app's.** §6 and §6.2.4 both
 *    write copy whose whole job is to state, plainly, that something cannot be
 *    undone. `window.confirm` renders that under a chrome-supplied title, in
 *    the browser's voice, outside the app — so the app's own irreversibility
 *    sentence competes with a heading it did not write. §6.1 already made this
 *    argument for the transfer step and resolved it the same way.
 *
 * 2. **The action button has to name the person or thing.** §6.1: the action
 *    button's accessible name is its visible label, so a screen-reader user
 *    reaching the point of no return hears WHO or WHAT is affected.
 *    `window.confirm` renders a browser-supplied OK/Cancel pair; naming the
 *    target is not merely awkward there, it is impossible beyond stuffing the
 *    name into the single message string — which is what `PasskeysSection`
 *    did (`Remove passkey "laptop"?`) and what §6's own generic title shape
 *    exists to avoid.
 *
 * 3. **It can be switched off, and then the control is a lie.** A browser that
 *    has shown several dialogs offers *"prevent this page from creating
 *    additional dialogs"*; after that `window.confirm` returns `false` without
 *    rendering anything. Every call site reads `false` as *cancel*, so the
 *    button goes silently dead — press `Leave group`, nothing happens, no
 *    dialog, no error, no notice. The direction is safe (nothing destructive
 *    fires) but a confirmation the user's browser can turn off is not a
 *    confirmation, and a dead control is its own defect.
 *
 * ## Why the decision lives here and not in `ConfirmDialog.tsx`
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in a plain `node`
 * environment with no jsdom and no Solid JSX transform, so a `.tsx` component
 * cannot be tested at all. Everything that DECIDES therefore lives in this
 * module — how an action names its target, and what answer a request resolves
 * to when nobody can be asked — and the component is left holding markup.
 * The same split `chat/remove-member-confirm.ts` already uses, and the shape
 * every flow's copy module here reaches.
 *
 * ## The resolution rules, and why each fails to `false`
 *
 * A destructive question that was not answered is refused. Three ways it can
 * go unanswered, all resolving `false`:
 *
 *   - **no host mounted** — nobody can be asked, so nothing is granted. This
 *     is what lets `backup/stale-restore-confirm.ts` keep the ADR-0037 F5a
 *     fail-closed posture it had when its default was `window.confirm`;
 *   - **the host unmounts with a question open** — a route change took the
 *     dialog off the screen mid-question;
 *   - **a second request arrives while one is open** — refused, and the FIRST
 *     is left untouched and still answerable, so a stray second caller cannot
 *     cancel the question the user is currently reading.
 *
 * Escape, the cancel button and a backdrop dismissal all land on the same
 * `false`, and the caller shape is uniformly
 * `if (!(await requestConfirm(...))) return;` — so a cancel leaves the action
 * unstarted rather than half-done.
 */

import { t } from "../l10n";
import { createSignal } from "solid-js";

/** How the action button is coloured. `danger` is the destructive treatment
 *  §6 asks for; `neutral` exists for a confirmation that is merely
 *  consequential, and has no caller today. */
export type ConfirmTone = "danger" | "neutral";

/** The copy one confirmation needs, whichever flow raised it.
 *
 *  Structurally identical to `chat/remove-member-confirm.ts`'s
 *  `RemoveMemberConfirmation` plus the tone, so that settled module's return
 *  value is assignable here without being edited. */
export interface ConfirmRequest {
  /** The question. §6's shape: `Remove this member?`, not a statement. */
  readonly title: string;
  /** The one fact the user needs, or `null` when there is nothing true to
   *  add — see `remove-member-confirm.ts` on why a body is dropped rather
   *  than reworded when it would be false. */
  readonly body: string | null;
  /** The destructive action, naming its target — see {@link namedAction}. */
  readonly confirmLabel: string;
  /** The exit. Always present: there is no confirmation without one. */
  readonly cancelLabel: string;
  /** {@link ConfirmTone}. */
  readonly tone: ConfirmTone;
  /** A notice with nothing to decide (design system: "A notice with nothing
   *  to decide has one primary button"): only `confirmLabel` is shown, and
   *  the answer carries no meaning. */
  readonly notice?: boolean;
}

/** A question currently on screen, and the promise waiting on its answer. */
export interface PendingConfirm {
  /** What the host renders. */
  readonly request: ConfirmRequest;
  /** Settles the `requestConfirm` promise. Call via
   *  {@link resolvePendingConfirm}, never directly — that is what clears the
   *  pending slot in the same step. */
  readonly resolve: (answer: boolean) => void;
}

const [pending, setPending] = createSignal<PendingConfirm | null>(null);

/** Whether a `ConfirmDialog` host is currently on screen. Plain state rather
 *  than a signal: it gates `requestConfirm` imperatively and nothing renders
 *  from it. */
let hostMounted = false;

/** The open question, for the host to render. `null` when nothing is asked. */
export function pendingConfirm(): PendingConfirm | null {
  return pending();
}

/**
 * Register / unregister the single `ConfirmDialog` host.
 *
 * Unmounting with a question open resolves it `false`: the dialog is off the
 * screen, so the question was never answered, and an unanswered destructive
 * question is a refusal. Without this the caller's `await` would hang forever
 * and the action would sit permanently half-started.
 */
export function setConfirmHostMounted(mounted: boolean): void {
  hostMounted = mounted;
  if (mounted) return;
  const open = pending();
  if (open === null) return;
  setPending(null);
  open.resolve(false);
}

/**
 * Ask the user to confirm a destructive action.
 *
 * Resolves `true` only on an explicit press of the confirm button. Every
 * other outcome — cancel, Escape, no host, host unmounted, a second request
 * while one is open — resolves `false`.
 */
export function requestConfirm(request: ConfirmRequest): Promise<boolean> {
  if (!hostMounted) return Promise.resolve(false);
  if (pending() !== null) return Promise.resolve(false);
  return new Promise<boolean>((resolve) => {
    setPending({ request, resolve });
  });
}

/**
 * Answer the open question. A no-op when nothing is pending, so a late click
 * on a dialog that has already closed cannot answer the NEXT question.
 */
export function resolvePendingConfirm(answer: boolean): void {
  const open = pending();
  if (open === null) return;
  setPending(null);
  open.resolve(answer);
}

/**
 * Compose an action label that names what it acts on — §6.1's rule, applied
 * wherever a target has a name.
 *
 * The verb is the spec's own label (`Remove`, `Delete`, `Leave`) and stays
 * exactly that; the name is appended. An absent, empty or whitespace-only
 * name falls back to the bare verb rather than producing `Remove ` or
 * `Remove undefined` — a roster label degrades to a fingerprint and a
 * fingerprint can itself be missing on a torn read, so the fallback is a real
 * path and not a defensive flourish.
 */
export function namedAction(
  verb: string,
  name: string | null | undefined,
): string {
  const trimmed = typeof name === "string" ? name.trim() : "";
  return trimmed.length === 0 ? verb : `${verb} ${trimmed}`;
}

/**
 * {@link namedAction} for a catalog verb (ADR-0061): the bare verb from
 * `bareKey`, or `namedKey` with the target as `{name}`, so a translator can
 * put the name where the language wants it.
 */
export function namedActionKey(
  bareKey: string,
  namedKey: string,
  name: string | null | undefined,
): string {
  const trimmed = typeof name === "string" ? name.trim() : "";
  return trimmed.length === 0 ? t(bareKey) : t(namedKey, { name: trimmed });
}
