// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Object URLs pin decrypted plaintext in memory until revoked; the registry
 * must release every URL it hands out, including ones that arrive after the
 * view that asked for them has gone.
 */
import { describe, expect, it } from "vitest";

import { ObjectUrlRegistry } from "./object-url-registry";

function harness(): { registry: ObjectUrlRegistry; live: Set<string> } {
  const live = new Set<string>();
  let seq = 0;
  const registry = new ObjectUrlRegistry(
    () => {
      seq += 1;
      const url = `blob:test/${seq}`;
      live.add(url);
      return url;
    },
    (url) => {
      live.delete(url);
    },
  );
  return { registry, live };
}

const blob = (): Blob => new Blob([new Uint8Array([1])]);

describe("ObjectUrlRegistry", () => {
  it("replacing a key revokes the URL it replaces", () => {
    const { registry, live } = harness();
    const first = registry.set("a", blob());
    const second = registry.set("a", blob());
    expect(first).not.toBe(second);
    expect(live).toEqual(new Set([second]));
    expect(registry.get("a")).toBe(second);
  });

  it("revoke and revokeAll release everything", () => {
    const { registry, live } = harness();
    registry.set("a", blob());
    registry.set("b", blob());
    registry.set("c", blob());
    registry.revoke("b");
    expect(live.size).toBe(2);
    expect(registry.get("b")).toBeNull();
    registry.revokeAll();
    expect(live.size).toBe(0);
    expect(registry.size).toBe(0);
  });

  it("after dispose a late registration is revoked on arrival", () => {
    const { registry, live } = harness();
    registry.set("a", blob());
    registry.dispose();
    expect(live.size).toBe(0);
    expect(registry.set("late", blob())).toBeNull();
    expect(live.size).toBe(0);
    expect(registry.get("late")).toBeNull();
    registry.dispose();
    expect(registry.isDisposed).toBe(true);
  });

  it("renaming moves the one URL and revokes nothing", () => {
    // What an outgoing take's bytes do when its upload lands. A media element
    // may be READING this URL at that moment, so the move has to leave the URL
    // string alone: `set` under the new key plus `revoke` of the old one made a
    // second copy of the same bytes and pulled the first out from under it.
    const { registry, live } = harness();
    const url = registry.set("local:1", blob());
    expect(registry.rename("local:1", "7e7e")).toBe(true);
    expect(registry.get("7e7e")).toBe(url);
    expect(registry.get("local:1")).toBeNull();
    expect(live).toEqual(new Set([url]));
    expect(registry.size).toBe(1);
  });

  it("renaming an unknown key changes nothing and says so", () => {
    const { registry, live } = harness();
    const kept = registry.set("a", blob());
    expect(registry.rename("nothing", "a")).toBe(false);
    expect(registry.get("a")).toBe(kept);
    expect(live).toEqual(new Set([kept]));
  });

  it("renaming onto an occupied key releases what was there", () => {
    const { registry, live } = harness();
    const stale = registry.set("7e7e", blob());
    const url = registry.set("local:1", blob());
    expect(registry.rename("local:1", "7e7e")).toBe(true);
    expect(registry.get("7e7e")).toBe(url);
    expect(live).toEqual(new Set([url]));
    expect(stale).not.toBe(url);
  });

  it("revoking an unknown key is a no-op", () => {
    const { registry, live } = harness();
    registry.set("a", blob());
    registry.revoke("zzz");
    expect(live.size).toBe(1);
  });
});
