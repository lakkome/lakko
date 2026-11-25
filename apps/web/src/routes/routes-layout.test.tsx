// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * Every conversation opens under ONE list layout, so moving between a chat, a
 * group and a channel never unmounts the list (seen on the desktop app,
 * 2026-11-25: a group under a layout of its own blanked the whole list on
 * every move between a chat and a group).
 */
import { beforeAll, describe, expect, it } from "vitest";
import type { RouteDefinition } from "@solidjs/router";

import { initWasm } from "../test/ui";

let routes: RouteDefinition[];

// Importing the route table loads every screen of the app; on a cold disk
// that alone took about 28 s, so the default 30 s hook limit is too tight.
beforeAll(async () => {
  await initWasm();
  ({ routes } = await import("./routes"));
}, 120_000);

const childPaths = (r: RouteDefinition): string[] => {
  const children = r.children;
  if (children === undefined) return [];
  return (Array.isArray(children) ? children : [children]).flatMap((c) =>
    Array.isArray(c.path) ? c.path : [c.path ?? ""],
  );
};

describe("the conversation routes", () => {
  it("open a chat, a group and a channel under the same layout", () => {
    const shell = routes.find((r) => r.path === "/" && r.children !== undefined);
    expect(shell).toBeDefined();
    const shellChildren = (shell as RouteDefinition).children as RouteDefinition[];
    const holders = shellChildren.filter((r) => {
      const paths = childPaths(r);
      return paths.includes("/chat/:id") || paths.includes("/groups/:id") || paths.includes("/channel/:id");
    });
    expect(holders).toHaveLength(1);
    const paths = childPaths(holders[0] as RouteDefinition);
    expect(paths).toEqual(expect.arrayContaining(["/chat/:id", "/groups/:id", "/groups/new", "/groups"]));
  });
});
