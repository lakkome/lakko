// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A passkey creation the provider refuses because it already holds a passkey
 * for this account is its own error and its own line, never "this browser
 * can't create passkeys" (docs/ux/identity-recovery.md 10.2, amended
 * 2026-11-11).
 */
import { describe, expect, it } from "vitest";

import { passkeyErrorKey } from "./passkey-error-key";
import {
  mapDomError,
  WebauthnCredentialExistsError,
  WebauthnUnsupportedError,
  WebauthnUserCancelledError,
} from "./webauthn-client";

function domError(name: string): Error {
  const err = new Error(`${name} from the browser`);
  err.name = name;
  return err;
}

describe("a WebAuthn DOM error", () => {
  it("from create with InvalidStateError means a passkey already exists", () => {
    const mapped = mapDomError(domError("InvalidStateError"), "create");
    expect(mapped).toBeInstanceOf(WebauthnCredentialExistsError);
    expect(passkeyErrorKey(mapped)).toBe("settings.passkeys.error.already_exists");
  });

  it("from get with InvalidStateError keeps the unsupported reading", () => {
    expect(mapDomError(domError("InvalidStateError"), "get")).toBeInstanceOf(
      WebauthnUnsupportedError,
    );
  });

  it("NotSupportedError stays unsupported for either ceremony", () => {
    for (const ceremony of ["create", "get"] as const) {
      const mapped = mapDomError(domError("NotSupportedError"), ceremony);
      expect(mapped).toBeInstanceOf(WebauthnUnsupportedError);
      expect(passkeyErrorKey(mapped)).toBe("settings.passkeys.error.unsupported");
    }
  });

  it("a dismissed sheet is a cancel for either ceremony", () => {
    for (const ceremony of ["create", "get"] as const) {
      for (const name of ["NotAllowedError", "AbortError"]) {
        expect(mapDomError(domError(name), ceremony)).toBeInstanceOf(WebauthnUserCancelledError);
      }
    }
  });

  it("anything else passes through unchanged", () => {
    const err = domError("SecurityError");
    expect(mapDomError(err, "create")).toBe(err);
  });
});
