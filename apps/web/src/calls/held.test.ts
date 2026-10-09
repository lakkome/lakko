// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `lastDefined`, and the failure it exists to prevent (`calls/held.ts`).
 *
 * No DOM and no JSX: `For` and `Show` are driven as the plain functions they
 * are, on Solid's real reactive build (see `vitest.config.ts`), in the very
 * shape the group call's grid has: a list keyed by participant id, a tile per
 * id drawn while the person is in the roster, and a tile that says who it
 * showed when it is taken down.
 *
 * The first test is the defect, kept so the mechanism is on record and so a
 * change of Solid's behaviour is noticed: read through the accessor `<Show>`
 * hands its child, the teardown throws out of the roster write, the roster
 * holds two people and everything drawn from it still says three.
 */

import { describe, expect, it } from "vitest";
import {
  createComponent,
  createEffect,
  createMemo,
  createRoot,
  createSignal,
  For,
  onCleanup,
  Show,
  type Accessor,
  type JSX,
} from "solid-js";

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";

import { lastDefined } from "./held";
import { sharedTileQualityRules, tileQualityOf, VideoWantsBook, type VideoQuality } from "./video-wants";

interface Person {
  readonly id: string;
}

interface Screen {
  /** The roster write under test; returns what it threw, if anything. */
  leave(id: string): unknown;
  join(id: string): void;
  /** The ids the grid draws. */
  drawn(): string[];
  /** The count the header draws. */
  header(): number;
  /** The roster itself. */
  roster(): string[];
  /** Who the tiles said they stopped showing. */
  withdrawn(): string[];
  dispose(): unknown;
}

/** `Show` as the plain function it is, without the overloads JSX picks from. */
const show = Show as unknown as (props: { when: unknown; children: unknown }) => JSX.Element;

type TileOf = (id: string, lookup: () => Person | undefined, gone: (id: string) => void) => JSX.Element;

/** The tile as it was: the person read back through `<Show>`'s accessor. */
const tileThroughShow: TileOf = (_id, lookup, gone) =>
  createComponent(show, {
    get when() {
      return lookup();
    },
    children: ((p: Accessor<Person>) => {
      onCleanup(() => gone(p().id));
      return p().id;
    }) as unknown as JSX.Element,
  });

/** The tile as it is: the person read through `lastDefined`. */
const tileHeld: TileOf = (_id, lookup, gone) => {
  const person = lastDefined(lookup);
  return createComponent(show, {
    get when() {
      return person() !== undefined;
    },
    get children() {
      onCleanup(() => gone((person() as Person).id));
      return (person() as Person).id;
    },
  });
};

function mount(tileOf: TileOf, ids: readonly string[]): Screen {
  return createRoot((dispose) => {
    const [people, setPeople] = createSignal<Person[]>(ids.map((id) => ({ id })));
    const gridIds = createMemo(() => people().map((p) => p.id), undefined, {
      equals: (a, b) => a.length === b.length && a.every((id, i) => id === b[i]),
    });
    const withdrawn: string[] = [];
    let header = 0;
    createEffect(() => {
      header = people().length;
    });
    const list = createComponent(For, {
      get each() {
        return gridIds();
      },
      children: (id: string) =>
        tileOf(
          id,
          () => people().find((p) => p.id === id),
          (gone) => withdrawn.push(gone),
        ),
    }) as unknown as Accessor<unknown[]>;
    let drawn: string[] = [];
    createEffect(() => {
      drawn = list().map((x) => String(typeof x === "function" ? (x as () => unknown)() : x));
    });
    const attempt = (fn: () => void): unknown => {
      try {
        fn();
        return null;
      } catch (err) {
        return err;
      }
    };
    return {
      leave: (id) => attempt(() => setPeople((l) => l.filter((p) => p.id !== id))),
      join: (id) => {
        setPeople((l) => (l.some((p) => p.id === id) ? l : [...l, { id }]));
      },
      drawn: () => drawn,
      header: () => header,
      roster: () => people().map((p) => p.id),
      withdrawn: () => withdrawn,
      dispose: () => attempt(dispose),
    };
  });
}

