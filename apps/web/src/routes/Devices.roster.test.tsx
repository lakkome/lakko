// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 3 - the open Devices screen follows the
 * roster. Seen live: after the code route, whose prompt lives in the app shell,
 * the new device was not on the open screen until the person left and came
 * back. This mounts the screen and moves the roster, as a commit does.
 */
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DeviceListItem } from "../devices/remove";
import type { DeviceRoster } from "../devices/sync/DeviceGroupSyncContext";

const h = vi.hoisted(() => ({
  listDevices: vi.fn(),
  roster: null as null | (() => DeviceRoster | null),
}));

vi.mock("@solidjs/router", () => ({ A: () => null }));
vi.mock("../l10n", () => ({
  t: (key: string) => key,
  currentLanguage: () => "en",
  displayLocale: () => "en",
}));
vi.mock("../ui/icons", () => ({ Icon: () => null }));
vi.mock("../ui/ModalLayer", () => ({ ModalLayer: () => null }));
vi.mock("../auth/AuthContext", () => ({
  useAuth: () => ({
    state: () => ({ kind: "session", account: { accountId: "acct", username: "ann" } }),
    identitySecrets: () => ({ accountId: new Uint8Array(16), sessionToken: "t" }),
    keyResetNote: () => false,
    dismissKeyResetNote: () => undefined,
  }),
}));
vi.mock("../auth/key-store", () => ({ getDeviceKey: async () => ({}) }));
vi.mock("../auth/client", () => ({ AuthClient: class {} }));
vi.mock("../auth/webauthn-client", () => ({
  WebauthnClient: class {},
  WebauthnHttpError: class extends Error {},
  actionHashForRemoveDevice: vi.fn(),
}));
vi.mock("../auth/protect/account-protection", () => ({ newWordsShown: () => false }));
vi.mock("../auth/protect/protection-state", () => ({ phraseProtectedHere: () => false }));
vi.mock("../auth/auth-error-copy", () => ({ screenText: () => null }));
vi.mock("../auth/fresh-auth", () => ({ FreshAuthRequiredError: class extends Error {}, PROOF_COPY: {} }));
vi.mock("../auth/proof-gate", () => ({ createProofGate: () => ({ clear: () => undefined }) }));
vi.mock("../auth/ProofBlock", () => ({ ProofBlock: () => null }));
vi.mock("../auth/session-device", () => ({ deviceIdB64Url: () => "id" }));
vi.mock("../chat/BackgroundReceiveContext", () => ({ useBackgroundReceive: () => ({}) }));
vi.mock("../chat/ChatContext", () => ({ useChats: () => ({}) }));
vi.mock("../profile/own-key", () => ({ ownKeysB64Of: () => [] }));
vi.mock("../devices/removal-runtime", () => ({
  useDeviceRemoval: () => ({ owed: () => [], blocked: () => false }),
}));
vi.mock("../devices/removal", () => ({ removalRowState: () => "normal" }));
vi.mock("../devices/removal-copy", () => ({
  REMOVE_DEVICE_COPY: {},
  ROTATE_COPY: {},
  revokeFailureLine: () => () => "",
}));
vi.mock("../devices/key-rotation-runtime", () => ({
  useKeyRotation: () => ({ record: () => null, activeStep: () => null }),
}));
vi.mock("../devices/key-rotation", () => ({
  rotationCommitted: () => false,
  rotationSavesPhrase: () => false,
  rotationStepLine: () => null,
}));
vi.mock("../devices/orchestrator", () => ({ prepareJoinFlow: vi.fn(), runScanFlow: vi.fn() }));
vi.mock("../devices/pairing", () => ({ ownPairingHandle: vi.fn(), parsePairingQrPayload: vi.fn() }));
vi.mock("../devices/pairing-copy", () => ({ pairingConfirmTitle: () => "" }));
vi.mock("../devices/recovery-sender", () => ({ recoveryHalfFor: vi.fn() }));
vi.mock("../devices/added-notice", () => ({
  addedNotice: () => ({ newKeys: [], titleKey: "t" }),
  noteDeviceLetIn: () => undefined,
}));
vi.mock("../devices/DeviceAddedNotice", () => ({ DeviceAddedBanner: () => null }));
vi.mock("../devices/device-card-runtime", () => ({
  currentOwnFields: () => ({}),
  deviceCards: () => ({}),
  ownDeviceCard: () => null,
  startPairingLocate: vi.fn(),
}));
vi.mock("../devices/device-cards", () => ({
  countryNameOf: () => "",
  deviceRowView: (row: { keyHex: string }) => ({
    icon: "browser",
    title: `device-${row.keyHex}`,
    detail: "",
    place: "",
    activity: "",
  }),
  orderDeviceRows: (rows: { isSelf: boolean }[]) => ({
    self: rows.find((r) => r.isSelf) ?? null,
    others: rows.filter((r) => !r.isSelf),
  }),
  pairingCardView: () => null,
  selfRowCard: () => null,
}));
vi.mock("../chat/folders/folder-model", () => ({
  bytesToHex: (b: Uint8Array) => [...b].map((x) => x.toString(16).padStart(2, "0")).join(""),
}));
vi.mock("../devices/remove", () => ({ listDevices: h.listDevices }));
vi.mock("../devices/sync/DeviceGroupSyncContext", () => ({
  useDeviceGroupSync: () => ({ deviceRoster: () => h.roster?.() ?? null }),
}));
vi.mock("qrcode", () => ({ default: {} }));
vi.mock("jsqr", () => ({ default: () => null }));

