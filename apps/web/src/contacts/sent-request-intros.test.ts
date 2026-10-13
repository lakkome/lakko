// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two device-key stores the spend round of ADR-0074 section 3.5a rule 5
 * reads, through the in-memory IndexedDB: the intro kept for a sent request,
 * and the list of links this device minted.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { putDeviceKey } from "../auth/key-store";
import { FakeIndexedDb } from "../test/fake-storage";
import { listMintedLinks, rememberMintedLink, forgetMintedLink } from "./minted-links";
import {
  deleteSentIntrosForAccount,
  forgetSentIntro,
  rememberSentIntro,
  sentIntroFor,
} from "./sent-request-intros";

const ACCOUNT = "c2VudC1pbnRyb3MtYWNjb3VudA==";
const OTHER = "b3RoZXItYWNjb3VudC0wMDAwMA==";
const HEX_A = "00112233445566778899aabbccddeeff";
const HEX_B = "ffeeddccbbaa99887766554433221100";

async function installDeviceKey(account: string): Promise<void> {
  const key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "encrypt",
    "decrypt",
  ]);
  await putDeviceKey(account, key);
}

beforeEach(async () => {
  (globalThis as unknown as Record<string, unknown>)["indexedDB"] = new FakeIndexedDb();
  await installDeviceKey(ACCOUNT);
});

afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["indexedDB"];
});

describe("the intro of a sent request, under the device key", () => {
  it("survives a re-read, per recipient", async () => {
    await rememberSentIntro(ACCOUNT, HEX_A, "hello from the fair");
    await rememberSentIntro(ACCOUNT, HEX_B, "second");
    expect(await sentIntroFor(ACCOUNT, HEX_A)).toBe("hello from the fair");
    expect(await sentIntroFor(ACCOUNT, HEX_B)).toBe("second");
  });

  it("is stored as ciphertext", async () => {
    await rememberSentIntro(ACCOUNT, HEX_A, "hello from the fair");
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const req = indexedDB.open("lakko-sent-request-intros", 1);
      req.onsuccess = (): void => resolve(req.result);
      req.onerror = (): void => reject(req.error);
    });
    const row = await new Promise<{ ciphertext: Uint8Array } | undefined>((resolve) => {
      const req = db.transaction("rows", "readonly").objectStore("rows").get(ACCOUNT);
      req.onsuccess = (): void => resolve(req.result as { ciphertext: Uint8Array } | undefined);
    });
    db.close();
    expect(row).toBeDefined();
    expect(new TextDecoder().decode(row?.ciphertext)).not.toContain("hello");
  });

  it("goes when the request is answered or withdrawn, and with the account wipe", async () => {
    await rememberSentIntro(ACCOUNT, HEX_A, "hello");
    await rememberSentIntro(ACCOUNT, HEX_B, "second");
    await forgetSentIntro(ACCOUNT, HEX_A);
    expect(await sentIntroFor(ACCOUNT, HEX_A)).toBe("");
    expect(await sentIntroFor(ACCOUNT, HEX_B)).toBe("second");
    await deleteSentIntrosForAccount(ACCOUNT);
    expect(await sentIntroFor(ACCOUNT, HEX_B)).toBe("");
  });

  it("without a device key nothing is kept and a read is empty", async () => {
    await rememberSentIntro(OTHER, HEX_A, "hello");
    expect(await sentIntroFor(OTHER, HEX_A)).toBe("");
  });
});

describe("the links this device minted, listed for the spend round", () => {
  it("lists both kinds with their mint time, of this account only", async () => {
    await installDeviceKey(OTHER);
    const link = (fill: number) => ({
      linkSecret: new Uint8Array(12).fill(fill),
      linkId: new Uint8Array(16).fill(fill),
    });
    await rememberMintedLink(ACCOUNT, "contact", "slugI", link(1), 1000);
    await rememberMintedLink(ACCOUNT, "call", "slugC", link(2), 2000);
    await rememberMintedLink(OTHER, "call", "slugX", link(3), 3000);
    const listed = (await listMintedLinks(ACCOUNT)).sort((a, b) => (a.mintedAtMs ?? 0) - (b.mintedAtMs ?? 0));
    expect(listed.map((l) => [l.kind, l.blobIdSlug, l.mintedAtMs])).toEqual([
      ["contact", "slugI", 1000],
      ["call", "slugC", 2000],
    ]);
    expect(Array.from(listed[1]?.linkId ?? [])).toEqual(Array.from(link(2).linkId));
    await forgetMintedLink(ACCOUNT, "call", "slugC");
    expect((await listMintedLinks(ACCOUNT)).map((l) => l.blobIdSlug)).toEqual(["slugI"]);
  });
});
