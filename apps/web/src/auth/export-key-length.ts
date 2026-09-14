// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The OPAQUE export-key length, and the one check every cache of it runs.
 *
 * ## Why this is its own module
 *
 * `export-key-store.ts` used to assert a length of **32** bytes before it
 * wrapped the key under the device root key. The suite Lakko actually runs —
 * `LakkoCipherSuite` in `crates/lakko-auth-client/src/cipher_suite.rs`:
 * Ristretto255 OPRF over SHA-512 — produces a **64**-byte export key
 * (`EXPORT_KEY_LEN`), and that is the value `OpaqueRegisterOutput.exportKey` /
 * `OpaqueLoginOutput.exportKey` hand the web client verbatim.
 *
 * So the check was not merely noisy. It threw on EVERY real key, which the
 * signup / login / recover call sites caught and logged as
 * `export key length 64 != 32`, and the cache was therefore never written. The
 * features that read it silently lost their input: passkey sign-in on a device
 * always ended in "No cached credentials for passkey sign-in on this device",
 * and a silent session restore always carried an empty export key.
 *
 * The number lives here, next to a check that cannot disagree with it, and the
 * unit test pins it against the Rust constant it mirrors, so the next suite
 * change fails a test instead of a sign-in.
 */

/** OPAQUE `export_key` length for `LakkoCipherSuite` (Ristretto255 / SHA-512).
 *  Mirrors `EXPORT_KEY_LEN` in `crates/lakko-auth-client/src/cipher_suite.rs`. */
export const OPAQUE_EXPORT_KEY_LEN = 64;

/** AES-GCM IV length the export-key envelope is written with. */
const IV_LEN = 12;
/** AES-GCM authentication tag length. */
const TAG_LEN = 16;

/** Length of a well-formed `[IV | ciphertext | tag]` envelope. AES-GCM adds no
 *  padding, so an envelope for a correct key is exactly this long. */
export const EXPORT_KEY_ENVELOPE_LEN = IV_LEN + OPAQUE_EXPORT_KEY_LEN + TAG_LEN;

/**
 * Throw unless `exportKey` is exactly {@link OPAQUE_EXPORT_KEY_LEN} bytes.
 *
 * `what` names the value in the error ("export key", "decrypted export key"),
 * so the two directions of the cache read distinctly in a log.
 */
export function assertExportKeyLength(exportKey: Uint8Array, what: string): void {
  if (exportKey.length !== OPAQUE_EXPORT_KEY_LEN) {
    throw new Error(
      `${what} length ${exportKey.length} != ${OPAQUE_EXPORT_KEY_LEN} ` +
        "(LakkoCipherSuite OPAQUE export-key contract)",
    );
  }
}
