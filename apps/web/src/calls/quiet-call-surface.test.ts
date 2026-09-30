// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two quiet surfaces of `docs/ux/calls.md` section 2.1.1 exist, are not
 * modals, and reach the one accept and the one decline this client has.
 *
 * ## Why a source guard and not a rendering test
 *
 * vitest here runs bare node: no jsdom and no Solid transform, so a component
 * cannot be mounted at all. What a surface DECIDES lives in
 * `quiet-call-offer.ts` and is tested directly. What is left is a small number
 * of facts about the markup itself, and every one of them fails in the
 * dangerous direction if it goes wrong silently:
 *
 *   - the panel copied from {@link IncomingCallScreen} keeping its
 *     `aria-modal`, its focus move, its Tab trap or its Escape-declines would
 *     turn the quiet surface back into an interruption — and a quieter one is
 *     worse than the loud one, because it steals the caret from a person who
 *     is typing and declines a call when they press Escape to close a menu;
 *   - a surface losing its Accept would restore the defect this whole change
 *     exists to remove, and it would look like nothing had happened;
 *   - a surface comparing the ring deadline itself, or restating the window,
 *     is the drift `lakko_ui_rules::ring` exists to stop, arriving in a
 *     component where no test can see it.
 *
 * ## Comments are stripped; strings are not
 *
 * The opposite of `ui/no-window-confirm.test.ts`, deliberately. That guard
 * forbids a CALL, so it strips strings to tell code from prose. This one
 * inspects JSX attribute VALUES — `role="dialog"` versus `role="status"` is
 * the whole question — so the strings are the code, and stripping them would
 * reduce both to `role=`. Comments still go, so this file's own accurate
 * sentences about what the panel must not be cannot trip it.
 */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

/** `src/`, resolved from this file rather than from the process cwd. */
const SRC_ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

/** Source with comments removed. See the header for why strings stay. */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/(^|[^:])\/\/[^\n]*/g, "$1 ");
}

function code(relativePath: string): string {
  return stripComments(readFileSync(join(SRC_ROOT, relativePath), "utf8"));
}

const PANEL = "calls/QuietIncomingCallPanel.tsx";
const CHAT = "routes/Chat.tsx";
const LIST = "chat/ChatList.tsx";
const HOST = "calls/CallHostContext.tsx";

/** The files that must never restate the ring window. */
const WINDOW_FREE = [PANEL, "calls/quiet-call-offer.ts", "calls/ring-rule.ts"];

