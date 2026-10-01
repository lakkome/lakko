// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { beforeAll, describe, expect, it } from "vitest";

import init, { Group, GroupKind, Identity } from "../../wasm/lakko_ffi_wasm";
import {
  GROUP_KIND_GROUP,
  GROUP_KIND_ONE_TO_ONE,
  acceptsGroupName,
  conversationKindOf,
  conversationKindOfGroupKind,
} from "./conversation-kind";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function identity(fill: number): Identity {
  return Identity.fromSeed(new Uint8Array(64).fill(fill));
}

describe("conversationKindOfGroupKind", () => {
  it("a one-to-one is a chat", () => {
    expect(conversationKindOfGroupKind(GROUP_KIND_ONE_TO_ONE)).toBe("chat");
  });

  it("a group is a group", () => {
    expect(conversationKindOfGroupKind(GROUP_KIND_GROUP)).toBe("group");
  });

  it("a group carrying no kind is filed as a group, never as a chat", () => {
    expect(conversationKindOfGroupKind(undefined)).toBe("group");
  });
});

describe("conversationKindOf", () => {
  it("a throwing read is filed as a group", () => {
    expect(
      conversationKindOf({
        groupKind: () => {
          throw new Error("freed");
        },
      }),
    ).toBe("group");
  });
});

describe("acceptsGroupName (ADR-0060 section 7)", () => {
  it("a one-to-one never takes a group name", () => {
    expect(acceptsGroupName("chat")).toBe(false);
  });

  it("a group takes one", () => {
    expect(acceptsGroupName("group")).toBe(true);
  });
});

describe("through the wasm boundary", () => {
  it("the constants are the wasm GroupKind values", () => {
    expect(GroupKind.OneToOne).toBe(GROUP_KIND_ONE_TO_ONE);
    expect(GroupKind.Group).toBe(GROUP_KIND_GROUP);
  });

  it("a one-to-one reads as a chat and a group as a group, whatever the member count", () => {
    const oneToOne = new Group(identity(1), GroupKind.OneToOne);
    const group = new Group(identity(2), GroupKind.Group);
    // Both hold only their creator: the count says nothing, the kind decides.
    expect(oneToOne.memberCount).toBe(group.memberCount);
    expect(conversationKindOf(oneToOne)).toBe("chat");
    expect(conversationKindOf(group)).toBe("group");
  });
});
