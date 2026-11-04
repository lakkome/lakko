// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { runCaptureGate, type RefusalAnswer } from "./capture-gate";

const refused = Object.assign(new Error("no"), { name: "NotAllowedError" });
const busy = Object.assign(new Error("busy"), { name: "NotReadableError" });

describe("runCaptureGate", () => {
  it("asks nothing when every device is allowed", async () => {
    const asked: string[] = [];
    const got = await runCaptureGate(
      { audio: true, video: true },
      () => Promise.resolve(),
      (d) => {
        asked.push(d);
        return Promise.resolve("without");
      },
    );
    expect(got).toEqual({ microphone: true, camera: true });
    expect(asked).toEqual([]);
  });

  it("probes only what the call wants", async () => {
    const probed: string[] = [];
    const got = await runCaptureGate(
      { audio: true, video: false },
      (d) => {
        probed.push(d);
        return Promise.resolve();
      },
      () => Promise.resolve("without"),
    );
    expect(probed).toEqual(["microphone"]);
    expect(got).toEqual({ microphone: true, camera: false });
  });

  it("tries again until the system allows it", async () => {
    let allowed = false;
    const answers: RefusalAnswer[] = [];
    const got = await runCaptureGate(
      { audio: true, video: false },
      () => (allowed ? Promise.resolve() : Promise.reject(refused)),
      () => {
        answers.push("retry");
        if (answers.length === 2) allowed = true;
        return Promise.resolve("retry");
      },
    );
    expect(answers).toHaveLength(2);
    expect(got.microphone).toBe(true);
  });

  it("goes on without a device the person gave up", async () => {
    const got = await runCaptureGate(
      { audio: true, video: true },
      (d) => (d === "camera" ? Promise.reject(refused) : Promise.resolve()),
      () => Promise.resolve("without"),
    );
    expect(got).toEqual({ microphone: true, camera: false });
  });

  it("leaves a failure that is not a refusal to the capture fallback", async () => {
    const asked: string[] = [];
    const got = await runCaptureGate(
      { audio: true, video: false },
      () => Promise.reject(busy),
      (d) => {
        asked.push(d);
        return Promise.resolve("without");
      },
    );
    expect(asked).toEqual([]);
    expect(got.microphone).toBe(true);
  });
});
