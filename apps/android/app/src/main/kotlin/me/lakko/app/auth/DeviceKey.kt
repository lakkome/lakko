// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * Device root key derivation.
 *
 * Mirror of `apps/web/src/auth/device-key.ts`. Same HKDF
 * construction, same label, same output length — a user signing
 * up in the web client and re-installing on Android will produce
 * the **same** device root key from the same password, because
 * the OPAQUE export key is deterministic across cipher-suite-
 * compatible implementations.
 *
 * Inputs and outputs:
 *
 *   * **Input** — the OPAQUE password export key (64 bytes, fresh
 *     out of `OpaqueRegister.finish` / `OpaqueLogin.finish`).
 *   * **Output** — a 32-byte AES-256 key. Raw bytes in
 *     Kotlin-side heap. The caller is expected to feed them
 *     immediately into [DeviceKeyStore.putDeviceKey] (which wraps
 *     them under a Keystore-resident AES-GCM master) and to
 *     zeroize the array as soon as the wrap completes.
 *
 * Derivation, identical to the web side:
 *
 * ```text
 *   IKM    = export_key             // 64 bytes
 *   salt   = "lakko-device-root-v1" // ASCII-only, fixed
 *   info   = "lakko/device-root/v1|" || account_id_b64
 *   length = 32 bytes (AES-256)
 * ```
 *
 * ## Why not Keystore-resident HKDF?
 *
 * Android's `KeyAgreement` / HKDF surfaces in `AndroidKeyStore`
 * arrived in API 31+ as `KeyProperties.PURPOSE_AGREE_KEY` and
 * still ship inconsistent behaviour across vendor builds. Doing
 * HKDF in plain Kotlin with `javax.crypto.Mac` works on every
 * supported API (our `minSdk = 26`) and produces byte-identical
 * output to the WebCrypto HKDF in `apps/web` — the web and the
 * Android client agree on the same device root key for the same
 * account, important for the eventual cross-device parity we
 * promise in 1.15 (one identity, multiple platforms, same data).
 *
 * The HKDF output is then **wrapped** under a Keystore key (see
 * [DeviceKeyStore]) — the master never leaves Keystore, but the
 * derived working key is small and short-lived enough that the
 * heap-side exposure is acceptable.
 *
 * ## Tests
 *
 * Host-side tests in `app/src/test/.../DeviceKeyTest.kt` exercise
 * the HKDF round-trip against RFC 5869 test vectors and verify
 * determinism + account-id binding.
 */

/** Fixed HKDF salt — must match `apps/web`'s `HKDF_SALT`. */
private val HKDF_SALT_BYTES: ByteArray =
    "lakko-device-root-v1".toByteArray(Charsets.US_ASCII)

/** Fixed HKDF info-prefix; the account_id_b64 string is appended. */
private const val HKDF_INFO_PREFIX = "lakko/device-root/v1|"

/** AES-256 — the output of the derivation, ready to drive AES-GCM. */
private const val DEVICE_ROOT_KEY_LEN = 32

/** HMAC-SHA-512 — RFC 5869 hash-len. */
private const val HMAC_SHA_512_LEN = 64

/**
 * Derive the 32-byte device root key for a given account.
 *
 * @param exportKey       OPAQUE password export key bytes (64 bytes
 *                        for `LakkoCipherSuite`). Caller is
 *                        responsible for wiping (`.fill(0)`) after
 *                        this call — the function does not retain
 *                        a reference.
 * @param accountIdB64    Base64-encoded raw 16-byte UUIDv7 account
 *                        id. Mixed into the HKDF `info` so two
 *                        accounts on the same device derive
 *                        different keys.
 * @return                Fresh 32-byte AES-256 key. **Treat as
 *                        secret**: hand to [DeviceKeyStore.putDeviceKey]
 *                        immediately and zeroize on the caller side.
 */
public fun deriveDeviceRootKey(
    exportKey: ByteArray,
    accountIdB64: String,
): ByteArray {
    val info = (HKDF_INFO_PREFIX + accountIdB64).toByteArray(Charsets.US_ASCII)
    return hkdfSha512(
        ikm = exportKey,
        salt = HKDF_SALT_BYTES,
        info = info,
        length = DEVICE_ROOT_KEY_LEN,
    )
}

// ============================================================================
// HKDF-SHA-512 (RFC 5869) — minimal, dep-free implementation
// ============================================================================

/**
 * RFC 5869 HKDF — extract-then-expand, hand-rolled over
 * `javax.crypto.Mac.HmacSHA512`. The whole construction is ~50
 * lines and audited against the RFC test vectors in
 * `DeviceKeyTest`; pulling Bouncy Castle in for just this would
 * be ~3 MB of bytecode for a 30-line function.
 *
 * @param ikm     Input keying material — the OPAQUE export key.
 * @param salt    Non-secret randomness; constant across the
 *                whole app per [HKDF_SALT_BYTES].
 * @param info    Domain-separation tag.
 * @param length  Desired output length in bytes. Per RFC 5869
 *                ≤ 255 × HashLen (= 16 320 bytes for SHA-512);
 *                we always ask for 32 here.
 */
internal fun hkdfSha512(
    ikm: ByteArray,
    salt: ByteArray,
    info: ByteArray,
    length: Int,
): ByteArray {
    require(length in 1..(255 * HMAC_SHA_512_LEN)) {
        "HKDF output length out of range"
    }
    // Step 1: extract — PRK = HMAC(salt, IKM).
    val prk = hmacSha512(salt, ikm)

    // Step 2: expand — T(i) = HMAC(PRK, T(i-1) || info || i).
    val out = ByteArray(length)
    var prev = ByteArray(0)
    var written = 0
    var counter = 1
    while (written < length) {
        val mac = Mac.getInstance("HmacSHA512")
        mac.init(SecretKeySpec(prk, "HmacSHA512"))
        mac.update(prev)
        mac.update(info)
        mac.update(byteArrayOf(counter.toByte()))
        val t = mac.doFinal()
        val take = minOf(t.size, length - written)
        System.arraycopy(t, 0, out, written, take)
        written += take
        prev = t
        counter += 1
    }
    return out
}

/** HMAC-SHA-512 helper. Returns a fresh 64-byte tag. */
private fun hmacSha512(key: ByteArray, message: ByteArray): ByteArray {
    val mac = Mac.getInstance("HmacSHA512")
    // RFC 2104 / 5869: an all-zero salt of HashLen bytes is the
    // canonical "no salt" value. `javax.crypto.Mac` rejects a
    // zero-length key; substitute the zero-byte default here so
    // the function still produces RFC-conformant output for
    // empty-salt callers (we don't hit that path in production,
    // but the test vectors do).
    val keySpec = if (key.isEmpty()) {
        SecretKeySpec(ByteArray(HMAC_SHA_512_LEN), "HmacSHA512")
    } else {
        SecretKeySpec(key, "HmacSHA512")
    }
    mac.init(keySpec)
    return mac.doFinal(message)
}
