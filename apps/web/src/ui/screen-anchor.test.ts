// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { screenAnchor } from "./screen-anchor";

describe("screenAnchor", () => {
  it("answers true while the person stays on the path the request started from", () => {
    const location = { pathname: "/chat/abc" };
    const here = screenAnchor(location);
    expect(here()).toBe(true);
  });

  it("answers false once the person has gone somewhere else", () => {
    const location = { pathname: "/chat/abc" };
    const here = screenAnchor(location);
    location.pathname = "/contacts";
    expect(here()).toBe(false);
  });

  it("answers true again when the person comes back to the same path", () => {
    // A request started on a screen the person left and returned to is
    // addressed to what is on display, so its follow-up may run.
    const location = { pathname: "/groups/new" };
    const here = screenAnchor(location);
    location.pathname = "/groups";
    expect(here()).toBe(false);
    location.pathname = "/groups/new";
    expect(here()).toBe(true);
  });

  it("anchors at call time, not at the first read", () => {
    const location = { pathname: "/channels" };
    const here = screenAnchor(location);
    location.pathname = "/channels/new";
    // The first read happens only now; the anchor is still `/channels`.
    expect(here()).toBe(false);
  });

  it("keeps two requests from two screens apart", () => {
    const location = { pathname: "/chat/a" };
    const first = screenAnchor(location);
    location.pathname = "/chat/b";
    const second = screenAnchor(location);
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });
});
