// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The taps of the "who can find me" screen and the `Members can find me`
 * switch, each bound to its `lakko_ui_rules` transition and applied through
 * `updateVisibility` (which stamps, persists and publishes the setting; the
 * reach session then syncs on its own).
 *
 * No rule is written here: every function names the wasm transition it runs
 * and nothing more. A `ScopeId` handed to wasm is built fresh for the call,
 * because a wasm-bindgen `Vec<ScopeId>` argument takes the JS objects over.
 */
import type { ScopeId, VisibilitySetting } from "../../wasm/lakko_ffi_wasm";
import { updateVisibility, visibilitySetting } from "../reach/visibility-store";
import { reachSession } from "../reach/reach-service";
import { loadWasm, type Wasm } from "../wasm";
import {
  FORM_ALL,
  FORM_CHOSEN,
  MODE_EVERYONE,
  MODE_NOBODY,
  MODE_SOME,
  SCOPE_CHANNEL,
  SCOPE_GROUP,
  type FormName,
  type ModeName,
  type ScopeEntry,
} from "./who-can-find-me";

function scopeIdOf(wasm: Wasm, entry: Pick<ScopeEntry, "kind" | "id">): ScopeId {
  return new wasm.ScopeId(entry.kind === "group" ? SCOPE_GROUP : SCOPE_CHANNEL, entry.id);
}

async function apply(
  transition: (wasm: Wasm, current: VisibilitySetting) => VisibilitySetting,
): Promise<VisibilitySetting | null> {
  const wasm = await loadWasm();
  const updated = await updateVisibility((s) => transition(wasm, s));
  // ADR-0078 section 9, decision 4: a change of the setting rotates the
  // account's one `group` and `channel` key, so a group or channel it no
  // longer allows stops holding a working key; the profile with the new
  // grants then goes to every group. Only the device the change is made on
  // rotates; the others adopt its counters.
  if (updated !== null) {
    await reachSession()
      ?.rotateOnSettingChange()
      .catch(() => undefined);
  }
  return updated;
}

export function chooseMode(mode: ModeName): Promise<VisibilitySetting | null> {
  const value = mode === "some" ? MODE_SOME : mode === "everyone" ? MODE_EVERYONE : MODE_NOBODY;
  return apply((wasm, s) => wasm.chooseVisibilityMode(s, value));
}

export function setContactsOfContacts(on: boolean): Promise<VisibilitySetting | null> {
  return apply((wasm, s) => wasm.setContactsOfContacts(s, on));
}

export function setGroupsVisible(on: boolean): Promise<VisibilitySetting | null> {
  return apply((wasm, s) => wasm.setGroupsVisible(s, on));
}

export function setGroupsForm(form: FormName): Promise<VisibilitySetting | null> {
  return apply((wasm, s) => wasm.setGroupsForm(s, form === "all" ? FORM_ALL : FORM_CHOSEN));
}

export function setScopeChosen(
  entry: Pick<ScopeEntry, "kind" | "id">,
  on: boolean,
): Promise<VisibilitySetting | null> {
  return apply((wasm, s) => {
    const scope = scopeIdOf(wasm, entry);
    try {
      return wasm.setScopeChosen(s, scope, on);
    } finally {
      scope.free();
    }
  });
}

/** Turn one scope off while "all" is on: the form becomes "only the ones I
 *  choose" with every other scope in `all` chosen. */
export function excludeScopeUnderAll(
  all: readonly Pick<ScopeEntry, "kind" | "id">[],
  excluded: Pick<ScopeEntry, "kind" | "id">,
): Promise<VisibilitySetting | null> {
  return apply((wasm, s) => {
    const scopes = all.map((e) => scopeIdOf(wasm, e));
    const target = scopeIdOf(wasm, excluded);
    try {
      return wasm.excludeScopeUnderAll(s, scopes, target);
    } finally {
      target.free();
    }
  });
}

/** Whether the members of one scope can find the person, by the shared
 *  rule; `false` while no setting is loaded. Reactive through
 *  `visibilitySetting()`. */
export function allowsScope(wasm: Wasm | undefined, entry: Pick<ScopeEntry, "kind" | "id">): boolean {
  const setting = visibilitySetting();
  if (wasm === undefined || setting === null) return false;
  const scope = scopeIdOf(wasm, entry);
  try {
    return wasm.allowsScope(setting, scope);
  } finally {
    scope.free();
  }
}
