// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// Device root key derivation.
///
/// Mirror of:
///
///   * `apps/web/src/auth/device-key.ts` (1.17d5)
///   * `apps/android/.../auth/DeviceKey.kt` (1.15d3)
///
/// Same HKDF construction, same label, same output length — a user
/// signing up in the web client and re-installing on iOS will produce
/// the **same** device root key from the same password, because the
/// OPAQUE export key is deterministic across cipher-suite-compatible
/// implementations.
///
/// Inputs and outputs:
///
///   * **Input** — the OPAQUE password export key (64 bytes, fresh
///     out of `OpaqueRegister.finish` / `OpaqueLogin.finish`).
///   * **Output** — a 32-byte AES-256 key. Caller MUST hand it to
///     ``DeviceKeyStore/putDeviceKey(accountIdB64:rootKey:)`` and
///     drop the reference; ``Data`` does not zeroize on `deinit`,
///     but its lifetime is bounded to the calling scope which keeps
///     the heap exposure short.
///
/// Derivation, identical to the web + Android sides:
///
/// ```text
///   IKM    = export_key             // 64 bytes
///   salt   = "lakko-device-root-v1" // ASCII-only, fixed
///   info   = "lakko/device-root/v1|" || account_id_b64
///   length = 32 bytes (AES-256)
/// ```
///
/// ## Why CryptoKit and not a hand-rolled HMAC loop?
///
/// CryptoKit ships `HKDF<SHA512>` natively since iOS 14 — same
/// RFC 5869 construction the Android side hand-rolls over
/// `javax.crypto.Mac.HmacSHA512` (because Android's
/// `KeyAgreement` HKDF surfaces in `AndroidKeyStore` ship
/// inconsistent vendor behaviour). On iOS the path is native + audited
/// + byte-identical to RFC test vectors. No reason to hand-roll.
///
/// ## Tests
///
/// Host-side tests at `apps/ios/Tests/LakkoTests/Auth/DeviceKeyTests.swift`
/// land alongside the SwiftUI screens in 1.14d3+ — they exercise
/// determinism, account-id binding, and a cross-platform vector
/// match against the Android `DeviceKeyTest` output.

/// Fixed HKDF salt — must match `apps/web` and `apps/android`.
private let hkdfSaltBytes: Data = Data("lakko-device-root-v1".utf8)

/// Fixed HKDF info-prefix; the `account_id_b64` string is appended.
private let hkdfInfoPrefix: String = "lakko/device-root/v1|"

/// AES-256 — the output of the derivation, ready to drive AES-GCM.
public let deviceRootKeyLength: Int = 32

/// Derive the 32-byte device root key for a given account.
///
/// - Parameters:
///   - exportKey:    OPAQUE password export key bytes (64 bytes for
///                   `LakkoCipherSuite`). Caller is responsible for
///                   discarding any retained reference — the
///                   function does not keep one.
///   - accountIdB64: Base64-encoded raw 16-byte UUIDv7 account id.
///                   Mixed into the HKDF `info` so two accounts on
///                   the same device derive different keys.
/// - Returns: Fresh 32-byte AES-256 key. **Treat as secret**: hand
///   to ``DeviceKeyStore/putDeviceKey(accountIdB64:rootKey:)``
///   immediately.
public func deriveDeviceRootKey(
    exportKey: Data,
    accountIdB64: String
) -> Data {
    let info = Data((hkdfInfoPrefix + accountIdB64).utf8)
    let ikm = SymmetricKey(data: exportKey)
    let derived = HKDF<SHA512>.deriveKey(
        inputKeyMaterial: ikm,
        salt: hkdfSaltBytes,
        info: info,
        outputByteCount: deviceRootKeyLength
    )
    return derived.withUnsafeBytes { Data($0) }
}

/// Fixed HKDF info-prefix for the IDENTITY-SEED-derived variant —
/// must match `apps/web/src/auth/device-key.ts` and
/// `apps/android/.../auth/DeviceKey.kt`. Distinct from
/// `hkdfInfoPrefix` so the two derivations can never collide.
private let hkdfInfoPrefixFromIdentitySeed: String =
    "lakko/device-root/from-identity-seed/v1|"

/// Derive the 32-byte device root key from the ACCOUNT-IDENTITY
/// (BIP39 phrase) seed instead of the OPAQUE export key — ADR-0018
/// factor (a), path (b): a device restored via another device has no
/// password (hence no export key) yet, but the rehydration gate
/// (``DeviceKeyStore`` existence check in `AuthRepository.rehydrate`)
/// still needs a key to exist. Same HKDF shape as
/// ``deriveDeviceRootKey(exportKey:accountIdB64:)`` with a distinct
/// domain-separated info string (mirror of the web's
/// `deriveDeviceRootKeyFromIdentitySeed` + Android's twin).
///
/// - Parameters:
///   - accountIdentitySeed: 64-byte BIP39 phrase seed. Caller wipes.
///   - accountIdB64:        Base64 16-byte account id, mixed into
///                          the HKDF `info`.
public func deriveDeviceRootKeyFromIdentitySeed(
    accountIdentitySeed: Data,
    accountIdB64: String
) -> Data {
    let info = Data((hkdfInfoPrefixFromIdentitySeed + accountIdB64).utf8)
    let ikm = SymmetricKey(data: accountIdentitySeed)
    let derived = HKDF<SHA512>.deriveKey(
        inputKeyMaterial: ikm,
        salt: hkdfSaltBytes,
        info: info,
        outputByteCount: deviceRootKeyLength
    )
    return derived.withUnsafeBytes { Data($0) }
}
