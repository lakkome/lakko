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
