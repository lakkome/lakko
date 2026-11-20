// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import android.content.Context
import java.util.Base64
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokens
import me.lakko.app.devices.sync.hexToBytesOrNull
import uniffi.lakko_ffi_uniffi.FfiEvidenceItem

/** The pinned moderation root key (`BuildConfig.LAKKO_MODERATION_ROOT_KEY`, base64url), or `null`. */
public fun pinnedModerationRootKey(pinned: String = me.lakko.app.BuildConfig.LAKKO_MODERATION_ROOT_KEY): ByteArray? {
    val text = pinned.trim().takeIf { it.isNotEmpty() } ?: return null
    return try {
        Base64.getUrlDecoder().decode(text.trimEnd('=')).takeIf { it.size == ROOT_KEY_BYTES }
    } catch (_: IllegalArgumentException) {
        null
    }
}

private const val ROOT_KEY_BYTES: Int = 32

/** The evidence items of a report: the picked messages that still hold their bytes, then the rest. */
public fun evidenceItems(
    picked: List<ReportableMessage>,
    other: List<ReportOtherEvidence>,
    pickedPosts: List<ReportablePost> = emptyList(),
    channelIdHex: String? = null,
): List<FfiEvidenceItem> {
    val messages = picked.mapNotNull { m ->
        val e = m.evidence ?: return@mapNotNull null
        val groupId = hexToBytesOrNull(m.groupIdHex) ?: return@mapNotNull null
        FfiEvidenceItem.Message(groupId = groupId, plaintext = e.plaintext, leafKey = e.leafKey, binding = e.binding)
    }
    val rest = other.map { o ->
        when (o) {
            is ReportOtherEvidence.ContactRequest -> FfiEvidenceItem.ContactRequest(o.wire)
            is ReportOtherEvidence.ChannelPost -> FfiEvidenceItem.ChannelPost(o.channelId, o.post)
            is ReportOtherEvidence.Listing -> FfiEvidenceItem.Listing(o.wire)
        }
    }
    val channelId = channelIdHex?.let(::hexToBytesOrNull)?.takeIf { it.size == CHANNEL_ID_BYTES }
    val posts = if (channelId == null) {
        emptyList()
    } else {
        pickedPosts.mapNotNull { p -> p.wire?.let { FfiEvidenceItem.ChannelPost(channelId, it) } }
    }
    return (messages + rest + posts).take(REPORT_MESSAGE_LIMIT)
}

/** The report's target bytes (ADR-0099 section 2). */
internal suspend fun targetBytes(target: ReportTargetRef): ByteArray? = when (target) {
    is ReportTargetRef.Account -> accountKeyBytes(target.accountKeyB64)
        ?.let { uniffi.lakko_ffi_uniffi.moderationAccountTarget(it) }
    // The group's @name, as the client looks one up: the OPRF lookup key.
    is ReportTargetRef.GroupHandle -> me.lakko.app.discovery.DiscoveryClient().use {
        it.oprfUsername(target.handle.removePrefix("@")).lookupKey
    }
    is ReportTargetRef.Channel -> hexToBytesOrNull(target.channelIdHex)?.takeIf { it.size == CHANNEL_ID_BYTES }
}

private const val CHANNEL_ID_BYTES: Int = 16

/** [ReportPorts] in production: the moderation service, the token pool, the core. */
public class ProductionReportPorts(
    private val context: Context,
    private val subject: ReportSubject,
    private val sheet: ReportSheetState,
    private val client: ModerationClient = ModerationClient(),
) : ReportPorts {
    private val versions = ModeratorListVersionStore(context)

    override fun rootKey(): ByteArray? = pinnedModerationRootKey()

    override suspend fun fetchModeratorList(): ModeratorListFetch = client.moderatorList().also {
        // ADR-0099 F2: the standing proofs reuse the list the sheet fetched.
        if (it is ModeratorListFetch.Ok) ModeratorListCache.shared.offer(it.bytes)
    }

    override fun moderatorListVersion(list: ByteArray, rootKey: ByteArray): ULong =
        uniffi.lakko_ffi_uniffi.moderatorListVersion(list, rootKey)

    override fun highestListVersion(): ULong? = versions.highest()

    override fun recordListVersion(version: ULong) {
        versions.record(version)
    }

    override suspend fun takeToken(): String = AnonTokens.shared.take(AnonTokenContext.REPORT).headerValue

    override suspend fun seal(list: ByteArray, rootKey: ByteArray): ByteArray {
        val target = targetBytes(subject.target) ?: error("report target not derivable")
        val reason = sheet.reason ?: error("no reason")
        return uniffi.lakko_ffi_uniffi.sealModerationReport(
            targetKind = subject.targetKind.wire,
            target = target,
            reason = reason,
            conversationKind = subject.conversationKind.wire,
            comment = sheet.comment,
            evidence = evidenceItems(sheet.picked, subject.other, sheet.pickedPosts, subject.pickChannelIdHex),
            moderatorList = list,
            rootKey = rootKey,
        )
    }

    override suspend fun post(report: ByteArray, tokenHeaderValue: String): ReportPost =
        client.postReport(report, AnonTokens.shared.headerName, tokenHeaderValue)
}

/** Keep a sent report in "Your reports" (this device only). */
public fun recordSentReport(context: Context, subject: ReportSubject, sheet: ReportSheetState, receiptId: String) {
    val account = BlockRegister.state.value?.accountIdB64 ?: return
    val store = SharedPrefsSentReportStore(context)
    val report = SentReport(
        receiptId = receiptId,
        targetName = subject.name,
        reason = (sheet.reason ?: return).toInt(),
        sentAtMs = System.currentTimeMillis(),
    )
    store.save(account, withSentReport(store.load(account), report))
}