describe("the muted conversation's panel is not a modal", () => {
  const panel = code(PANEL);

  it("reads the file it thinks it is reading", () => {
    // A guard that scans an empty string passes forever.
    expect(panel).toContain("QuietIncomingCallPanel");
  });

  it("is not a dialog and does not seal the page behind it", () => {
    // Section 2.1.1: it "sits in the layout rather than over it, so nothing
    // the person was reading moves or is covered".
    expect(panel).not.toContain("aria-modal");
    expect(panel).not.toContain('role="dialog"');
  });

  it("never moves focus", () => {
    // The full screen focuses Accept on mount, which is right for a call that
    // may interrupt. Here it would take the caret out of whatever the person
    // is typing, silently.
    expect(panel).not.toMatch(/\.focus\s*\(/);
  });

  it("binds no keys at all, so Escape still belongs to the page", () => {
    // On the full screen Escape declines, because dismissing a modal and
    // refusing the call are the same act. In a chat, Escape closes a menu or
    // leaves a search field; declining a call by reflex is not this surface's
    // to do. No trap either — there is nothing to trap focus inside of.
    expect(panel).not.toContain("keydown");
    expect(panel).not.toContain("onKeyDown");
    expect(panel).not.toContain("addEventListener");
  });

  it("announces itself, since nothing else does", () => {
    // No sound, no vibration, no focus move: a live region is the only thing
    // that tells a screen-reader user a call is being offered.
    expect(panel).toContain('role="status"');
  });

  it("holds no call state of its own", () => {
    // It takes a caller, a kind and two callbacks. A panel that reached into
    // the call host would be a second place deciding whether to show itself.
    expect(panel).not.toContain("useCallHost");
  });
});

describe("what each quiet surface offers", () => {
  it("offers Accept and Decline on the panel", () => {
    const panel = code(PANEL);
    expect(panel).toContain("Accept");
    expect(panel).toContain("Decline");
  });

  it("offers the third action on the panel, where the full screen would have", () => {
    // Section 2.2 gives a video call three actions, and the full screen — the
    // only other place that carries them — is the surface mute withholds. So
    // without this, a muted video call could be answered in no way except
    // with the camera on.
    expect(code(PANEL)).toContain("Accept without video");
  });

  it("offers Accept and Decline on the conversation row", () => {
    // ADR-0058: the row's words come from the catalog, so the guard looks for
    // the two catalog keys and the two host calls they drive.
    const list = code(LIST);
    expect(list).toContain('t("call.incoming.accept")');
    expect(list).toContain('t("call.incoming.decline")');
    expect(list).toContain("acceptIncomingCall");
    expect(list).toContain("declineIncomingCall");
  });
});

describe("there is one accept path and one decline path", () => {
  it("routes both quiet surfaces through the call host's pair", () => {
    for (const file of [CHAT, LIST]) {
      const src = code(file);
      expect(src, file).toContain("acceptIncomingCall");
      expect(src, file).toContain("declineIncomingCall");
    }
  });

  it("routes the full-screen incoming call through the same pair", () => {
    // It used to reach a second, unguarded copy on the host's internals. Two
    // pairs is how the surfaces come to differ on what accepting means.
    const host = code(HOST);
    expect(host).toContain("host.acceptIncomingCall");
    expect(host).toContain("host.declineIncomingCall");
    expect(host).not.toContain("internals.accept");
    expect(host).not.toContain("internals.decline");
  });

  it("lets no surface reach a call session directly", () => {
    for (const file of [PANEL, CHAT, LIST]) {
      expect(code(file), file).not.toMatch(/\bsession\s*\.\s*(accept|decline)\s*\(/);
    }
  });
});

describe("the ring window stays in one place", () => {
  it("is asked, not re-derived, by both surfaces", () => {
    // Whether the call is this conversation's, and whether it is still inside
    // the window, are one module's answer (`quiet-call-offer.ts`).
    for (const file of [CHAT, LIST]) {
      expect(code(file), file).toContain("quietCallOfferFor");
    }
  });

  it("is compared in no surface", () => {
    // A surface that read the deadline itself would be a second copy of the
    // boundary, in the one place a test cannot reach.
    for (const file of [PANEL, CHAT, LIST]) {
      expect(code(file), file).not.toContain("expiresAtMs");
    }
  });

  it("is never written out as a number", () => {
    // 45 seconds is `lakko_ui_rules::ring::CALL_RING_WINDOW_MS` and nothing
    // else. It had been copied out four times before the shared rule existed.
    for (const file of WINDOW_FREE) {
      expect(code(file), file).not.toMatch(/\b45_?000\b/);
    }
  });
});

describe("the guard reads attributes rather than prose", () => {
  it("strips a comment that mentions what the panel must not be", () => {
    // Without this the guard would force the deletion of an accurate sentence,
    // which is how a guard gets deleted instead.
    expect(stripComments('/* never aria-modal */ const a = 1;')).not.toContain("aria-modal");
    expect(stripComments('// no role="dialog" here\nconst a = 1;')).not.toContain("dialog");
  });

  it("keeps an attribute value, which is the thing being judged", () => {
    expect(stripComments('<div role="dialog" aria-modal="true" />')).toContain('role="dialog"');
    expect(stripComments('<div role="dialog" aria-modal="true" />')).toContain("aria-modal");
  });
});
