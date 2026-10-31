// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The taps of the "who can find me" screen, the `Members can find me` switch
 * and the `Their contacts can find me` switch, each bound to its `lakko_ui_rules` transition and applied through
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
import { accountIdBytes, accountIdsHexOf, concatAccountIds, contactsNarrowed } from "./contact-visibility";
import {
  CONTACTS_FORM_ALL,
  CONTACTS_FORM_CHOSEN,
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
  let narrowed = false;
  const updated = await updateVisibility((s) => {
    const next = transition(wasm, s);
    narrowed = contactsNarrowed(wasm, s, next);
    return next;
  });
  // ADR-0078 section 9, decision 4: a change of the setting rotates the
  // account's one `group` and `channel` key, so a group or channel it no
  // longer allows stops holding a working key; the profile with the new
  // grants then goes to every group. Only the device the change is made on
  // rotates; the others adopt its counters.
  if (updated !== null) {
    await reachSession()
      ?.rotateOnSettingChange(narrowed)
      .catch(() => undefined);
  }
  return updated;
}

/**
 * A change of part 2.1 alone: it says nothing about groups and channels, so
 * only the `fof` key moves, and only when someone was taken out (ADR-0060
 * section 4). Widening needs no new key: the next publication pass offers the
 * record to the contact it now allows.
 */
async function applyToContacts(
  transition: (wasm: Wasm, current: VisibilitySetting) => VisibilitySetting,
): Promise<VisibilitySetting | null> {
  const wasm = await loadWasm();
  let narrowed = false;
  const updated = await updateVisibility((s) => {
    const next = transition(wasm, s);
    narrowed = contactsNarrowed(wasm, s, next);
    return next;
  });
  if (updated !== null && narrowed) {
    await reachSession()
      ?.rotateOnContactsNarrowed()
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

export function setContactsForm(form: FormName): Promise<VisibilitySetting | null> {
  const value = form === "all" ? CONTACTS_FORM_ALL : CONTACTS_FORM_CHOSEN;
  return applyToContacts((wasm, s) => wasm.setContactsForm(s, value));
}

/** Throws on an id that is not one: the transition refuses its length. */
function idOf(accountIdHex: string): Uint8Array {
  return accountIdBytes(accountIdHex) ?? new Uint8Array(0);
}

export function setContactChosen(
  accountIdHex: string,
  on: boolean,
): Promise<VisibilitySetting | null> {
  return applyToContacts((wasm, s) => wasm.setContactChosen(s, idOf(accountIdHex), on));
}

/** Turn one contact off while "all" is on: the form becomes "only the ones I
 *  choose" with every other contact in `all` chosen. */
export function excludeContactUnderAll(
  all: readonly string[],
  excludedHex: string,
): Promise<VisibilitySetting | null> {
  return applyToContacts((wasm, s) =>
    wasm.excludeContactUnderAll(s, concatAccountIds(all), idOf(excludedHex)),
  );
}

/**
 * A contact was removed: they leave the chosen list, so adding them back brings
 * them in switched off (UX section 4.1). The removal moved the `fof` key
 * already (`onContactRemoved`).
 */
export async function forgetChosenContact(accountIdHex: string): Promise<void> {
  const held = visibilitySetting();
  const id = accountIdBytes(accountIdHex);
  if (held === null || id === null) return;
  if (!accountIdsHexOf(held.chosenContacts).includes(accountIdHex.toLowerCase())) return;
  const wasm = await loadWasm();
  await updateVisibility((s) => wasm.setContactChosen(s, id, false));
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

/** Whether the contacts of one contact can find the person, by the shared
 *  rule; `false` while no setting is loaded. Reactive through
 *  `visibilitySetting()`. */
export function allowsContact(wasm: Wasm | undefined, accountIdHex: string): boolean {
  const setting = visibilitySetting();
  const id = accountIdBytes(accountIdHex);
  if (wasm === undefined || setting === null || id === null) return false;
  return wasm.allowsContact(setting, id);
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
