// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §4 — reactive glue between a username (or display-name) input and
 * the rule decisions in `rules.ts`.
 *
 * The wasm core loads asynchronously; until it has, a field reads `unchecked`
 * and a lookup is blocked — never waved through. The decisions themselves are
 * in `rules.ts` and tested there; this file only wires them to Solid.
 */

import { createMemo, createSignal, type Accessor } from "solid-js";

import { loadWasm, type Wasm } from "../wasm";
import { checkDisplayName, checkUsername, type DisplayNameCheck, type UsernameCheck } from "./rules";

const [core, setCore] = createSignal<Wasm | null>(null);
let loading = false;

function ensureCore(): void {
  if (loading) return;
  loading = true;
  void loadWasm()
    .then((w) => setCore(() => w))
    .catch(() => {
      // Stay `unchecked`: a field that cannot be checked blocks its lookup.
      loading = false;
    });
}

/** The live state of a username field. */
export function createUsernameCheck(input: Accessor<string>): Accessor<UsernameCheck> {
  ensureCore();
  return createMemo(() => {
    const w = core();
    return checkUsername(input(), w === null ? null : (s) => w.validateUsername(s));
  });
}

/** The live state of a display-name field. */
export function createDisplayNameCheck(input: Accessor<string>): Accessor<DisplayNameCheck> {
  ensureCore();
  return createMemo(() => {
    const w = core();
    return checkDisplayName(input(), w === null ? null : (s) => w.normalizeDisplayName(s));
  });
}

/**
 * For a submit handler: the canonical username, or a thrown `Error` carrying
 * the reason copy. Waits for the core rather than reading `unchecked`, so a
 * submit that races the core load still gets a real verdict.
 */
export async function requireValidUsername(input: string): Promise<string> {
  const w = await loadWasm();
  const check = checkUsername(input, (s) => w.validateUsername(s));
  if (check.kind === "valid") return check.normalised;
  throw new Error(check.kind === "invalid" ? check.reason : "Enter a username");
}
