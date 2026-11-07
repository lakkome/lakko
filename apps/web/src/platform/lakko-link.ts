// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Lakko links outside the browser (ADR-0085 section 5, `docs/ux/desktop.md`
 * section 7).
 *
 * The `https` link is `/<kind>/<ledger id>#<secret>`, and the `lakko://` form
 * carries the same parts. Where the OS hands the `https` link to the app itself
 * the page has nothing to do; everywhere else the page opens the `lakko://`
 * form.
 */

export type LinkKind = "i" | "c" | "g";

export interface LakkoLink {
  readonly kind: LinkKind;
  readonly id: string;
  readonly secret: string;
}

const ID_CHARS = 8;
const SECRET_CHARS = 16;
const LINK_PATH = new RegExp(`^/([icg])/([A-Za-z0-9_-]{${ID_CHARS}})$`);
const SECRET = new RegExp(`^[A-Za-z0-9_-]{${SECRET_CHARS}}$`);

/** A whole Lakko link at `origin`, or `null`: anything else is never opened
 *  inside Lakko. */
export function parseLakkoLink(text: string, origin: string): LakkoLink | null {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return null;
  }
  if (url.origin !== origin) return null;
  const path = LINK_PATH.exec(url.pathname);
  const secret = url.hash.slice(1);
  if (path === null || !SECRET.test(secret) || url.search !== "") return null;
  return { kind: path[1] as LinkKind, id: path[2] as string, secret };
}

/** The route of a link inside the app: the screen the web shows for it. */
export function appPathOf(link: LakkoLink): string {
  return `/${link.kind}/${link.id}#${link.secret}`;
}

/** The address "Open in the app" opens on a page showing `link`. */
export function openInAppUrl(link: LakkoLink): string {
  return `lakko://${link.kind}/${link.id}#${link.secret}`;
}

/**
 * Whether the page tries the app by itself, once, when it opens. Only on
 * macOS, and only in the browsers measured to stay quiet with no app installed:
 * Safari answers with its own "address is invalid" message, and nothing was
 * measured on Windows or Linux, so there, as in any browser not named here, the
 * button is the way in.
 */
export function opensAppByItself(userAgent: string, mac: boolean): boolean {
  return mac && /\b(?:Chrome|Firefox)\//.test(userAgent);
}

/** Whether this browser could have the desktop app beside it. */
export function mayHaveDesktopApp(userAgent: string, touchPoints: number): boolean {
  if (/Android|iPhone|iPad|iPod|Mobile/.test(userAgent)) return false;
  // An iPad reports itself as a Mac, with a touch screen no Mac has.
  return !(/Macintosh/.test(userAgent) && touchPoints > 1);
}

/**
 * A link the phone app opens, read off the page a phone browser shows
 * (`docs/ux/desktop.md` section 7.1): the three ledger links, and a shared
 * feed (`/feed/import#<rule>`, ADR-0063). `path` is the route, `fragment` what
 * rides after `#`; neither is ever sent to a server by this module.
 */
export interface PhoneAppLink {
  readonly path: string;
  readonly fragment: string;
}

const FEED_IMPORT_PATH = "/feed/import";
/** A shared rule is at most 4096 bytes (`lakko-feed`), 5462 base64url
 *  characters; anything longer is not a feed and is not offered. */
const FEED_FRAGMENT = /^[A-Za-z0-9_-]{1,5462}$/;

/** The link `pathname` + `hash` show, if a phone app opens it, else `null`. */
export function phoneAppLinkOf(pathname: string, hash: string, origin: string): PhoneAppLink | null {
  const ledger = parseLakkoLink(`${origin}${pathname}${hash}`, origin);
  if (ledger !== null) return { path: `/${ledger.kind}/${ledger.id}`, fragment: ledger.secret };
  const fragment = hash.startsWith("#") ? hash.slice(1) : "";
  if (pathname.replace(/\/$/, "") === FEED_IMPORT_PATH && FEED_FRAGMENT.test(fragment)) {
    return { path: FEED_IMPORT_PATH, fragment };
  }
  return null;
}

/** The phone the page runs on, when it is one an app of ours runs on. */
export type PhoneAppPlatform = "android" | "ios";

/** Which phone app this browser could have beside it, or `null` on a computer.
 *  An iPad reports itself as a Mac, with a touch screen no Mac has. */
export function phoneAppPlatform(userAgent: string, touchPoints: number): PhoneAppPlatform | null {
  if (/Android/.test(userAgent)) return "android";
  if (/iPhone|iPad|iPod/.test(userAgent)) return "ios";
  if (/Macintosh/.test(userAgent) && touchPoints > 1) return "ios";
  return null;
}

/** The Android package the page opens when the build names none: the one
 *  package every Android build has today (`apps/android/app/build.gradle.kts`
 *  `applicationId`), for the test stack and for `lakko.me` alike. */
export const DEFAULT_ANDROID_PACKAGE = "me.lakko.dev";

/** The package `VITE_LAKKO_ANDROID_PACKAGE` names, the default when unset, or
 *  `null` when it is not a package name (then Android is offered nothing). */
export function androidPackageFromEnv(value: string | undefined): string | null {
  const name = value === undefined || value.trim() === "" ? DEFAULT_ANDROID_PACKAGE : value.trim();
  return /^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)+$/.test(name) ? name : null;
}

/**
 * The `intent:` address that opens `link` in the Android app `pkg`. Chrome
 * does not hand a link to an app when the person opened it inside Chrome, so
 * the page names the app. The data is the `https` link itself, the one the app
 * claims; with no app installed the browser goes to `browser_fallback_url`,
 * the same page. `#` inside an extra would end the data early, so the
 * fallback is percent-encoded.
 */
export function androidIntentUrl(link: PhoneAppLink, origin: string, pkg: string): string {
  const url = new URL(origin);
  const scheme = url.protocol.replace(/:$/, "");
  const https = `${url.origin}${link.path}#${link.fragment}`;
  return (
    `intent://${url.host}${link.path}#${link.fragment}` +
    `#Intent;scheme=${scheme};package=${pkg};` +
    `S.browser_fallback_url=${encodeURIComponent(https)};end`
  );
}

/** The `lakko:` address that opens `link` in the iOS app: Safari never hands a
 *  link to an app while the person stays on the same site, so the page uses
 *  the app's own scheme, the same form the desktop app takes. */
export function iosAppUrl(link: PhoneAppLink): string {
  return `lakko:/${link.path}#${link.fragment}`;
}
