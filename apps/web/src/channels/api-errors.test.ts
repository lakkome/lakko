// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { CHANNELS_UNREACHABLE, channelErrorText, createChannel } from "./api";

function withFetch(resp: Response, run: () => Promise<unknown>): Promise<unknown> {
  const real = globalThis.fetch;
  globalThis.fetch = (() => Promise.resolve(resp)) as typeof fetch;
  return run().finally(() => {
    globalThis.fetch = real;
  });
}

describe("channels API error surface", () => {
  it("reports a non-JSON reply as the service being unreachable, even on a 200", async () => {
    const html = new Response("<!doctype html><title>Lakko</title>", {
      status: 200,
      headers: { "content-type": "text/html" },
    });
    await expect(withFetch(html, () => createChannel("00".repeat(16), "/api/channels"))).rejects.toMatchObject(
      { code: CHANNELS_UNREACHABLE, status: 200 },
    );
  });

  it("reports a bodyless 502 the same way", async () => {
    const gateway = new Response("", { status: 502 });
    await expect(withFetch(gateway, () => createChannel("00".repeat(16), "/api/channels"))).rejects.toMatchObject(
      { code: CHANNELS_UNREACHABLE, status: 502 },
    );
  });

  it("keeps the server's own envelope when there is one", async () => {
    const refused = new Response(JSON.stringify({ code: "unknown-channel", detail: "no such channel" }), {
      status: 404,
      headers: { "content-type": "application/json" },
    });
    await expect(withFetch(refused, () => createChannel("00".repeat(16), "/api/channels"))).rejects.toMatchObject(
      { code: "unknown-channel", detail: "no such channel" },
    );
  });

  it("phrases the error for a person, never as 'unknown —'", () => {
    expect(channelErrorText({ status: 200, code: CHANNELS_UNREACHABLE, detail: "" })).toMatch(/not reachable/);
    expect(channelErrorText({ status: 404, code: "unknown-channel", detail: "no such channel" })).toBe(
      "This channel does not exist on the server.",
    );
    // The server's detail prose never reaches the screen.
    expect(channelErrorText({ status: 500, code: "internal", detail: "db down" })).toBe(
      "The server refused the request. Try again later.",
    );
  });
});
