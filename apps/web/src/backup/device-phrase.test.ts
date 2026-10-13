// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "A device that holds the account's keys also holds the recovery phrase."
 *
 * `AuthContext` is a Solid component and cannot be mounted here (vitest runs
 * in plain node without the Solid transform), so the rule is pinned on its
 * source: every landing path named in `PHRASE_LANDING_PATHS` has exactly one
 * `storeDevicePhrase(..., "<path>")` call, no call names an unlisted path,
 * and no code writes the phrase store behind the helper's back.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { PHRASE_LANDING_PATHS } from "./device-phrase";

const SRC = fileURLToPath(new URL("..", import.meta.url));
const authContext = readFileSync(join(SRC, "auth/AuthContext.tsx"), "utf8");

/** The path label of every `storeDevicePhrase(` call in `source`. */
function storedPaths(source: string): string[] {
  const out: string[] = [];
  const call = /storeDevicePhrase\(([^;]*?)\);/gs;
  for (const m of source.matchAll(call)) {
    const label = /"([a-z-]+)"\s*,?\s*$/s.exec((m[1] ?? "").trim());
    if (label?.[1] !== undefined) out.push(label[1]);
    else out.push(`<unlabelled: ${(m[1] ?? "").slice(0, 40)}>`);
  }
  return out;
}

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      if (name === "catalog" || name === "node_modules") continue;
      out.push(...sourceFiles(full));
    } else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) {
      out.push(full);
    }
  }
  return out;
}

describe("every path that lands a device with keys stores the phrase", () => {
  it("names each landing path once in AuthContext", () => {
    expect(storedPaths(authContext).sort()).toEqual([...PHRASE_LANDING_PATHS].sort());
  });

  it("covers every way keys reach a device", () => {
    expect([...PHRASE_LANDING_PATHS]).toEqual([
      "signup",
      "signup-guest",
      "recover",
      "provide-phrase",
      "device-pairing",
      "device-restore",
      "fresh-identity",
      "key-rotation",
    ]);
  });

  it("stores the phrase only after the identity check on the paths that have one", () => {
    // Typed phrase, another device (signed in) and another device (no
    // password): the fail-closed check precedes the write in source order.
    for (const [method, path] of [
      ["async provideRecoveryPhrase(", "provide-phrase"],
      ["async completeFromPairing(", "device-pairing"],
    ] as const) {
      const start = authContext.indexOf(method);
      expect(start).toBeGreaterThan(0);
      const verify = authContext.indexOf("await verifySeedAgainstPublishedIdentity(", start);
      const store = authContext.indexOf(`"${path}"`, start);
      expect(verify).toBeGreaterThan(start);
      expect(store).toBeGreaterThan(verify);
    }
    // The no-password restore stores inside `landDeviceRestore`, which
    // `runNameCheckedDeviceRestore` calls only after every gate.
    const restore = authContext.indexOf("async completeFromDeviceRestore(");
    const land = authContext.indexOf("const landDeviceRestore = async", restore);
    const store = authContext.indexOf('"device-restore"', restore);
    const gates = authContext.indexOf("await runNameCheckedDeviceRestore(", restore);
    expect(land).toBeGreaterThan(restore);
    expect(store).toBeGreaterThan(land);
    expect(gates).toBeGreaterThan(store);
  });

  it("stores a reset identity's phrase only after the server reset answered, and before the publish (ADR-0073 2.3)", () => {
    // The point of no return is `reset/finish`: nothing is stored before it
    // (a failure there discards the generated phrase), and everything local
    // is persisted before the publishing steps, which can fail and be
    // retried with what was stored.
    // Since ADR-0074 the part after the server's answer is one function,
    // `adoptKeyResetOutcome`, which the reset and the recall both call.
    const start = authContext.indexOf("async mintFreshIdentity(");
    const reset = authContext.indexOf("await client.resetAccountKeys(", start);
    const adopt = authContext.indexOf("await adoptKeyResetOutcome(", start);
    const end = authContext.indexOf("async confirmFreshPhraseSaved(", start);
    expect(reset).toBeGreaterThan(start);
    expect(adopt).toBeGreaterThan(reset);
    expect(adopt).toBeLessThan(end);

    const adoptStart = authContext.indexOf("async function adoptKeyResetOutcome(");
    const store = authContext.indexOf('"fresh-identity"', adoptStart);
    const owed = authContext.indexOf("writeKeyResetOwed(", adoptStart);
    const publish = authContext.indexOf("await retryKeyResetPublish(", adoptStart);
    expect(adoptStart).toBeGreaterThan(0);
    expect(store).toBeGreaterThan(adoptStart);
    expect(owed).toBeGreaterThan(store);
    expect(publish).toBeGreaterThan(owed);
    // The phrase is stored in exactly one place for both.
    expect(authContext.split('"fresh-identity"').length - 1).toBe(1);
  });

  it("a recall continues through the reset's one path, and drops its record only after the new keys are stored (ADR-0074 part 1)", () => {
    // ADR-0075 section 8: the old device's recall and the recall by phrase
    // share one runner; each supplies what happens once the keys are stored.
    const wrapperStart = authContext.indexOf("function recallTakeBack(");
    const wrapper = authContext.slice(wrapperStart, authContext.indexOf("\n  }\n", wrapperStart));
    const start = authContext.indexOf("function runRecall(");
    const end = authContext.indexOf("\n  }\n", start);
    const body = authContext.slice(start, end);
    expect(wrapper.includes("return runRecall(")).toBe(true);
    expect(body.includes("afterPersist: target.afterPersist,")).toBe(true);
    // The server exchange is two calls since ADR-0074 T5 item 25 (the finish
    // request is kept until it is answered); the adoption follows the finish.
    const server = body.lastIndexOf("await client.recallFinish(");
    expect(body.indexOf("await client.recallPrepare(")).toBeGreaterThan(0);
    const adopt = body.indexOf("await adoptKeyResetOutcome(");
    const destroy = wrapper.indexOf("await deleteRecallRecord(");
    expect(server).toBeGreaterThan(0);
    expect(adopt).toBeGreaterThan(server);
    // Inside `afterPersist`, which the shared path runs once everything
    // local is stored; nowhere in the runner itself.
    expect(wrapper.indexOf("afterPersist:")).toBeGreaterThan(0);
    expect(destroy).toBeGreaterThan(wrapper.indexOf("afterPersist:"));
    expect(body.includes("deleteRecallRecord(")).toBe(false);
    expect(body.includes("storeDevicePhrase(")).toBe(false);
    expect(body.includes("publishResetIdentity(")).toBe(false);
    const adoptStart = authContext.indexOf("async function adoptKeyResetOutcome(");
    const persisted = authContext.indexOf("writeKeyResetOwed(", adoptStart);
    const hook = authContext.indexOf("await ctx.afterPersist?.();", adoptStart);
    const publish = authContext.indexOf("await retryKeyResetPublish(", adoptStart);
    expect(hook).toBeGreaterThan(persisted);
    expect(publish).toBeGreaterThan(hook);
  });

  it("lets nothing but the helper and the backup bootstrap write the phrase store", () => {
    const writers = sourceFiles(SRC)
      .filter((f) => /\bputPhrase\(/.test(readFileSync(f, "utf8")))
      .map((f) => f.slice(SRC.length))
      .sort();
    expect(writers).toEqual(["backup/bootstrap.ts", "backup/device-phrase.ts", "backup/phrase-store.ts"]);
  });
});
