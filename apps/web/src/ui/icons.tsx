// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The icon set: inline stroke SVGs on a 24px grid, recoloured through
 * `currentColor`. One consistent style everywhere; no emoji in product UI.
 * Names line up with `.claude/skills/ux-ui/design-system.md`.
 */
import type { JSX } from "solid-js";

const PATHS = {
  chat: `<path d="M4 6a2.5 2.5 0 0 1 2.5-2.5h11A2.5 2.5 0 0 1 20 6v8a2.5 2.5 0 0 1-2.5 2.5H9.2L4.8 20v-3.6A2.5 2.5 0 0 1 4 14z"/>`,
  contacts: `<circle cx="9" cy="8" r="3.2"/><path d="M3.5 19a5.5 5.5 0 0 1 11 0"/><circle cx="17" cy="9" r="2.4"/><path d="M15.6 14.3a4.4 4.4 0 0 1 4.9 4.2"/>`,
  channels: `<path d="M4 10v4a1 1 0 0 0 1 1h3l6 4V5L8 9H5a1 1 0 0 0-1 1z"/><path d="M17 9.5a3.5 3.5 0 0 1 0 5"/>`,
  calls: `<path d="M6.6 3.5h2.9l1.5 4-2 1.5a10 10 0 0 0 6 6l1.5-2 4 1.5v2.9a2 2 0 0 1-2 2A15.5 15.5 0 0 1 4.6 5.5a2 2 0 0 1 2-2z"/>`,
  groups: `<circle cx="12" cy="7.5" r="3"/><path d="M6.5 19a5.5 5.5 0 0 1 11 0"/><circle cx="5" cy="9.5" r="2.2"/><path d="M1.5 17.5a4 4 0 0 1 4.2-3.9"/><circle cx="19" cy="9.5" r="2.2"/><path d="M22.5 17.5a4 4 0 0 0-4.2-3.9"/>`,
  settings: `<circle cx="12" cy="12" r="3.3"/><path d="M12 2.8v2.4M12 18.8v2.4M2.8 12h2.4M18.8 12h2.4M5.5 5.5l1.7 1.7M16.8 16.8l1.7 1.7M5.5 18.5l1.7-1.7M16.8 7.2l1.7-1.7"/>`,
  search: `<circle cx="11" cy="11" r="6"/><path d="M15.5 15.5 20 20"/>`,
  compose: `<path d="M4 20h4l10.5-10.5a2.1 2.1 0 0 0-3-3L5 17z"/><path d="M13.5 8.5l3 3"/>`,
  plus: `<path d="M12 5v14M5 12h14"/>`,
  phone: `<path d="M6.6 3.5h2.9l1.5 4-2 1.5a10 10 0 0 0 6 6l1.5-2 4 1.5v2.9a2 2 0 0 1-2 2A15.5 15.5 0 0 1 4.6 5.5a2 2 0 0 1 2-2z"/>`,
  video: `<rect x="3" y="6.5" width="12.5" height="11" rx="2"/><path d="M15.5 10.5 21 7.5v9l-5.5-3"/>`,
  more: `<circle cx="5.5" cy="12" r="1.4"/><circle cx="12" cy="12" r="1.4"/><circle cx="18.5" cy="12" r="1.4"/>`,
  clip: `<path d="M20 11.5 12 19.5a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7L9.7 17.2a1.6 1.6 0 0 1-2.3-2.3L15 7.3"/>`,
  mic: `<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M6 11a6 6 0 0 0 12 0M12 17v4M9 21h6"/>`,
  send: `<path d="M4 12 20 4l-4 16-4.5-6.5z"/><path d="M11.5 13.5 20 4"/>`,
  check: `<path d="M5 12.5l4.5 4.5L19 7.5"/>`,
  // The `Select` row of the message menu (docs/ux/message-actions.md section
  // 8): the same empty-circle-with-a-check the row's selection mark draws.
  select: `<circle cx="12" cy="12" r="8.5"/><path d="M8 12.3l2.7 2.7L16.3 9.5"/>`,
  dcheck: `<path d="M2.5 12.5 6.5 16.5 14.5 8.5"/><path d="M9.5 15 11 16.5 21 6.5"/>`,
  key: `<circle cx="8" cy="14" r="4.5"/><path d="M11.5 10.5 20 2.5M17 5.5l2.5 2.5M14.5 8l2.5 2.5"/>`,
  devices: `<rect x="3" y="5" width="13" height="10" rx="1.5"/><path d="M7 19h5"/><rect x="17" y="9" width="4.5" height="10" rx="1.2"/>`,
  shield: `<path d="M12 3 5 6v5.5c0 4.3 3 7.6 7 8.5 4-.9 7-4.2 7-8.5V6z"/>`,
  user: `<circle cx="12" cy="8.5" r="3.6"/><path d="M5 20a7 7 0 0 1 14 0"/>`,
  bell: `<path d="M6.5 17V11a5.5 5.5 0 0 1 11 0v6l1.5 2h-14z"/><path d="M10 21h4"/>`,
  bellOff: `<path d="M6.5 17V11a5.5 5.5 0 0 1 11 0v6l1.5 2h-14z"/><path d="M10 21h4M4 4l16 16"/>`,
  palette: `<path d="M12 3a9 9 0 0 0 0 18c1.4 0 2-.9 2-1.9 0-.9-.6-1.3-.6-2 0-1 .8-1.6 1.8-1.6h1.6A4.2 4.2 0 0 0 21 11.3C21 6.7 17 3 12 3z"/><circle cx="7.5" cy="11" r="1.2"/><circle cx="10.5" cy="7" r="1.2"/><circle cx="15.5" cy="7" r="1.2"/>`,
  db: `<ellipse cx="12" cy="6" rx="7" ry="2.8"/><path d="M5 6v12c0 1.5 3.1 2.8 7 2.8s7-1.3 7-2.8V6M5 12c0 1.5 3.1 2.8 7 2.8s7-1.3 7-2.8"/>`,
  info: `<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5M12 8v.5"/>`,
  chevron: `<path d="m9 6 6 6-6 6"/>`,
  back: `<path d="m15 6-6 6 6 6"/>`,
  logout: `<path d="M10 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h4M15 8l4 4-4 4M19 12H9"/>`,
  eye: `<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>`,
  eyeOff: `<path d="M3 3l18 18M10.6 5.9A9.8 9.8 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a17 17 0 0 1-3.2 3.9M6.6 6.6C4 8.5 2.5 12 2.5 12s3.5 6.5 9.5 6.5c1.5 0 2.8-.4 4-1"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>`,
  lock: `<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8 10.5V8a4 4 0 0 1 8 0v2.5"/>`,
  pin: `<path d="M9 3.5h6l-1 6 3 3H7l3-3zM12 12.5V21"/>`,
  archive: `<rect x="3" y="4" width="18" height="4" rx="1"/><path d="M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8M10 12h4"/>`,
  link: `<path d="M10 14a3.5 3.5 0 0 0 5 0l3-3a3.5 3.5 0 0 0-5-5l-1 1"/><path d="M14 10a3.5 3.5 0 0 0-5 0l-3 3a3.5 3.5 0 0 0 5 5l1-1"/>`,
  copy: `<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>`,
  // ADR-0052 — the Reply row of the message menu: an arrow curving back.
  reply: `<path d="m9 7-5 5 5 5"/><path d="M4 12h9.5a6 6 0 0 1 6 6v1"/>`,
  // ADR-0052 decision 2 — the Forward row of the message menu and the line
  // above a forwarded bubble: the reply arrow, curving the other way.
  forward: `<path d="m15 7 5 5-5 5"/><path d="M20 12h-9.5a6 6 0 0 0-6 6v1"/>`,
  close: `<path d="M6 6l12 12M18 6 6 18"/>`,
  incoming: `<path d="M19 5 9 15M9 8v7h7"/>`,
  outgoing: `<path d="M5 19 15 9M15 16V9H8"/>`,
  missed: `<path d="M19 5 9 15M9 8v7h7"/><path d="M4 4l3 3"/>`,
  trash: `<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>`,
  pencil: `<path d="M4 20h4l10.5-10.5a2.1 2.1 0 0 0-3-3L5 17z"/><path d="M13.5 8.5l3 3"/>`,
  share: `<circle cx="18" cy="5.5" r="2.5"/><circle cx="6" cy="12" r="2.5"/><circle cx="18" cy="18.5" r="2.5"/><path d="M8.2 10.8 15.8 6.7M8.2 13.2l7.6 4.1"/>`,
  qr: `<rect x="4" y="4" width="6" height="6" rx="1"/><rect x="14" y="4" width="6" height="6" rx="1"/><rect x="4" y="14" width="6" height="6" rx="1"/><path d="M14 14h2v2h-2zM18 14h2v2h-2zM14 18h2v2h-2zM18 18h2v2h-2z"/>`,
  sun: `<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4"/>`,
  moon: `<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>`,
  globe: `<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.3 2.4 3.5 5.2 3.5 8.5s-1.2 6.1-3.5 8.5c-2.3-2.4-3.5-5.2-3.5-8.5S9.7 5.9 12 3.5z"/>`,
  monitor: `<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>`,
  cameraFlip: `<path d="M3 9a2 2 0 0 1 2-2h2.2l1.3-2h7l1.3 2H19a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M8.5 13.5a3.5 3.5 0 0 1 6.2-2.2M15.5 12.5a3.5 3.5 0 0 1-6.2 2.2"/><path d="M14.7 8.8v2.5h-2.5M9.3 17.2v-2.5h2.5"/>`,
  smile: `<circle cx="12" cy="12" r="8.5"/><path d="M8.5 14a4.5 4.5 0 0 0 7 0M9.2 9.8h.01M14.8 9.8h.01"/>`,
  micOff: `<path d="M9 9.5V11a3 3 0 0 0 5.1 2.1M15 10.6V6a3 3 0 0 0-5.7-1.3"/><path d="M6 11a6 6 0 0 0 9.6 4.8M18 11a6 6 0 0 1-.3 1.9M12 17v4M9 21h6M4 4l16 16"/>`,
  videoOff: `<path d="M15.5 10.5 21 7.5v9l-5.5-3"/><path d="M13.5 6.5H5a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h9a2 2 0 0 0 1.5-.7"/><path d="M3 3l18 18"/>`,
  hand: `<path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V11M11 10.5V4.5a1.5 1.5 0 0 1 3 0V11M14 11V6a1.5 1.5 0 0 1 3 0v7.5"/><path d="M17 13.5V9.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-2a6 6 0 0 1-4.8-2.4L4 14.6a1.5 1.5 0 0 1 2.4-1.8L8 14.6"/>`,
  mail: `<rect x="3" y="5.5" width="18" height="13" rx="2"/><path d="m3.5 7.5 8.5 6 8.5-6"/>`,
  lines: `<path d="M5 7h14M5 12h10M5 17h7"/>`,
  folder: `<path d="M3.5 7a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>`,
  grip: `<circle cx="9" cy="6.5" r="1.3"/><circle cx="15" cy="6.5" r="1.3"/><circle cx="9" cy="12" r="1.3"/><circle cx="15" cy="12" r="1.3"/><circle cx="9" cy="17.5" r="1.3"/><circle cx="15" cy="17.5" r="1.3"/>`,
  phoneEnd: `<path d="M3.5 13.5c4.7-4.7 12.3-4.7 17 0l-1.6 2.2a1.6 1.6 0 0 1-2 .5l-2.4-1.2a1.6 1.6 0 0 1-.9-1.6v-1.4a11 11 0 0 0-3.2 0v1.4a1.6 1.6 0 0 1-.9 1.6l-2.4 1.2a1.6 1.6 0 0 1-2-.5z"/>`,
  // The in-call Speaker control (docs/ux/calls.md section 4.1). `channels`
  // draws the same idea for a subscriber list; a call control needs its own
  // name so a change to one never silently restyles the other.
  speaker: `<path d="M4 9.5v5a1 1 0 0 0 1 1h2.8l4.7 3.5v-14L7.8 8.5H5a1 1 0 0 0-1 1z"/><path d="M16 9.4a3.6 3.6 0 0 1 0 5.2M18.6 6.8a7.2 7.2 0 0 1 0 10.4"/>`,
} as const;

/** Every icon name the set knows. */
export type IconName = keyof typeof PATHS;

/** One inline SVG icon; `size` in px (default 20), colour from `currentColor`. */
export function Icon(props: {
  name: IconName;
  size?: number;
  strokeWidth?: number;
  class?: string;
  title?: string;
}): JSX.Element {
  const size = (): number => props.size ?? 20;
  return (
    <svg
      width={size()}
      height={size()}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width={props.strokeWidth ?? 1.8}
      stroke-linecap="round"
      stroke-linejoin="round"
      class={props.name === "back" ? `lk-icon-flip-rtl ${props.class ?? ""}`.trim() : props.class}
      aria-hidden={props.title === undefined ? "true" : undefined}
      role={props.title === undefined ? undefined : "img"}
      style={{ "flex-shrink": 0, display: "block" }}
      // The paths are static strings from this module, never user input.
      // eslint-disable-next-line solid/no-innerhtml
      innerHTML={(props.title === undefined ? "" : `<title>${props.title}</title>`) + PATHS[props.name]}
    />
  );
}
