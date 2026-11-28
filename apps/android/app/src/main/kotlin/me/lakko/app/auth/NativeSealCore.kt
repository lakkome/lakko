// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import uniffi.lakko_ffi_uniffi.FfiSealAnnouncement
import uniffi.lakko_ffi_uniffi.FfiSealEntry
import uniffi.lakko_ffi_uniffi.FfiSealKind
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic
import uniffi.lakko_ffi_uniffi.ResetSealKey

/**
 * [SealCore] over the native core (`crates/lakko-ffi-uniffi/src/reset_seal.rs`,
 * ADR-0101 section 10). Nothing here decides anything: it converts between
 * this layer's views and the generated records.
 */
public object NativeSealCore : SealCore {

    private fun keyOf(secret: SealSecret): ResetSealKey = when (secret) {
        is SealSecret.Password -> ResetSealKey.fromPasswordExportKey(secret.exportKey)
        is SealSecret.Passkey -> ResetSealKey.fromPasskeyPrf(secret.prfOutput)
        is SealSecret.Phrase -> RecoveryMnemonic.fromPhrase(secret.phrase).use { ResetSealKey.fromMnemonic(it) }
    }

    private fun credentialIdOf(secret: SealSecret): ByteArray? = (secret as? SealSecret.Passkey)?.credentialId

    override fun entryOf(secret: SealSecret): SealEntryView =
        keyOf(secret).use { viewOf(it.announcementEntry(credentialIdOf(secret))) }

    override fun passkeyTagHex(credentialId: ByteArray): String =
        hex(uniffi.lakko_ffi_uniffi.resetSealPasskeyTag(credentialId))

    override fun open(announcement: ByteArray, accountKey: ByteArray): SealAnnouncementView? = try {
        viewOf(uniffi.lakko_ffi_uniffi.openSealAnnouncement(announcement, accountKey))
    } catch (_: Throwable) {
        null
    }

    override fun decodeUnverified(announcement: ByteArray): SealAnnouncementView? = try {
        viewOf(uniffi.lakko_ffi_uniffi.decodeSealAnnouncement(announcement))
    } catch (_: Throwable) {
        null
    }

    override fun signAnnouncement(
        accountSeed: ByteArray,
        accountId: ByteArray,
        keyGeneration: Long,
        seq: Long,
        issuedAtMs: Long,
        entries: List<SealEntryView>,
    ): ByteArray = Identity.fromSeed(accountSeed).use { identity ->
        uniffi.lakko_ffi_uniffi.signSealAnnouncement(
            identity,
            accountId,
            keyGeneration.toULong(),
            seq.toULong(),
            issuedAtMs.toULong(),
            entries.map(::ffiOf),
        )
    }

    override fun signResetSeal(
        secret: SealSecret,
        accountId: ByteArray,
        announcementSeq: Long,
        fromGeneration: Long,
        newSignaturePubkey: ByteArray,
    ): ByteArray = keyOf(secret).use {
        it.signResetSeal(accountId, announcementSeq.toULong(), fromGeneration.toULong(), newSignaturePubkey)
    }

    private fun viewOf(a: FfiSealAnnouncement): SealAnnouncementView = SealAnnouncementView(
        keyGeneration = a.keyGeneration.toLong(),
        seq = a.seq.toLong(),
        entries = a.entries.map(::viewOf),
        issuedAtMs = a.issuedAtMs.toLong(),
    )

    private fun viewOf(e: FfiSealEntry): SealEntryView = SealEntryView(
        kind = when (e.kind) {
            FfiSealKind.PASSWORD -> SealKindName.PASSWORD
            FfiSealKind.PASSKEY -> SealKindName.PASSKEY
            FfiSealKind.PHRASE -> SealKindName.PHRASE
        },
        tagHex = hex(e.tag),
        sealPkB64 = base64Encode(e.sealPk),
    )

    private fun ffiOf(e: SealEntryView): FfiSealEntry = FfiSealEntry(
        kind = when (e.kind) {
            SealKindName.PASSWORD -> FfiSealKind.PASSWORD
            SealKindName.PASSKEY -> FfiSealKind.PASSKEY
            SealKindName.PHRASE -> FfiSealKind.PHRASE
        },
        tag = unhex(e.tagHex),
        sealPk = base64Decode(e.sealPkB64),
    )

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    private fun unhex(text: String): ByteArray =
        ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
