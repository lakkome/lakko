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

import { compareContactLabels } from "../contacts/contact-list-model";
import { t } from "../l10n";

/** The numeric wire values of `VisibilityMode` (frozen). */
export const MODE_NOBODY = 0;
export const MODE_SOME = 1;
export const MODE_EVERYONE = 2;
/** The numeric wire values of `GroupsForm` (frozen); `undefined` is off. */
export const FORM_ALL = 1;
export const FORM_CHOSEN = 2;
/** The numeric wire values of `ContactsForm` (frozen). */
export const CONTACTS_FORM_ALL = 1;
export const CONTACTS_FORM_CHOSEN = 2;
/** The numeric wire values of `ScopeKind` (frozen). */
export const SCOPE_GROUP = 1;
export const SCOPE_CHANNEL = 2;

/** The fields of a `VisibilitySetting` this module reads. */
export interface SettingFields {
  readonly mode: number;
  readonly contactsOfContacts: boolean;
  readonly contactsForm: number;
  readonly groups: number | undefined;
}

export type ModeName = "nobody" | "some" | "everyone";
export type FormName = "all" | "chosen";

/** The setting as the screen reads it. */
export interface VisibilityView {
  readonly mode: ModeName;
  readonly contactsOfContacts: boolean;
  /** Part 2.1's form; meaningful only while part 2.1 is on. */
  readonly contactsForm: FormName;
  /** Part 2.2 and its form; `null` when off. */
  readonly groups: FormName | null;
}

export function viewOf(s: SettingFields): VisibilityView {
  const mode: ModeName =
    s.mode === MODE_SOME ? "some" : s.mode === MODE_EVERYONE ? "everyone" : "nobody";
  const groups: FormName | null =
    s.groups === FORM_ALL ? "all" : s.groups === FORM_CHOSEN ? "chosen" : null;
  const contactsForm: FormName = s.contactsForm === CONTACTS_FORM_CHOSEN ? "chosen" : "all";
  return { mode, contactsOfContacts: s.contactsOfContacts, contactsForm, groups };
}

/** Which rows are on screen (UX section 3). */
export interface VisibleRows {
  /** The two parts of Some people, indented under it. */
  readonly parts: boolean;
  /** All / Only the ones I choose, under Members of my groups and channels. */
  readonly groupsForm: boolean;
  /** The disclosure into the list, beside Only the ones I choose. */
  readonly listDisclosure: boolean;
  /** All / Only the ones I choose, under Contacts of my contacts. */
  readonly contactsForm: boolean;
  /** The disclosure into the list of contacts. */
  readonly contactsDisclosure: boolean;
}