const { default: Devices } = await import("./Devices");

const device = (key: number, isSelf = false): DeviceListItem => ({
  leafIndex: key,
  signaturePublicKey: new Uint8Array([key]),
  isSelf,
});

const shown = (host: HTMLElement): boolean => host.textContent?.includes("device-02") === true;

let host: HTMLElement;
let dispose: () => void;
let setRoster: (r: DeviceRoster | null) => void;

beforeEach(() => {
  const [roster, set] = createSignal<DeviceRoster | null>({ keys: ["01"], ownKey: "01", addedHere: [] });
  h.roster = roster;
  setRoster = set;
  h.listDevices.mockReset();
  h.listDevices.mockResolvedValue({ ownLeafIndex: 1, devices: [device(1, true)] });
  host = document.createElement("div");
  document.body.append(host);
  dispose = render(() => <Devices />, host);
});

afterEach(() => {
  dispose();
  host.remove();
});

describe("the open Devices screen", () => {
  it("shows a device that joined, with no reload of the route", async () => {
    await vi.waitFor(() => expect(host.textContent).toContain("device-01"));
    expect(shown(host)).toBe(false);

    h.listDevices.mockResolvedValue({ ownLeafIndex: 1, devices: [device(1, true), device(2)] });
    setRoster({ keys: ["01", "02"], ownKey: "01", addedHere: ["02"] });
    await vi.waitFor(() => expect(shown(host)).toBe(true));
  });

  it("drops a device that left", async () => {
    h.listDevices.mockResolvedValue({ ownLeafIndex: 1, devices: [device(1, true), device(2)] });
    setRoster({ keys: ["01", "02"], ownKey: "01", addedHere: [] });
    await vi.waitFor(() => expect(shown(host)).toBe(true));

    h.listDevices.mockResolvedValue({ ownLeafIndex: 1, devices: [device(1, true)] });
    setRoster({ keys: ["01"], ownKey: "01", addedHere: [] });
    await vi.waitFor(() => expect(shown(host)).toBe(false));
  });

  it("keeps the list on screen while it reads it again, and if that fails", async () => {
    await vi.waitFor(() => expect(host.textContent).toContain("device-01"));
    let finish: (reason: Error) => void = () => undefined;
    h.listDevices.mockReturnValue(
      new Promise((_, reject) => {
        finish = reject;
      }),
    );
    setRoster({ keys: ["01", "02"], ownKey: "01", addedHere: [] });
    await vi.waitFor(() => expect(h.listDevices).toHaveBeenCalledTimes(2));
    expect(host.textContent).not.toContain("common.loading");
    expect(host.textContent).toContain("device-01");

    finish(new Error("read failed"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(host.textContent).toContain("device-01");
    expect(host.textContent).not.toContain("settings.devices.error.load");
  });

  it("lands only the newest read", async () => {
    await vi.waitFor(() => expect(host.textContent).toContain("device-01"));
    let finishOlder: (list: unknown) => void = () => undefined;
    h.listDevices.mockReturnValueOnce(
      new Promise((resolve) => {
        finishOlder = resolve;
      }),
    );
    h.listDevices.mockResolvedValueOnce({ ownLeafIndex: 1, devices: [device(1, true), device(2)] });
    setRoster({ keys: ["01", "03"], ownKey: "01", addedHere: [] });
    await vi.waitFor(() => expect(h.listDevices).toHaveBeenCalledTimes(2));
    setRoster({ keys: ["01", "02"], ownKey: "01", addedHere: [] });
    await vi.waitFor(() => expect(shown(host)).toBe(true));

    finishOlder({ ownLeafIndex: 1, devices: [device(1, true)] });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(shown(host)).toBe(true);
  });
});