/** Effects of a root run after the function that made it returns. */
function settle(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

describe("a tile that reads its person through <Show>'s accessor", () => {
  it("throws out of the roster write and leaves the screen saying three", async () => {
    const s = mount(tileThroughShow, ["local", "b", "c"]);
    await settle();
    expect(s.drawn()).toEqual(["local", "b", "c"]);
    const threw = s.leave("c");
    expect(String(threw)).toContain("<Show>");
    // The store dropped the person; nothing drawn from it followed.
    expect(s.roster()).toEqual(["local", "b"]);
    expect(s.drawn()).toEqual(["local", "b", "c"]);
    expect(s.header()).toBe(3);
  });

  it("and its screen cannot be taken down afterwards", async () => {
    const s = mount(tileThroughShow, ["local", "b", "c"]);
    await settle();
    s.leave("c");
    expect(String(s.dispose())).toContain("<Show>");
  });
});

describe("a tile that reads its person through lastDefined", () => {
  it("is taken down with the person, and the count follows", async () => {
    const s = mount(tileHeld, ["local", "b", "c"]);
    await settle();
    expect(s.leave("c")).toBeNull();
    expect(s.roster()).toEqual(["local", "b"]);
    expect(s.drawn()).toEqual(["local", "b"]);
    expect(s.header()).toBe(2);
    // The tile could still say who it was showing.
    expect(s.withdrawn()).toEqual(["c"]);
  });

  it("gives a person who comes back one tile", async () => {
    const s = mount(tileHeld, ["local", "b", "c"]);
    await settle();
    s.leave("c");
    s.join("c");
    s.join("c");
    expect(s.drawn()).toEqual(["local", "b", "c"]);
    expect(s.header()).toBe(3);
  });

  it("can be taken down after a person left, and with everyone at once", async () => {
    const s = mount(tileHeld, ["local", "b", "c"]);
    await settle();
    s.leave("c");
    expect(s.dispose()).toBeNull();
    expect(s.withdrawn().sort()).toEqual(["b", "c", "local"]);
  });
});

describe("lastDefined", () => {
  it("is undefined until there is a value, then keeps the last one", () => {
    createRoot((dispose) => {
      const [value, setValue] = createSignal<string | undefined | null>(undefined);
      const held = lastDefined(value);
      expect(held()).toBeUndefined();
      setValue("a");
      expect(held()).toBe("a");
      setValue(undefined);
      expect(held()).toBe("a");
      setValue(null);
      expect(held()).toBe("a");
      setValue("b");
      expect(held()).toBe("b");
      dispose();
    });
  });
});

describe("what is asked of the person who remains", () => {
  it("becomes the main picture when two people are left", async () => {
    const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
    await init({ module_or_path: readFileSync(path) });
    const book = new VideoWantsBook();
    book.setPeers(["b", "c"]);
    const tiles = { b: {}, c: {} };
    const width = 400;
    const ask = (people: number): VideoQuality =>
      tileQualityOf(sharedTileQualityRules, people, width, false);
    // Three in the call: each grid tile asks for the middle picture.
    book.setTile(tiles.b, "b", ask(book.people()));
    book.setTile(tiles.c, "c", ask(book.people()));
    expect(Object.fromEntries(book.wants())).toEqual({ b: "middle", c: "middle" });
    // `c` leaves: their tile is gone, and the remaining tile asks again with
    // the people now in the call.
    book.removePeer("c");
    book.setTile(tiles.c, "c", null);
    expect(book.people()).toBe(2);
    book.setTile(tiles.b, "b", ask(book.people()));
    expect(Object.fromEntries(book.wants())).toEqual({ b: "main" });
  });
});