export function visibleRows(v: VisibilityView): VisibleRows {
  const parts = v.mode === "some";
  const groupsForm = parts && v.groups !== null;
  const contactsForm = parts && v.contactsOfContacts;
  return {
    parts,
    groupsForm,
    listDisclosure: groupsForm && v.groups === "chosen",
    contactsForm,
    contactsDisclosure: contactsForm && v.contactsForm === "chosen",
  };
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

/**
 * UX section 5.1: `Their contacts can find me` on a contact's screen exists
 * only while Contacts of my contacts is on. The row is absent rather than dead
 * otherwise.
 */
export function showsTheirContactsSwitch(v: VisibilityView): boolean {
  return v.mode === "some" && v.contactsOfContacts;
}

// ------------------------------------------------------------------ copy

/** The screen's copy, rendered in the current language on every call. */
export const COPY = {
  title: (): string => t("who_can_find_me.title"),
  lede: (): string => t("who_can_find_me.lede"),
  ledeLink: (): string => t("who_can_find_me.lede_link"),
  nobody: (): string => t("who_can_find_me.mode.nobody"),
  nobodyHelper: (): string => t("who_can_find_me.mode.nobody_helper"),
  some: (): string => t("who_can_find_me.mode.some"),
  contactsOfContacts: (): string => t("who_can_find_me.contacts_of_contacts"),
  contactsOfContactsHelper: (): string => t("who_can_find_me.contacts_of_contacts_helper"),
  contactsOfContactsSpreading: (): string => t("who_can_find_me.contacts_of_contacts_spreading"),
  contactsAll: (): string => t("who_can_find_me.contacts.all"),
  contactsChosen: (): string => t("who_can_find_me.contacts.chosen"),
  theirContactsCanFindMe: (): string => t("who_can_find_me.their_contacts_can_find_me"),
  groups: (): string => t("who_can_find_me.groups"),
  groupsAllHelper: (): string => t("who_can_find_me.groups_all_helper"),
  all: (): string => t("who_can_find_me.groups.all"),
  chosen: (): string => t("who_can_find_me.groups.chosen"),
  everyone: (): string => t("who_can_find_me.mode.everyone"),
  everyoneHelper: (): string => t("who_can_find_me.mode.everyone_helper"),
  offline: (): string => t("who_can_find_me.status.offline"),
  failed: (): string => t("who_can_find_me.status.failed"),
  retry: (): string => t("common.retry"),
  listEmpty: (): string => t("who_can_find_me.list.empty"),
  listGroups: (): string => t("who_can_find_me.list.groups"),
  listChannels: (): string => t("who_can_find_me.list.channels"),
  listSearch: (): string => t("who_can_find_me.list.search"),
  listNoMatch: (): string => t("who_can_find_me.list.no_match"),
  contactsListEmpty: (): string => t("who_can_find_me.list.contacts_empty"),
  contactsListSearch: (): string => t("who_can_find_me.list.contacts_search"),
  membersCanFindMe: (): string => t("who_can_find_me.members_can_find_me"),
  /** The notice as plain text; the screen renders it with `privacy` as a link. */
  excludedNotice: (): string =>
    t("who_can_find_me.members_excluded_notice", { privacy: t("settings.section.privacy") }),
  signupLine: (): string => t("who_can_find_me.signup_line"),
  signupChange: (): string => t("who_can_find_me.signup_change"),
} as const;

/** The helper under Members of my groups and channels (UX section 3). */
export function groupsHelper(v: Pick<VisibilityView, "groups">, chosen: number, total: number): string {
  if (v.groups === "chosen") return t("who_can_find_me.groups_chosen_helper", { chosen, total });
  return COPY.groupsAllHelper();
}

/**
 * The helper under Contacts of my contacts (UX section 8). Right after it is
 * turned on the widening travels through the contacts' devices, so the
 * screen says so rather than claiming it is already in effect.
 */
export function contactsOfContactsHelper(
  v: Pick<VisibilityView, "contactsOfContacts" | "contactsForm">,
  chosen: number,
  total: number,
  spreading: boolean,
): string {
  if (v.contactsOfContacts && spreading) return COPY.contactsOfContactsSpreading();
  if (v.contactsOfContacts && v.contactsForm === "chosen") {
    return t("who_can_find_me.contacts_chosen_helper", { chosen, total });
  }
  return COPY.contactsOfContactsHelper();
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
      return { spinner: false, message: COPY.offline(), retry: false };
    case "failed":
      return { spinner: false, message: COPY.failed(), retry: true };
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

// ------------------------------------------------------- the contact list

/** One contact of the list of section 4.1. */
export interface ContactEntry {
  /** The 16-byte account id, lower-case hex: what the setting names, so the
   *  choice survives a key rotation. */
  readonly accountIdHex: string;
  readonly keyB64: string;
  readonly name: string;
}

/**
 * The contacts the list shows, sorted as the Contacts screen sorts them. A
 * contact with no known account id cannot be chosen, so it has no row.
 */
export function contactEntries<C extends { readonly signaturePubkeyB64: string; readonly accountIdHex?: string }>(
  contacts: readonly C[],
  nameOf: (contact: C) => string,
): ContactEntry[] {
  const out: ContactEntry[] = [];
  for (const c of contacts) {
    const hex = c.accountIdHex?.toLowerCase();
    if (hex === undefined || !/^[0-9a-f]{32}$/.test(hex)) continue;
    out.push({ accountIdHex: hex, keyB64: c.signaturePubkeyB64, name: nameOf(c) });
  }
  return out.sort((a, b) => compareContactLabels(a.name, b.name) || a.keyB64.localeCompare(b.keyB64));
}

/** The list filtered by the search text (any part of the name). */
export function contactList(entries: readonly ContactEntry[], query: string): readonly ContactEntry[] {
  const q = foldForSearch(query);
  return q.length === 0 ? entries : entries.filter((e) => foldForSearch(e.name).includes(q));
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

/** ADR-0089 step 5: whether Everyone can be chosen. Not while moderators hid
 *  the account from search; the other choices stay as they are. */
export function everyoneOffered(hiddenFromSearch: boolean): boolean {
  return !hiddenFromSearch;
}
