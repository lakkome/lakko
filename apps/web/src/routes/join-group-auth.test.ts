// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A signed-out visitor who opens a `/g/<slug>` group invite must be able to
 * create an account from the page and come back to the invite afterwards.
 *
 * Found live: the page offered a bare "Log in" link to `/login` with no
 * `?return=`, and no sign-up at all, so someone without a Lakko account had
 * nowhere to go, and someone with one landed on Home and lost the invite. The
 * `/i/` contact invite already solved this with the `?return=` round-trip.
 */
import { describe, expect, it } from "vitest";
import { joinGroupAuthLinks } from "./join-group-auth";

describe("joinGroupAuthLinks", () => {
  const slug = "AQAgTcBaot-tzbAeAL9CbxJR-tSZ_VStkq";

  it("offers sign-up that returns to the invite", () => {
    const { signup } = joinGroupAuthLinks(slug);
    const url = new URL(signup, "http://lakko.test");
    expect(url.pathname).toBe("/signup");
    expect(url.searchParams.get("return")).toBe(`/g/${slug}`);
  });

  it("keeps the link key fragment in the return path, so the preview survives the round trip", () => {
    const hash = "#XFxcXFxcXFxcXFxcXFxcXFxcXFxcXFxcXFxcXFxcXFw";
    const { signup, login } = joinGroupAuthLinks(slug, hash);
    for (const href of [signup, login]) {
      const url = new URL(href, "http://lakko.test");
      expect(url.hash).toBe("");
      expect(url.searchParams.get("return")).toBe(`/g/${slug}${hash}`);
    }
  });

  it("accepts a fragment without its leading # and leaves an empty one out", () => {
    expect(
      new URL(joinGroupAuthLinks(slug, "abc").login, "http://lakko.test").searchParams.get("return"),
    ).toBe(`/g/${slug}#abc`);
    expect(
      new URL(joinGroupAuthLinks(slug, "").login, "http://lakko.test").searchParams.get("return"),
    ).toBe(`/g/${slug}`);
  });

  it("offers log-in that returns to the invite", () => {
    const { login } = joinGroupAuthLinks(slug);
    const url = new URL(login, "http://lakko.test");
    expect(url.pathname).toBe("/login");
    expect(url.searchParams.get("return")).toBe(`/g/${slug}`);
  });
});
