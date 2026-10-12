// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One email-vault job (ADR-0067 section 2), over the subset of the wasm core
 * it needs, so the worker and the main-thread fallback run the same code and
 * a test can run it with a fake core.
 *
 * Both calls run Argon2id over 64 MiB: `email-vault.ts` runs them in a
 * worker.
 */

/** The mnemonic handle the core returns. */
export interface VaultMnemonic {
  readonly phrase: string;
  free(): void;
}

/** The subset of the wasm core a job uses. */
export interface EmailVaultCore {
  RecoveryMnemonic: { fromPhrase(phrase: string): VaultMnemonic };
  emailVaultSeal(
    mnemonic: VaultMnemonic,
    loginHandle: Uint8Array,
    address: string,
    vaultSalt: Uint8Array,
  ): Uint8Array;
  emailVaultOpen(
    loginHandle: Uint8Array,
    address: string,
    vaultSalt: Uint8Array,
    sealed: Uint8Array,
  ): VaultMnemonic;
}

export type EmailVaultRequest =
  | {
      readonly kind: "seal";
      /** The account's 12 words, as this device holds them. */
      readonly phrase: string;
      readonly loginHandle: Uint8Array;
      readonly address: string;
      readonly vaultSalt: Uint8Array;
    }
  | {
      readonly kind: "open";
      readonly loginHandle: Uint8Array;
      readonly address: string;
      readonly vaultSalt: Uint8Array;
      readonly sealed: Uint8Array;
    };

export type EmailVaultResult =
  /** The 44-byte blob `enrol/finish` stores. */
  | { readonly kind: "sealed"; readonly sealed: Uint8Array }
  /** The phrase the vault held. */
  | { readonly kind: "opened"; readonly phrase: string }
  /** The blob does not open under this username, address and salt,
   *  whatever the reason. */
  | { readonly kind: "not-opened" }
  /** Anything else; `code` is the core's `"<code>"` prefix. */
  | { readonly kind: "error"; readonly code: string };

/** The `<code>` of a core refusal (`"<code>: <reason>"`). */
export function vaultErrorCode(err: unknown): string {
  const message = err instanceof Error ? err.message : "";
  const code = message.split(":")[0]?.trim() ?? "";
  return /^[a-z-]+$/.test(code) ? code : "unknown";
}

export function runEmailVaultJob(core: EmailVaultCore, request: EmailVaultRequest): EmailVaultResult {
  try {
    if (request.kind === "seal") {
      const mnemonic = core.RecoveryMnemonic.fromPhrase(request.phrase);
      try {
        return {
          kind: "sealed",
          sealed: core.emailVaultSeal(mnemonic, request.loginHandle, request.address, request.vaultSalt),
        };
      } finally {
        mnemonic.free();
      }
    }
    const opened = core.emailVaultOpen(
      request.loginHandle,
      request.address,
      request.vaultSalt,
      request.sealed,
    );
    try {
      return { kind: "opened", phrase: opened.phrase };
    } finally {
      opened.free();
    }
  } catch (err) {
    const code = vaultErrorCode(err);
    return code === "not-opened" ? { kind: "not-opened" } : { kind: "error", code };
  }
}
