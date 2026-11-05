// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `coc-discovery-findings.md` item 1: a failure building this device's own
 * introduction record no longer stops the relay of the contacts' records.
 */
import { describe, expect, it } from "vitest";

import { PublicationStepsFailed, runPublicationSteps } from "./publication-steps";

describe("runPublicationSteps", () => {
  it("the relay runs after the own record failed, and the pass still reports the failure", async () => {
    const ran: string[] = [];
    const logged: string[] = [];
    const pass = runPublicationSteps(
      [
        { name: "everyone", run: async () => void ran.push("everyone") },
        {
          name: "own-record",
          run: async () => {
            ran.push("own-record");
            throw new Error("429 on the OPRF batch");
          },
        },
        { name: "offer", run: async () => void ran.push("offer") },
        { name: "relay", run: async () => void ran.push("relay") },
      ],
      (name) => logged.push(name),
    );
    await expect(pass).rejects.toBeInstanceOf(PublicationStepsFailed);
    expect(ran).toEqual(["everyone", "own-record", "offer", "relay"]);
    expect(logged).toEqual(["own-record"]);
  });

  it("every step failing still runs every step", async () => {
    const ran: string[] = [];
    const fail = (name: string) => ({
      name,
      run: async () => {
        ran.push(name);
        throw new Error(name);
      },
    });
    await expect(runPublicationSteps([fail("a"), fail("b")])).rejects.toMatchObject({ failed: ["a", "b"] });
    expect(ran).toEqual(["a", "b"]);
  });

  it("nothing failing resolves", async () => {
    await expect(runPublicationSteps([{ name: "a", run: async () => undefined }])).resolves.toBeUndefined();
  });

  it("the runtime wraps the four steps, the relay last", async () => {
    const { readFileSync } = await import("node:fs");
    const { resolve } = await import("node:path");
    const src = readFileSync(resolve(process.cwd(), "src/reach/ScopedDiscoveryRuntime.tsx"), "utf8");
    const order = ["everyone", "own-record", "offer", "relay"].map((n) => src.indexOf(`name: "${n}"`));
    expect(order.every((i) => i > 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });
});
