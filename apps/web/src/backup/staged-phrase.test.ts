// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `StagedPhrase` (ADR-0077 decision 3 rule 1, G-47): the phrase is written
 * before the irrevocable step, a write that cannot be made refuses it, and a
 * step that did not happen puts the row back only while it still holds this
 * phrase.
 */
import { describe, expect, it } from "vitest";

import { PhraseNotStagedError, StagedPhrase, type PhraseRow } from "./staged-phrase";

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);
const dec = (b: Uint8Array | null): string | null => (b === null ? null : new TextDecoder().decode(b));

/** A row whose snapshot is the stored text itself. */
class FakeRow implements PhraseRow<string> {
  value: string | null;
  refuse = false;
  failWrite = false;
  failRead = false;
  constructor(initial: string | null) {
    this.value = initial;
  }
  async readSnapshot(): Promise<string | null> {
    if (this.failRead) throw new Error("unreadable");
    return this.value;
  }
  async readPhrase(): Promise<Uint8Array | null> {
    return this.value === null ? null : enc(this.value);
  }
  async write(phrase: Uint8Array): Promise<boolean> {
    if (this.failWrite) throw new Error("write failed");
    if (this.refuse) return false;
    this.value = dec(phrase);
    return true;
  }
  async restore(snapshot: string): Promise<boolean> {
    if (this.refuse) return false;
    this.value = snapshot;
    return true;
  }
  async remove(): Promise<void> {
    this.value = null;
  }
}

describe("staging a new phrase", () => {
  it("writes the phrase over what the row held", async () => {
    const row = new FakeRow("old words");
    const staged = new StagedPhrase(row, "new words");
    await staged.stage();
    expect(row.value).toBe("new words");
    expect(await staged.isHeld()).toBe(true);
  });

  it("refuses when the gate refuses the write, and when the row cannot be read or written", async () => {
    for (const setup of [
      (r: FakeRow) => (r.refuse = true),
      (r: FakeRow) => (r.failWrite = true),
      (r: FakeRow) => (r.failRead = true),
    ]) {
      const row = new FakeRow("old words");
      setup(row);
      await expect(new StagedPhrase(row, "new words").stage()).rejects.toBeInstanceOf(PhraseNotStagedError);
      expect(row.value).toBe("old words");
    }
  });

  it("a step that did not happen puts the previous row back", async () => {
    const row = new FakeRow("old words");
    const staged = new StagedPhrase(row, "new words");
    await staged.stage();
    expect(await staged.rollback()).toBe("restored");
    expect(row.value).toBe("old words");
  });

  it("removes the row when there was none before", async () => {
    const row = new FakeRow(null);
    const staged = new StagedPhrase(row, "new words");
    await staged.stage();
    expect(await staged.rollback()).toBe("restored");
    expect(row.value).toBeNull();
  });

  it("leaves a row that no longer holds this phrase alone", async () => {
    const row = new FakeRow("old words");
    const staged = new StagedPhrase(row, "new words");
    await staged.stage();
    // A later write replaced it.
    row.value = "someone else's words";
    expect(await staged.rollback()).toBe("not-held");
    expect(row.value).toBe("someone else's words");
    // A wipe removed it.
    const wiped = new FakeRow("old words");
    const second = new StagedPhrase(wiped, "new words");
    await second.stage();
    wiped.value = null;
    expect(await second.rollback()).toBe("not-held");
    expect(wiped.value).toBeNull();
  });

  it("does not roll back what it never staged", async () => {
    const row = new FakeRow("new words");
    const staged = new StagedPhrase(row, "new words");
    expect(await staged.rollback()).toBe("not-held");
    expect(row.value).toBe("new words");
  });
});
