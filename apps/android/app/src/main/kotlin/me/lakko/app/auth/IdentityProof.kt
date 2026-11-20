// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import uniffi.lakko_ffi_uniffi.Identity

/**
 * ADR-0098 F3 — the identity proof auth requires wherever it records the
 * account's identity key (`POST /auth/genesis/attest`,
 * `/account/keys/reset/finish`, `/account/keys/rotate/finish`,
 * `/account/recall/finish`): the key being recorded signs the account id and
 * itself (`signIdentityProof`), so no account can record a key it does not
 * hold. [phraseSeed] is the 64-byte phrase seed the account identity is made
 * from; the result is the 64-byte signature.
 *
 * Auth answers `400 identity-proof-invalid` or `409 identity-key-taken`
 * when it refuses one; both take the call's ordinary failure path.
 */
internal fun identityProofFor(phraseSeed: ByteArray, accountId: ByteArray): ByteArray =
    Identity.fromSeed(phraseSeed).use { uniffi.lakko_ffi_uniffi.signIdentityProof(it, accountId) }
