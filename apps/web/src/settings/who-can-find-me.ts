// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` sections 3-5, 8, 9 - what the "Who can find
 * me" screen shows for a given setting, and the copy it shows.
 *
 * What is NOT here: any rule that turns a tap into a new setting. Those are
 * `lakko_ui_rules` transitions reached over the wasm surface
 * (`chooseVisibilityMode`, `setContactsOfContacts`, `setGroupsVisible`,
 * `setGroupsForm`, `excludeScopeUnderAll`, `setScopeChosen`) and whether a
 * scope's members can find the person is `allowsScope`. A second copy here
 * would drift from the one iOS and Android use (UX section 11).
 *
 * What is here is presentation only: which rows are on screen, the helper
 * texts, the `N of M` count, the list of groups and channels (grouped,
 * sorted, filtered) and the status line. Pure: no wasm instance, no Solid, no
 * storage, so every function is testable in bare node. The setting is read
 * through a structural type the wasm `VisibilitySetting` satisfies.
 */

/** The numeric wire values of `VisibilityMode` (frozen). */
export const MODE_NOBODY = 0;
export const MODE_SOME = 1;
export const MODE_EVERYONE = 2;
/** The numeric wire values of `GroupsForm` (frozen); `undefined` is off. */
export const FORM_ALL = 1;
export const FORM_CHOSEN = 2;
/** The numeric wire values of `ScopeKind` (frozen). */
export const SCOPE_GROUP = 1;
export const SCOPE_CHANNEL = 2;

/** The fields of a `VisibilitySetting` this module reads. */
export interface SettingFields {
  readonly mode: number;
  readonly contactsOfContacts: boolean;
  readonly groups: number | undefined;
}

export type ModeName = "nobody" | "some" | "everyone";
export type FormName = "all" | "chosen";

/** The setting as the screen reads it. */
export interface VisibilityView {
  readonly mode: ModeName;
  readonly contactsOfContacts: boolean;
  /** Part 2.2 and its form; `null` when off. */
  readonly groups: FormName | null;
}

export function viewOf(s: SettingFields): VisibilityView {
  const mode: ModeName =
    s.mode === MODE_SOME ? "some" : s.mode === MODE_EVERYONE ? "everyone" : "nobody";
  const groups: FormName | null =
    s.groups === FORM_ALL ? "all" : s.groups === FORM_CHOSEN ? "chosen" : null;
  return { mode, contactsOfContacts: s.contactsOfContacts, groups };
}

/** Which rows are on screen (UX section 3). */
export interface VisibleRows {
  /** The two parts of Some people, indented under it. */
  readonly parts: boolean;
  /** All / Only the ones I choose, under Members of my groups and channels. */
  readonly groupsForm: boolean;
  /** The disclosure into the list, beside Only the ones I choose. */
  readonly listDisclosure: boolean;
}

export function visibleRows(v: VisibilityView): VisibleRows {
  const parts = v.mode === "some";
  const groupsForm = parts && v.groups !== null;
  return { parts, groupsForm, listDisclosure: groupsForm && v.groups === "chosen" };
}

/**
 * UX section 5: the `Members can find me` switch in group and channel info
 * exists only while Members of my groups and channels is on. When the
 * setting does not include groups at all, the row is absent rather than
 * dead.
 */
export function showsMembersSwitch(v: VisibilityView): boolean {
  return v.mode === "some" && v.groups !== null;
}

// ------------------------------------------------------------------ copy

export const COPY = {
  title: "Who can find me",
  lede: "Whoever can find you by username or name can ask to be your contact.",
  ledeLink: "People with your invite link or QR code can always reach you.",
  nobody: "Nobody",
  nobodyHelper: "Only people with your invite link or QR code can reach you.",
  some: "Some people",
  contactsOfContacts: "Contacts of my contacts",
  contactsOfContactsHelper: "People your contacts have added.",
  contactsOfContactsSpreading:
    "Contacts of your contacts can find you once your contacts next open Lakko.",
  groups: "Members of my groups and channels",
  groupsAllHelper: "Everyone in the groups and channels you're in.",
  all: "All groups and channels",
  chosen: "Only the ones I choose",
  everyone: "Everyone",
  everyoneHelper: "Anyone can find you by your username or name.",
  offline: "Will apply when you're back online.",
  failed: "Couldn't update who can find you. Try again.",
  retry: "Retry",
  listEmpty: "You're not in any groups or channels yet.",
  listGroups: "Groups",
  listChannels: "Channels",
  listSearch: "Search groups and channels",
  listNoMatch: "No groups or channels match.",
  membersCanFindMe: "Members can find me",
  excludedNotice: "Now only the groups and channels you choose. Change in Privacy.",
  signupLine:
    "Contacts of your contacts and people in your groups and channels can find you. You can change this in Settings > Privacy.",
  signupChange: "Change",
} as const;

