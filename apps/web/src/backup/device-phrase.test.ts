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

  it("stores a reset identity's phrase only after the server reset answered, and before the publish (ADR-0065 2.3)", () => {
    // The point of no return is `reset/finish`: nothing is stored before it
    // (a failure there discards the generated phrase), and everything local
    // is persisted before the publishing steps, which can fail and be
    // retried with what was stored.
    const start = authContext.indexOf("async mintFreshIdentity(");
    const reset = authContext.indexOf("await client.resetAccountKeys(", start);
    const store = authContext.indexOf('"fresh-identity"', start);
    const publish = authContext.indexOf("await retryKeyResetPublish(", start);
    expect(reset).toBeGreaterThan(start);
    expect(store).toBeGreaterThan(reset);
    expect(publish).toBeGreaterThan(store);
  });

  it("lets nothing but the helper and the backup bootstrap write the phrase store", () => {
    const writers = sourceFiles(SRC)
      .filter((f) => /\bputPhrase\(/.test(readFileSync(f, "utf8")))
      .map((f) => f.slice(SRC.length))
      .sort();
    expect(writers).toEqual(["backup/bootstrap.ts", "backup/device-phrase.ts", "backup/phrase-store.ts"]);
  });
});