/** The helper under Members of my groups and channels (UX section 3). */
export function groupsHelper(v: VisibilityView, chosen: number, total: number): string {
  if (v.groups === "chosen") return `Only the ones you choose (${chosen} of ${total}).`;
  return COPY.groupsAllHelper;
}

/**
 * The helper under Contacts of my contacts (UX section 8). Right after it is
 * turned on the widening travels through the contacts' devices, so the
 * screen says so rather than claiming it is already in effect.
 */
export function contactsOfContactsHelper(on: boolean, spreading: boolean): string {
  return on && spreading ? COPY.contactsOfContactsSpreading : COPY.contactsOfContactsHelper;
}

/** The apply states of the reach session (`reachApplyStatus`). */
export type ApplyStatus = "idle" | "applying" | "applied" | "offline" | "failed";

/** What the screen shows about applying the setting (UX sections 8, 9). */
export interface StatusLine {
  /** The small spinner beside the title. */
  readonly spinner: boolean;
  /** The line under the title, or `null`. */
  readonly message: string | null;
  /** Whether that line offers Retry. */
  readonly retry: boolean;
}

export function statusLine(status: ApplyStatus): StatusLine {
  switch (status) {
    case "applying":
      return { spinner: true, message: null, retry: false };
    case "offline":
      return { spinner: false, message: COPY.offline, retry: false };
    case "failed":
      return { spinner: false, message: COPY.failed, retry: true };
    default:
      return { spinner: false, message: null, retry: false };
  }
}

// ------------------------------------------------------------ the list

/** One group or channel the account is in. */
export interface ScopeEntry {
  readonly kind: "group" | "channel";
  /** The scope id as raw bytes (group id, or the channel id's 16 bytes). */
  readonly id: Uint8Array;
  /** A stable string key for the scope (kind and hex id). */
  readonly key: string;
  readonly name: string;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The stable key of one scope. */
export function scopeKey(kind: "group" | "channel", id: Uint8Array): string {
  return `${kind}:${hexOf(id)}`;
}

/** A channel id in its wire form (32 lowercase hex characters) as bytes;
 *  `null` when it is not hex of an even length. */
export function channelIdBytes(idHex: string): Uint8Array | null {
  if (idHex.length === 0 || idHex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(idHex)) return null;
  const out = new Uint8Array(idHex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(idHex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** More rows than fit a screen get a search field on top (UX section 4).
 *  Chosen, not measured. */
export const SEARCH_FIELD_THRESHOLD = 8;

export function showsListSearch(total: number): boolean {
  return total > SEARCH_FIELD_THRESHOLD;
}

/** Case, accents and repeated spaces never matter. */
export function foldForSearch(text: string): string {
  return text
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/\s+/g, " ")
    .trim();
}

export interface ScopeList {
  readonly groups: readonly ScopeEntry[];
  readonly channels: readonly ScopeEntry[];
}

/**
 * The list of section 4: Groups then Channels, each by name, filtered by the
 * search text (any part of the name).
 */
export function scopeList(entries: readonly ScopeEntry[], query: string): ScopeList {
  const q = foldForSearch(query);
  const matches = (e: ScopeEntry): boolean => q.length === 0 || foldForSearch(e.name).includes(q);
  const byName = (a: ScopeEntry, b: ScopeEntry): number =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.key.localeCompare(b.key);
  return {
    groups: entries.filter((e) => e.kind === "group" && matches(e)).sort(byName),
    channels: entries.filter((e) => e.kind === "channel" && matches(e)).sort(byName),
  };
}

/** `N` of `N of M`: how many of the scopes the account is in are on. */
export function chosenCount(
  entries: readonly ScopeEntry[],
  isOn: (entry: ScopeEntry) => boolean,
): number {
  let n = 0;
  for (const e of entries) if (isOn(e)) n += 1;
  return n;
}

/**
 * UX section 3: selecting Only the ones I choose for the first time opens
 * the list straight away. "First time" is read off the setting itself: the
 * person has not chosen any of their groups or channels yet. With none at
 * all it still opens: the empty list is the answer (UX section 9).
 */
export function opensListOnChoose(chosenBefore: number): boolean {
  return chosenBefore === 0;
}

/** The keys of the setting's chosen list (a `ScopeId[]` read off the wasm
 *  setting, or any structural equivalent). Unknown kinds are skipped. */
export function chosenKeysOf(
  chosen: readonly { readonly kind: number; readonly id: Uint8Array }[],
): Set<string> {
  const out = new Set<string>();
  for (const c of chosen) {
    if (c.kind === SCOPE_GROUP) out.add(scopeKey("group", c.id));
    else if (c.kind === SCOPE_CHANNEL) out.add(scopeKey("channel", c.id));
  }
  return out;
}
