// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Base64
import me.lakko.app.chat.GroupNotAcceptingRequests
import me.lakko.app.chat.JournalHeadEpoch
import me.lakko.app.chat.runLinkJoinKnock
import me.lakko.app.contacts.MailboxCrypto
import me.lakko.app.contacts.MailboxRowKind
import me.lakko.app.contacts.NativeMailboxCrypto
import me.lakko.app.contacts.NativeOwnMailbox
import me.lakko.app.contacts.sealFor
import me.lakko.app.contacts.verifiedGrantDest
import me.lakko.app.contacts.welcomeBodyOf
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FfiDsJournalSigner
import uniffi.lakko_ffi_uniffi.dsMailboxSigner
import uniffi.lakko_ffi_uniffi.GroupInviteCapability
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.composeCoPublisherInviteCapability
import uniffi.lakko_ffi_uniffi.composeGroupJoinAttribution
import uniffi.lakko_ffi_uniffi.composeChannelCopublisherBinding
import uniffi.lakko_ffi_uniffi.decodeChannelCopublisherBinding
import uniffi.lakko_ffi_uniffi.publisherGroupLoad
import uniffi.lakko_ffi_uniffi.publisherGroupLoadJoined

/**
 * ADR-0028 OQ3 — channel co-publisher management (owner + joiner side, Android).
 *
 * Kotlin mirror of `apps/web/src/channels/copublisher.ts` and
 * `apps/ios/Lakko/Channels/CoPublisher.swift`. A channel's PublisherGroup
 * (ADR-0010 Tier 1) is a vanilla MLS group, so adding a *co-publisher* reuses
 * the group join-ticket + external-join machinery (ADR-0027/0028) verbatim —
 * exactly what the OQ3 FFI surface on [PublisherGroup] exposes.
 *
 * **Reseal-after-add (load-bearing).** `approveExternalJoin` commits the Add,
 * which advances the PublisherGroup MLS epoch — so the channel content-key seed
 * (the epoch-keyed exporter output) CHANGES. Every existing subscriber holds the
 * OLD seed, so the owner MUST reseal the new seed to all subscribers (the
 * ADR-0010 / C-54 rotation fanout) or those subscribers can no longer decrypt.
 * [approve] does this through [resealToSubscribers] — the same staged ADR-0030
 * fanout as [RotationCron], but WITHOUT a second `rotate()` (the Add already
 * advanced the epoch; the reseal binds to the post-approve epoch the
 * co-publisher joins at).
 *
 * **G-50.** The approve is recorded ([PublisherCommitEntry]) with the N+1
 * provider snapshot before any of its effects leaves the device, and the
 * effects resume from that record ([replayPublisherCommits]).
 *
 * **ADR-0078 section 6.** A PublisherGroup id is derived from the owner's
 * account seed and the channel id, so only the owner computes it; a
 * co-publisher opens the group under the id its link conveyed (recorded per
 * channel in [CoPublishedJournalMarks]), and the link carries the channel id
 * in its sealed preview.
 *
 * The end-to-end correctness of the reseal under a real 2-publisher topology
 * needs a live 2-publisher e2e against a deployed channels backend — deferred
 * (no deployed environment), the same posture as every other live-channel gate.
 */
public object CoPublisher {

    /** ADR-0078 - the binding check and sealing of mailbox rows; a test
     *  injects a fake (the host JVM loads no native library). */
    @Volatile
    internal var mailboxCrypto: MailboxCrypto = NativeMailboxCrypto
    /** Path prefix every `/g/` co-publisher invite URL starts with. */
    public const val URL_PREFIX: String = "/g/"

    /** Default origin Lakko shares its invite links under: the web origin of
     *  the stack this build talks to ([me.lakko.app.config.ApiEndpoints.webOrigin]). */
    public val DEFAULT_ORIGIN: String
        get() = me.lakko.app.config.ApiEndpoints.webOrigin

    /** Errors surfaced by the co-publisher flow. */
    public sealed class CoPublisherException(msg: String) : Exception(msg) {
        public class BadChannelId(hex: String) :
            CoPublisherException("channel id hex is not valid: $hex")

        public class UnexpectedServerEpoch(epoch: UInt) :
            CoPublisherException("server epoch wraparound at $epoch — cannot reseal")

        public class MalformedSubscriberHex(hex: String) :
            CoPublisherException("subscriber pubkey hex is not valid: $hex")

        public class NotACoPublisherLink(link: String) :
            CoPublisherException("not a /g/ co-publisher link: $link")

        public object NotAChannelGroup :
            CoPublisherException("co-publisher link does not target a channel PublisherGroup")
    }

    // -----------------------------------------------------------------
    // Owner side
    // -----------------------------------------------------------------

    /** Open the persistent PublisherGroup handle for a channel. The caller
     *  owns the returned handle and MUST `.close()` it when done. ADR-0090:
     *  the group lives on this device's bound identity ([ChannelIdentity]).
     *  A co-published channel ([coPublished]) is opened under the group id
     *  its link conveyed and never founded here (ADR-0078 section 6). An
     *  owned channel is founded only when [allowFound] (New channel, and the
     *  re-founding of ADR-0090 decision A); everywhere else a miss throws
     *  [ChannelsComposerException.NotPublishedHere]: this device has no leaf
     *  and reads until another device of the account admits it
     *  (docs/ux/second-device.md section 4a). */
    public suspend fun openPublisherGroup(
        ids: ChannelIdentity,
        channelIdHex: String,
        coPublished: CoPublishedJournalMarks? = null,
        allowFound: Boolean = false,
    ): PublisherGroup {
        val joined = coPublished?.journalIdOf(channelIdHex)?.let(::hexToBytes)
        return try {
            openPublisherGroupOn(ids, channelIdHex, joined, allowFound)
                ?: when (publisherGroupOpenMode(joined = joined != null, allowFound = allowFound)) {
                    PublisherGroupOpenMode.LOAD_JOINED -> throw CoPublisherException.NotAChannelGroup
                    else -> throw ChannelsComposerException.NotPublishedHere()
                }
        } catch (e: Throwable) {
            // A channel minted under the retired v1 derivation refuses here and
            // cannot be repaired — surface the readable copy, not the raw string.
            throw LegacyChannelV1.mapOpenError(e)
        }
    }

    /** The channel's PublisherGroup on this device, or `null` (no leaf
     *  here). A co-published channel is opened under the group id its link
     *  conveyed, an owned one under the owner's derivation (ADR-0078 section
     *  6). Never creates. The caller owns the returned handle. */
    public suspend fun loadPublisherGroup(
        ids: ChannelIdentity,
        channelIdHex: String,
        coPublished: CoPublishedJournalMarks? = null,
    ): PublisherGroup? =
        openPublisherGroupOn(ids, channelIdHex, coPublished?.journalIdOf(channelIdHex)?.let(::hexToBytes), allowFound = false)

    /**
     * ADR-0090 section 1 - the one place a PublisherGroup is opened: a
     * co-published channel by [joinedGroupId] (`publisherGroupLoadJoined`),
     * an owned one by the owner account seed (`publisherGroupLoad`), and,
     * only with [allowFound], founded (`PublisherGroup(...)`, New channel and
     * decision A). `null` when this device holds no group for it.
     */
    public suspend fun openPublisherGroupOn(
        ids: ChannelIdentity,
        channelIdHex: String,
        joinedGroupId: ByteArray?,
        allowFound: Boolean,
    ): PublisherGroup? {
        val channelId = hexToBytes(channelIdHex)
            ?: throw CoPublisherException.BadChannelId(channelIdHex)
        if (joinedGroupId != null) {
            return ids.mls { device -> publisherGroupLoadJoined(device, joinedGroupId, channelId) }
        }
        val ownerSeed = ids.ownerAccountSeed()
        try {
            return ids.mls { device ->
                publisherGroupLoad(device, ownerSeed, channelId)
                    ?: if (allowFound) PublisherGroup.create(device, ownerSeed, channelId) else null
            }
        } finally {
            ownerSeed.fill(0)
        }
    }

    /** Mint a co-publisher invite capability over the PublisherGroup's MLS
     *  `group_id`, sealed under a fresh link secret. A non-member opens the
     *  link, requests to join, and the owner approves (reusing the
     *  ADR-0027/0028 external-join machinery). `0` = never expires.
     *  ADR-0078 section 6 — the joiner cannot recover the channel id from the
     *  group id, so the capability's sealed preview carries it (and the
     *  channel's name when known). Upload it with [uploadCoPublisherLink],
     *  outside the identity lock. */
    public fun mintCoPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        channelName: String? = null,
        /** Phase B — who invites, shown on the link page and the card. */
        inviterName: String? = null,
        avatar: uniffi.lakko_ffi_uniffi.FfiAvatar? = null,
    ): uniffi.lakko_ffi_uniffi.InviteLinkMint {
        // ADR-0087 section 5 — the link names the PublisherGroup's knock key,
        // derived from the owner's journal root seed (version 0, as for a
        // group). Phase B moves the co-publisher join onto the sealed row.
        val groupId = publisherGroup.groupId()
        val root = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(identity.ed25519Seed(), groupId)
        val knockPublicKey = try {
            val seed = uniffi.lakko_ffi_uniffi.groupKnockSeed(root, me.lakko.app.chat.CURRENT_KNOCK_VERSION)
            try {
                uniffi.lakko_ffi_uniffi.groupKnockPublicKey(seed)
            } finally {
                seed.fill(0)
            }
        } finally {
            root.fill(0)
        }
        return composeCoPublisherInviteCapability(
            identity.ed25519Seed(),
            groupId,
            0uL,
            publisherGroup.channelId(),
            channelName?.takeIf { it.isNotBlank() },
            knockPublicKey,
            me.lakko.app.chat.CURRENT_KNOCK_VERSION,
            avatar,
            inviterName?.takeIf { it.isNotBlank() },
        )
    }

    /** Upload a minted co-publisher link to the group-invite ledger like any
     *  `/g/` link and answer `<origin>/g/<id>#<secret>`. It was always the
     *  inline form until 2026-10-19, when the inline form was removed: any
     *  failed upload, a network one included, throws and mints no link. The
     *  link's revocation seed is kept in this device's record of its links
     *  for [accountIdB64], as for a group link. No use limit: the owner's
     *  approval is the boundary. */
    public suspend fun uploadCoPublisherLink(
        sealed: ByteArray,
        linkSecret: ByteArray,
        discovery: me.lakko.app.discovery.DiscoveryClient,
        accountIdB64: String?,
        origin: String = DEFAULT_ORIGIN,
        /** The ledger's use ceiling: `0` one use, `0xFFFFFFFF` none. A
         *  publisher link ([LinkRecordKind.PUBLISHER]) is always one use,
         *  whatever is passed: it names one person (UX 15.2). */
        maxUses: Long = 0L,
        /** Section 14.2 — when the link stops working (unix ms), `0` never. */
        expiryMs: Long = 0L,
        /** Phase B (item 8) — how the link is filed in the account's synced
         *  link register, so every device of the account lists and revokes
         *  it; `null` files nothing there. */
        register: LinkRegisterEntry? = null,
        /** Told the ledger id the row got (an invitation card names it). */
        onUploaded: (blobId: ByteArray) -> Unit = {},
    ): String {
        val (seed, revocationPubkey) = me.lakko.app.contacts.OwnLinks.freshRevocationKey()
        @Suppress("NAME_SHADOWING")
        val maxUses = if (register?.kind == uniffi.lakko_ffi_uniffi.LinkRecordKind.PUBLISHER) 0L else maxUses
        try {
            val blobId = discovery.uploadGroupInvite(
                me.lakko.app.discovery.SealedInviteUpload(
                    sealed = sealed,
                    revocationPubkey = revocationPubkey,
                    expiryMs = expiryMs.coerceAtLeast(0L),
                    maxUses = maxUses,
                ),
            )
            if (accountIdB64 != null) {
                me.lakko.app.contacts.OwnLinkStores.current.put(
                    accountIdB64,
                    me.lakko.app.contacts.OwnLinks.record(
                        kind = me.lakko.app.contacts.OwnLink.KIND_GROUP,
                        blobId = blobId,
                        seed = seed,
                        maxUses = maxUses,
                    ),
                )
            }
            onUploaded(blobId)
            register?.let {
                me.lakko.app.channels.GroupLinkRegister.record(
                    entry = it,
                    ledgerId = blobId,
                    linkSecret = linkSecret,
                    revocationSeed = seed,
                    capability = sealed,
                    maxUses = maxUses,
                    expiryMs = expiryMs,
                )
            }
            return me.lakko.app.discovery.composeSealedInviteUrl(
                origin = origin,
                prefix = URL_PREFIX,
                blobId = blobId,
                linkSecret = linkSecret,
            )
        } finally {
            seed.fill(0)
        }
    }

    /** Poll the PublisherGroup's DS journal and apply incoming MLS messages
     *  (commits of an owner device, the publishers' application messages)
     *  through the gated receive (ADR-0090: a commit whose committer is not a
     *  device of the owner's account is refused, never merged). Surfaces
     *  external-join requests into the retained pending set from the knock
     *  queue (ADR-0053). Non-MLS rows and own already-merged commits throw and
     *  are skipped. Returns how many entries were applied, the pending
     *  requests after the poll, and the DS time of the newest applied commit
     *  that moved the epoch (the start of the channel epoch, section 4). */
    public suspend fun pollJournal(
        ds: DsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        /** ADR-0087 section 7 — a retained request's joiner ACCOUNT key (its
         *  KeyPackage's binding) and the account id its profile carried. */
        onRequest: (joinerAccountKey: ByteArray, accountId: ByteArray?) -> Unit = { _, _ -> },
        /** The journal row id already read (0 reads from the start). */
        after: Long = 0L,
        /** The knock row id already read, or `null` to skip the knocks. */
        knockAfter: Long? = 0L,
    ): JournalPoll {
        val groupId = publisherGroup.groupId()
        var cursor = after
        var applied = 0
        val applications = ArrayList<ByteArray>()
        val senders = ArrayList<ByteArray?>()
        var lastCommitAtMs: Long? = null
        var epochMoved = false
        var latestEpoch: ULong? = null
        while (true) {
            val raw = ds.fetchGroup(groupId, after = cursor, waitSecs = 0, signer = signerOf(publisherGroup, ids))
            raw.latestEpoch?.let { latestEpoch = it }
            for (m in raw.messages) {
                try {
                    val (processed, moved) = ids.mls { device ->
                        val before = publisherGroup.epoch()
                        val p = publisherGroup.processIncomingVerifyingRoles(device, m.payload)
                        p to (publisherGroup.epoch() != before)
                    }
                    if (processed is uniffi.lakko_ffi_uniffi.ProcessedMessage.Application) {
                        applications += processed.`data`
                        // The account the MLS sender resolves to at receipt.
                        senders += processed.senderAccount?.takeIf { it.size == 32 }
                    }
                    if (moved) {
                        epochMoved = true
                        lastCommitAtMs = isoMillis(m.createdAt) ?: lastCommitAtMs
                    }
                    applied += 1
                } catch (_: Throwable) {
                    // A commit we have already merged (epoch mismatch), a commit
                    // the ADR-0090 gate refused, or a non-MLS row — skip.
                }
                cursor = maxOf(cursor, m.id)
            }
            if (!raw.hasMore || raw.messages.isEmpty()) break
        }
        var knockCursor = knockAfter
        // ADR-0087 section 5 (phase B) — a prospective co-publisher knocks a
        // SEALED row: a fresh KeyPackage of its device, a reply grant and the
        // /g/ capability, sealed to the PublisherGroup's knock key (the
        // owner's journal root over the group id, as the link names it). The
        // co-publisher key binding rides beside it in the same knock.
        if (knockAfter != null) try {
            val knocks = ds.fetchKnocks(groupId, after = knockAfter, waitSecs = 0, signer = signerOf(publisherGroup, ids))
            val channelId = publisherGroup.channelId()
            for (row in knocks.messages) {
                knockCursor = maxOf(knockCursor ?: 0L, row.id)
                val bytes = row.payload
                val version = uniffi.lakko_ffi_uniffi.knockRowVersion(bytes)
                if (version == null) {
                    // ADR-0059 — not a sealed row: perhaps the binding.
                    recordCoPublisherBinding(groupId, channelId, bytes)
                    continue
                }
                val root = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(ids.account.ed25519Seed(), groupId)
                val request = try {
                    val seed = uniffi.lakko_ffi_uniffi.groupKnockSeed(root, version)
                    try {
                        uniffi.lakko_ffi_uniffi.openKnockRow(seed, bytes)
                    } finally {
                        seed.fill(0)
                    }
                } catch (_: Throwable) {
                    null
                } finally {
                    root.fill(0)
                } ?: continue
                // Only a request that came by a co-publisher link of THIS group.
                val byLink = request.capability?.let { capBytes ->
                    try {
                        GroupInviteCapability.decodeAndVerify(capBytes).use { it.groupId().contentEquals(groupId) }
                    } catch (_: Throwable) {
                        false
                    }
                } == true
                if (!byLink) continue
                // ADR-0090 — the joiner's leaf is a bound device: the account
                // it belongs to is its KeyPackage's binding. An unbound one is
                // never approved (the core refuses it).
                val joinerAccount = try {
                    uniffi.lakko_ffi_uniffi.keyPackageAccountKey(request.keyPackage)
                } catch (_: Throwable) {
                    null
                }?.takeIf { it.size == 32 } ?: continue
                val kept = try {
                    ids.mls { device -> publisherGroup.retainJoinRequest(device, request.keyPackage) }
                } catch (_: Throwable) {
                    knockCursor = row.id - 1
                    break
                }
                when (kept.outcome) {
                    uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.QUEUE_FULL -> {
                        knockCursor = row.id - 1
                        break
                    }
                    uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.RETAINED -> {
                        applied += 1
                        keepJoiner(groupId, kept.joinerSignatureKey, joinerAccount, request.capability, request.replyGrant)
                        onRequest(joinerAccount, request.profile?.accountId)
                    }
                    uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.ALREADY_PENDING ->
                        keepJoiner(groupId, kept.joinerSignatureKey, joinerAccount, request.capability, request.replyGrant)
                    else -> Unit
                }
            }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            // The knock read is best effort; the next poll retries.
        }
        return JournalPoll(
            applied = applied,
            pending = ids.mls { publisherGroup.pendingExternalJoins().size },
            applications = applications,
            senders = senders,
            journalCursor = cursor,
            knockCursor = knockCursor,
            lastCommitAtMs = lastCommitAtMs,
            latestEpoch = latestEpoch,
            epochMoved = epochMoved,
        )
    }

    private fun keepJoiner(
        groupId: ByteArray,
        joinerKey: ByteArray,
        joinerAccount: ByteArray,
        capability: ByteArray?,
        replyGrant: uniffi.lakko_ffi_uniffi.FfiReachGrant,
    ) {
        keepCapability(groupId, joinerKey, capability)
        joinerAccounts[me.lakko.app.reach.reachHex(joinerKey)] = joinerAccount
        keepReplyGrant(joinerAccount, replyGrant)
    }

    /** ADR-0090 — each pending joiner's ACCOUNT key, by its device leaf key
     *  (hex). Process memory; the knock read refills it. */
    private val joinerAccounts = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()

    /** The account a pending joiner's device belongs to, or `null`. */
    public fun joinerAccountOf(joinerKey: ByteArray): ByteArray? = joinerAccounts[me.lakko.app.reach.reachHex(joinerKey)]

    /** The millisecond time of an ISO-8601 DS timestamp, or `null`. */
    internal fun isoMillis(iso: String): Long? =
        try {
            java.time.Instant.parse(iso).toEpochMilli()
        } catch (_: Throwable) {
            null
        }

    /** ADR-0053 — the PublisherGroup journal's signer at its current epoch,
     *  or `null` when the handle cannot derive one. */
    public suspend fun signerOf(publisherGroup: PublisherGroup, ids: ChannelIdentity): DsJournalSigner? =
        try {
            ids.mls { device -> FfiDsJournalSigner(publisherGroup.dsAccessSigner(device)) }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }

    /**
     * ADR-0057 obligations 1 and 7 — the channel owner claims the
     * PublisherGroup journal with its ROOT (derived from the owner account
     * seed, the same on every owner device) at the current epoch: an
     * idempotent re-send of the claim New channel made. A `409` is left alone:
     * a group is founded only in New channel and in the re-founding of
     * ADR-0090 decision A, which reset there. Best effort. A co-publisher
     * ([coPublished]) never claims.
     */
    public suspend fun claimJournal(
        ds: DsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        coPublished: CoPublishedJournalMarks,
    ) {
        val signer = signerOf(publisherGroup, ids) ?: return
        val groupId = publisherGroup.groupId()
        try {
            val outcome = claimPublisherJournal(
                journalId = groupId,
                journalIdHex = DsClient.pseudonymHex(groupId),
                epoch = signer.epoch,
                epochKey = signer.verifyingKey,
                coPublished = coPublished,
                root = { me.lakko.app.ds.FfiDsStatementSigning.root(ids.account, groupId) },
                put = { make -> ds.putStatement(make) },
            )
            android.util.Log.i("CoPublisher", "ADR-0057: publisher journal claim $outcome")
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("CoPublisher", "ADR-0057: publisher journal claim did not land", e)
        }
    }

    /**
     * ADR-0057 second security pass, finding 3 — creating a channel: found its
     * PublisherGroup on this device's leaf and claim the journal with the
     * owner's ROOT, requiring the 204 (a `409` on the fresh founding epoch is
     * a root reset), BEFORE the channel is announced. Throws when the claim
     * does not land.
     *
     * [refound] — ADR-0090 decision A: this device founded the account's
     * DeviceGroup alone, so it founds the PublisherGroup of an owned channel
     * again at the same id: any state it holds under that id is deleted
     * first, and the journal is reset under the same root.
     */
    public suspend fun foundAndClaimAtCreation(
        ds: DsClient,
        ids: ChannelIdentity,
        channelIdHex: String,
        refound: Boolean = false,
    ) {
        if (refound) deleteLocalPublisherGroup(ids, channelIdHex, joinedGroupId = null)
        val publisherGroup = openPublisherGroup(ids, channelIdHex, allowFound = true)
        try {
            val signer = signerOf(publisherGroup, ids)
                ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the channel journal key could not be derived")
            val groupId = publisherGroup.groupId()
            me.lakko.app.ds.requireClaim {
                me.lakko.app.ds.claimOrReset(
                    journalId = groupId,
                    epoch = signer.epoch,
                    epochKey = signer.verifyingKey,
                    statements = me.lakko.app.ds.FfiDsStatementSigning.root(ids.account, groupId),
                    refounded = signer.epoch == 0uL,
                    put = { make -> ds.putStatement(make) },
                )
            }
            // ADR-0064 section 7 — a channel created (or founded again) now
            // has its head from the start. Best effort.
            // Security pass — a re-founding REPLACES the head (the dead
            // group's higher epochs must not survive a merge).
            ChannelVaultHeads.writeCurrent(ids, publisherGroup, replace = refound)
        } finally {
            publisherGroup.close()
        }
    }

    /**
     * ADR-0090 section 2.5 — drop this device's PublisherGroup state for
     * [channelIdHex] (an old owner device whose journal was reset under it,
     * or before a re-founding). `true` when a group was held and deleted.
     */
    public suspend fun deleteLocalPublisherGroup(
        ids: ChannelIdentity,
        channelIdHex: String,
        joinedGroupId: ByteArray?,
    ): Boolean {
        val held = openPublisherGroupOn(ids, channelIdHex, joinedGroupId, allowFound = false) ?: return false
        try {
            ids.mls { device -> held.delete(device) }
        } finally {
            held.close()
        }
        ids.persist()
        return true
    }

    /** What one [pollJournal] pass did. */
    public data class JournalPoll(
        /** Journal entries the group accepted this pass. */
        val applied: Int,
        /** Pending co-publisher requests after the pass. */
        val pending: Int,
        /** Application messages read this pass (an owner's admission seed). */
        val applications: List<ByteArray> = emptyList(),
        /** The account each of [applications] came from, resolved at receipt
         *  (`null` when it did not resolve). */
        val senders: List<ByteArray?> = emptyList(),
        /** The last journal row id read. */
        val journalCursor: Long = 0L,
        /** The knock row id read through, or `null` when knocks were skipped. */
        val knockCursor: Long? = null,
        /** ADR-0090 section 4 — the DS time of the newest applied commit that
         *  moved the epoch this pass, or `null`. */
        val lastCommitAtMs: Long? = null,
        /** The newest epoch the DS has registered for the journal. */
        val latestEpoch: ULong? = null,
        /** ADR-0064 section 7 — a commit applied this pass moved the epoch. */
        val epochMoved: Boolean = false,
    )

    /** The pending co-publisher external-join requests — each a 32-byte joiner
     *  leaf signature key (the `proposalRef` for approve / reject). */
    public suspend fun pending(ids: ChannelIdentity, publisherGroup: PublisherGroup): List<ByteArray> =
        ids.mls { publisherGroup.pendingExternalJoins() }

    /** A PublisherGroup commit of this device lost the race at the DS (`409`):
     *  another owner device committed at that epoch. The staged commit was
     *  abandoned and nothing of it left the device; the journal read applies
     *  the winner. */
    public class PublisherCommitLost : IllegalStateException("another device committed to the channel first")

    /**
     * Approve a pending co-publisher (ADR-0090 section 3: STAGED). The server
     * epoch the reseal binds to is read first; the approve is staged, the
     * entry recorded with the staged snapshot and the epoch-N access seed
     * ([recordCommit]); then the commit route decides: taken, it merges and
     * the Welcome goes to the joiner's mailbox, the new seed is resealed to
     * every subscriber and the joiner's key appointed; `409`, the stage is
     * abandoned and [PublisherCommitLost] thrown.
     */
    public suspend fun approve(
        ds: DsClient,
        channelsClient: ChannelsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        proposalRef: ByteArray,
        channelIdHex: String,
        accountIdB64: String,
        store: PublisherCommitStore,
    ) {
        val joinerAccount = joinerAccountOf(proposalRef)
            ?: throw IllegalStateException("the request's account is not known yet")
        val serverTarget = nextServerEpoch(channelsClient, channelIdHex)
        val entry = recordCommit(ids, publisherGroup, accountIdB64, store) { device ->
            val proofEpoch = publisherGroup.epoch().toLong()
            val proofSeed = publisherGroup.dsAccessSeed(device)
            val added = publisherGroup.stageApproveExternalJoin(device, proposalRef)
            PublisherCommitEntry(
                id = PublisherCommitEntry.newId(),
                kind = PublisherCommitEntry.Kind.APPROVE,
                channelIdHex = channelIdHex.lowercase(),
                groupIdHex = DsClient.pseudonymHex(publisherGroup.groupId()),
                targetEpoch = proofEpoch + 1,
                serverTargetEpoch = serverTarget,
                commitB64 = Base64.encodeToString(added.commit, Base64.NO_WRAP),
                welcomeB64 = Base64.encodeToString(added.welcome, Base64.NO_WRAP),
                joinerKeyB64 = Base64.encodeToString(joinerAccount, Base64.NO_WRAP),
                proofEpoch = proofEpoch,
                proofSeedB64 = Base64.encodeToString(proofSeed, Base64.NO_WRAP),
            ).also { proofSeed.fill(0) }
        }
        val outcome = runPublisherCommit(
            entry,
            commitPorts(ds, channelsClient, ids, publisherGroup, channelIdHex, accountIdB64, store),
        )
        if (outcome == PublisherCommitOutcome.LOST) throw PublisherCommitLost()
    }

    /**
     * ADR-0087 section 15 (phase B), ADR-0090 section 3 — an owner device
     * removes the publisher [accountKey] (every leaf of it), STAGED: recorded,
     * then the commit route; taken, it merges, reseals and withdraws the
     * appointment; `409` throws [PublisherCommitLost].
     */
    public suspend fun removePublisher(
        ds: DsClient,
        channelsClient: ChannelsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        accountKey: ByteArray,
        channelIdHex: String,
        accountIdB64: String,
        store: PublisherCommitStore,
    ) {
        val serverTarget = nextServerEpoch(channelsClient, channelIdHex)
        val entry = recordCommit(ids, publisherGroup, accountIdB64, store) { device ->
            val proofEpoch = publisherGroup.epoch().toLong()
            val proofSeed = publisherGroup.dsAccessSeed(device)
            val commit = publisherGroup.stageRemovePublisher(device, accountKey)
            PublisherCommitEntry(
                id = PublisherCommitEntry.newId(),
                kind = PublisherCommitEntry.Kind.REMOVE,
                channelIdHex = channelIdHex.lowercase(),
                groupIdHex = DsClient.pseudonymHex(publisherGroup.groupId()),
                targetEpoch = proofEpoch + 1,
                serverTargetEpoch = serverTarget,
                commitB64 = Base64.encodeToString(commit, Base64.NO_WRAP),
                joinerKeyB64 = Base64.encodeToString(accountKey, Base64.NO_WRAP),
                proofEpoch = proofEpoch,
                proofSeedB64 = Base64.encodeToString(proofSeed, Base64.NO_WRAP),
            ).also { proofSeed.fill(0) }
        }
        val outcome = runPublisherCommit(
            entry,
            commitPorts(ds, channelsClient, ids, publisherGroup, channelIdHex, accountIdB64, store),
        )
        if (outcome == PublisherCommitOutcome.LOST) throw PublisherCommitLost()
    }

    /**
     * ADR-0090 sections 2.1 / 2.2 — an owner device admits [admit] (wire
     * KeyPackages of devices of an account that already publishes) and retires
     * [retire] (32-byte leaf keys) in ONE staged commit. The Welcome goes to
     * [welcomeAccount]'s mailbox: this account's own (its own `contact` key)
     * or a co-publisher's ([delegationHeader] / [delegationBinding], the
     * committer delegation its request carried, else a held grant). After the
     * merge the new seed is resealed and the account of the added devices is
     * owed the hand-over again (read key, admission seeds, epoch seeds).
     * Returns the outcome; `LOST` when another owner device committed first.
     */
    public suspend fun admitDevices(
        ds: DsClient,
        channelsClient: ChannelsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        admit: List<ByteArray>,
        retire: List<ByteArray>,
        welcomeAccount: ByteArray?,
        channelIdHex: String,
        accountIdB64: String,
        store: PublisherCommitStore,
        delegationHeader: String? = null,
        delegationBinding: ByteArray? = null,
    ): PublisherCommitOutcome {
        val serverTarget = nextServerEpoch(channelsClient, channelIdHex)
        val entry = recordCommit(ids, publisherGroup, accountIdB64, store) { device ->
            val proofEpoch = publisherGroup.epoch().toLong()
            val proofSeed = publisherGroup.dsAccessSeed(device)
            val staged = publisherGroup.stageCommit(device, admit, retire, false)
            PublisherCommitEntry(
                id = PublisherCommitEntry.newId(),
                kind = PublisherCommitEntry.Kind.ADMIT,
                channelIdHex = channelIdHex.lowercase(),
                groupIdHex = DsClient.pseudonymHex(publisherGroup.groupId()),
                targetEpoch = proofEpoch + 1,
                serverTargetEpoch = serverTarget,
                commitB64 = Base64.encodeToString(staged.commit, Base64.NO_WRAP),
                welcomeB64 = staged.welcome?.let { Base64.encodeToString(it, Base64.NO_WRAP) },
                joinerKeyB64 = welcomeAccount?.takeIf { staged.welcome != null }
                    ?.let { Base64.encodeToString(it, Base64.NO_WRAP) },
                proofEpoch = proofEpoch,
                proofSeedB64 = Base64.encodeToString(proofSeed, Base64.NO_WRAP),
                delegationHeader = delegationHeader,
                delegationBindingB64 = delegationBinding?.let { Base64.encodeToString(it, Base64.NO_WRAP) },
            ).also { proofSeed.fill(0) }
        }
        return runPublisherCommit(
            entry,
            commitPorts(ds, channelsClient, ids, publisherGroup, channelIdHex, accountIdB64, store),
        )
    }

    /**
     * ADR-0064 section 7 (amendment of 2026-11-14) — this owner device holds
     * no PublisherGroup for [channelIdHex]: enter from the saved epoch
     * [saved] by an external commit (ExternalInit alone, this device's bound
     * leaf), HELD until the Delivery Service answers. Recorded like any
     * staged PublisherGroup commit (the entry, then the provider snapshot
     * carrying the hold), sent through the commit route signed with the
     * journal root and proven under the saved epoch's access seed; taken, it
     * merges, the new seed is resealed to the subscribers and the head
     * written. [EntryAttempt.NOT_TAKEN] when the core refuses the GroupInfo or
     * the DS refuses the commit (the entry abandoned: the handle then holds
     * nothing). A throw (no answer) leaves the hold and the entry for the
     * replay.
     */
    public suspend fun enterFromSaved(
        ds: DsClient,
        channelsClient: ChannelsClient,
        ids: ChannelIdentity,
        channelIdHex: String,
        saved: ReentryEpoch,
        accountIdB64: String,
        store: PublisherCommitStore,
    ): EntryAttempt {
        val channelId = hexToBytes(channelIdHex) ?: throw CoPublisherException.BadChannelId(channelIdHex)
        val serverTarget = nextServerEpoch(channelsClient, channelIdHex)
        val ownerSeed = ids.ownerAccountSeed()
        val entered = try {
            // G-55: the commit comes back wrapped under the saved epoch's access
            // seed; the Delivery Service never reads the leaf's binding.
            ids.mls { device ->
                uniffi.lakko_ffi_uniffi.publisherGroupEnter(device, saved.groupInfo, ownerSeed, channelId, saved.dsAccessSeed)
            }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.i("CoPublisher", "ADR-0064: a saved channel epoch was refused by the core: ${e.javaClass.simpleName}")
            return EntryAttempt.NOT_TAKEN
        } finally {
            ownerSeed.fill(0)
        }
        val pg = entered.group
        try {
            val entry = PublisherCommitEntry(
                id = PublisherCommitEntry.newId(),
                kind = PublisherCommitEntry.Kind.ENTER,
                channelIdHex = channelIdHex.lowercase(),
                groupIdHex = DsClient.pseudonymHex(pg.groupId()),
                targetEpoch = saved.epoch + 1,
                serverTargetEpoch = serverTarget,
                commitB64 = Base64.encodeToString(entered.commit, Base64.NO_WRAP),
                proofEpoch = saved.epoch,
                proofSeedB64 = Base64.encodeToString(saved.dsAccessSeed, Base64.NO_WRAP),
            )
            var recorded = false
            try {
                store.put(accountIdB64, entry)
                recorded = true
                ids.persist()
            } catch (e: Throwable) {
                if (recorded) store.remove(accountIdB64, entry.id)
                try {
                    ids.mls { device -> pg.abandonStagedCommit(device) }
                    ids.persist()
                } catch (abandon: Throwable) {
                    android.util.Log.w("CoPublisher", "ADR-0064: an entry was not abandoned after a failed record", abandon)
                }
                throw e
            }
            // The channel epoch the entered group's epoch serves (the reseal
            // moves the service to [serverTarget] as the group reaches the
            // saved epoch + 1).
            ChannelEpochOffsets.set(channelIdHex, serverTarget - (saved.epoch + 1))
            val outcome = runPublisherCommit(
                entry,
                commitPorts(ds, channelsClient, ids, pg, channelIdHex, accountIdB64, store, entering = true),
            )
            return if (outcome == PublisherCommitOutcome.LANDED) EntryAttempt.ENTERED else EntryAttempt.NOT_TAKEN
        } finally {
            pg.close()
        }
    }

    /** ADR-0064 section 7 — a refusal of an entry commit that will not change
     *  on a re-send: a refused proof (not a skewed clock) or any other 4xx
     *  but a rate limit. */
    internal fun entryRefused(cause: Throwable): Boolean = when (cause) {
        is me.lakko.app.ds.DsProofRefusedError -> !cause.clockSkewed
        is me.lakko.app.ds.DsHttpError -> cause.status in 400..499 && cause.status != 408 && cause.status != 429
        else -> false
    }

    /** The channels-service epoch a reseal after the next commit binds to. */
    internal suspend fun nextServerEpoch(channelsClient: ChannelsClient, channelIdHex: String): Long {
        val current = channelsClient.fetchEpoch(channelIdHex)
        if (current == UInt.MAX_VALUE) throw CoPublisherException.UnexpectedServerEpoch(current)
        return (current + 1u).toLong()
    }

    /**
     * ADR-0087 section 15 (phase B) — a co-publisher steps down: one
     * application message on the PublisherGroup journal an owner device
     * applies as a removal of the sender's account.
     */
    public suspend fun stepDown(ds: DsClient, ids: ChannelIdentity, publisherGroup: PublisherGroup) {
        val message = ids.mls { device -> publisherGroup.stepDown(device) }
        // The sealed message advanced the ratchet: on disk before it leaves.
        ids.persist()
        ds.publishGroup(publisherGroup.groupId(), message, signer = signerOf(publisherGroup, ids))
    }

    /** ADR-0090 — seal [plaintext] as a PublisherGroup application message
     *  and publish it, the ratchet step on disk first (G-34 class). */
    public suspend fun publishApplication(
        ds: DsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        plaintext: ByteArray,
    ) {
        val message = ids.mls { device -> publisherGroup.createApplicationMessage(device, plaintext) }
        ids.persist()
        ds.publishGroup(publisherGroup.groupId(), message, signer = signerOf(publisherGroup, ids))
    }

    /**
     * ADR-0090 section 3 — STAGE a PublisherGroup commit with [create] and
     * record it: the stage (under the provider lock), then the entry, then
     * the provider snapshot carrying the staged commit, both fail-closed. On
     * a failure of the record or the snapshot the stage is abandoned and the
     * entry removed: nothing leaves the device.
     */
    internal suspend fun recordCommit(
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        accountIdB64: String,
        store: PublisherCommitStore,
        create: (device: Identity) -> PublisherCommitEntry,
    ): PublisherCommitEntry {
        val entry = ids.mls { device -> create(device) }
        var recorded = false
        try {
            store.put(accountIdB64, entry)
            recorded = true
            ids.persist()
            return entry
        } catch (e: Throwable) {
            if (recorded) store.remove(accountIdB64, entry.id)
            try {
                ids.mls { device -> publisherGroup.abandonStagedCommit(device) }
            } catch (abandon: Throwable) {
                android.util.Log.w("CoPublisher", "ADR-0090: a staged commit was not abandoned after a failed record", abandon)
            }
            throw e
        }
    }

    /** ADR-0090 — the ports a recorded commit of [publisherGroup] runs through. */
    internal fun commitPorts(
        ds: DsClient,
        channelsClient: ChannelsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        channelIdHex: String,
        accountIdB64: String,
        store: PublisherCommitStore,
        /** ADR-0064 section 7 — an entry from a saved epoch: a refused proof
         *  (the saved epoch is no longer one the DS keeps) or any other
         *  definitive refusal is also lost, so the older epoch is tried. */
        entering: Boolean = false,
    ): PublisherCommitPorts = object : PublisherCommitPorts {
        override suspend fun beforeSend() {
            if (ids.mls { publisherGroup.hasStagedCommit() }) ChannelVaultHeads.writeStaged(ids, publisherGroup)
        }

        override suspend fun writeHead() {
            ChannelVaultHeads.writeCurrent(ids, publisherGroup)
        }

        /** ADR-0081 / ADR-0090 — registers the epoch the staged commit moves
         *  to (its key from `dsStagedAccessSigner`; after a merge the current
         *  one) and appends the commit in one request, proven under the
         *  recorded seed of the epoch it leaves. */
        override suspend fun commitToJournal(commit: ByteArray, proofEpoch: Long?, proofSeed: ByteArray?) {
            val groupId = publisherGroup.groupId()
            val next: DsJournalSigner = ids.mls { device ->
                val staged = if (publisherGroup.hasStagedCommit()) publisherGroup.dsStagedAccessSigner(device) else null
                FfiDsJournalSigner(staged ?: publisherGroup.dsAccessSigner(device))
            }
            val proof: DsJournalSigner = if (proofEpoch != null && proofSeed != null) {
                FfiDsJournalSigner(uniffi.lakko_ffi_uniffi.DsProofSigner.forGroup(proofEpoch.toULong(), proofSeed))
            } else {
                next
            }
            val root = me.lakko.app.ds.FfiDsStatementSigning.root(ids.account, groupId)
            val started = System.currentTimeMillis()
            try {
                ds.postCommit(
                    { root.commit(groupId, next.epoch, next.verifyingKey, emptyList(), commit) },
                    proof,
                )
                // ADR-0090 open question 1 — the commit-route outcome is
                // logged (no identifiers), so the rotation jitter can be set
                // from what is seen.
                android.util.Log.i("CoPublisher", "ADR-0090: publisher commit taken after ${System.currentTimeMillis() - started} ms")
            } catch (e: me.lakko.app.ds.DsCommitConflictError) {
                android.util.Log.i("CoPublisher", "ADR-0090: publisher commit lost (409) after ${System.currentTimeMillis() - started} ms")
                throw e
            }
        }

        override fun isLost(cause: Throwable): Boolean =
            cause is me.lakko.app.ds.DsCommitConflictError ||
                me.lakko.app.chat.SweepRegistrationBackoff.isBackoffRefusal(cause) ||
                (entering && entryRefused(cause))

        override suspend fun merge() {
            ids.mls { device -> if (publisherGroup.hasStagedCommit()) publisherGroup.mergeStagedCommit(device) }
            ids.persist()
        }

        override suspend fun abandon() {
            ids.mls { device -> if (publisherGroup.hasStagedCommit()) publisherGroup.abandonStagedCommit(device) }
            ids.persist()
        }

        override suspend fun deliverWelcome(entry: PublisherCommitEntry, welcome: ByteArray, accountKey: ByteArray) {
            val dest = welcomeDest(ids, entry, accountKey)
                ?: throw IllegalStateException("no verified way to the account's mailbox yet")
            val sealed = mailboxCrypto.sealFor(dest, MailboxRowKind.WELCOME_RENDEZVOUS, welcome)
            ds.publishMailbox(sealed.address, sealed.row, reach = dest.auth)
        }

        override suspend fun serverEpoch(): Long = channelsClient.fetchEpoch(channelIdHex).toLong()

        override suspend fun reseal(targetEpoch: Long) {
            resealToSubscribers(
                channelsClient,
                ids,
                publisherGroup,
                channelIdHex,
                targetEpoch,
                ChannelReseal.throughFor(accountIdB64, channelIdHex),
                ChannelReseal.accountKeysFor(accountIdB64, channelIdHex),
            )
        }

        override suspend fun appoint(joinerKey: ByteArray) {
            val key = bindingFor(publisherGroup.groupId(), joinerKey)
            if (key == null) {
                android.util.Log.w("CoPublisher", "ADR-0059: no binding read for the approved joiner; not appointed")
                return
            }
            channelsClient.setCoPublisherAppointment(true, channelIdHex, ids.account, key)
        }

        override suspend fun unappoint(accountKey: ByteArray) {
            val key = bindingFor(publisherGroup.groupId(), accountKey)
            if (key == null) {
                android.util.Log.w("CoPublisher", "ADR-0087: no binding read for the removed publisher; appointment left")
                return
            }
            channelsClient.setCoPublisherAppointment(false, channelIdHex, ids.account, key)
        }

        override suspend fun handOver(accountKey: ByteArray) {
            ChannelAdmissionSeedShare.owedAgain(accountIdB64, channelIdHex, accountKey, ids.accountKey())
        }

        override fun save(entry: PublisherCommitEntry) = store.put(accountIdB64, entry)

        override fun finish(entry: PublisherCommitEntry) = store.remove(accountIdB64, entry.id)
    }

    /**
     * ADR-0090 section 2.3 — where the Welcome of [entry] to [accountKey]
     * goes: this account's own mailbox with its own `contact` key; a
     * co-publisher's with the committer delegation its device request carried;
     * a joiner's (an approve) with the reply grant its knock carried; else any
     * grant this account holds for the person. `null` keeps it owed.
     */
    private fun welcomeDest(ids: ChannelIdentity, entry: PublisherCommitEntry, accountKey: ByteArray): me.lakko.app.contacts.MailboxDest? {
        val reach = me.lakko.app.reach.ReachManager.peek()
        if (accountKey.contentEquals(ids.accountKey())) {
            return reach?.reachAuthFor(accountKey, me.lakko.app.reach.WritePurpose.WELCOME)
        }
        val header = entry.delegationHeader
        val binding = entry.delegationBindingB64?.let { Base64.decode(it, Base64.NO_WRAP) }
        if (header != null && binding != null) {
            val pk = mailboxCryptoBinding(binding, accountKey)
            if (pk != null) {
                val seed = ids.account.ed25519Seed()
                try {
                    return me.lakko.app.contacts.MailboxDest(
                        pk,
                        me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(seed), delegationHeader = header),
                    )
                } finally {
                    seed.fill(0)
                }
            }
        }
        joinerReplyGrants[me.lakko.app.reach.reachHex(accountKey)]?.let { grant ->
            verifiedGrantDest(grant, accountKey, mailboxCrypto) { seed ->
                me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(seed))
            }?.let { return it }
        }
        return reach?.reachAuthFor(accountKey, me.lakko.app.reach.WritePurpose.WELCOME)
    }

    /**
     * Resume every recorded commit of [accountIdB64] whose steps had not all
     * landed (a restart, an unanswered commit route), on this device's
     * [ids]. ADR-0090: a staged commit is sent again (the DS answers the
     * identical re-send `200`, or `409`, which abandons it); an entry whose
     * group neither reached nor staged its target is dropped
     * ([replayVerdict]); a staged commit no entry names is abandoned.
     * Returns the channels whose commit finished (their content seed
     * changed).
     */
    public suspend fun replayPublisherCommits(
        ds: DsClient,
        channelsClient: ChannelsClient,
        ids: ChannelIdentity,
        accountIdB64: String,
        store: PublisherCommitStore,
        coPublished: CoPublishedJournalMarks?,
        /** Only this channel's entries, or every channel's when `null`. */
        onlyChannel: String? = null,
    ): List<String> {
        val finished = ArrayList<String>()
        for (entry in store.entries(accountIdB64)) {
            if (onlyChannel != null && entry.channelIdHex != onlyChannel.lowercase()) continue
            val group = try {
                loadPublisherGroup(ids, entry.channelIdHex, coPublished)
            } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                throw e
            } catch (_: Throwable) {
                null
            }
            try {
                val restored = group?.let { g -> ids.mls { g.epoch().toLong() to g.hasStagedCommit() } }
                when (replayVerdict(entry, restored?.first, restored?.second == true)) {
                    PublisherCommitReplay.DISCARD -> {
                        android.util.Log.w("CoPublisher", "G-50: a commit whose epoch never became durable was dropped")
                        store.remove(accountIdB64, entry.id)
                    }
                    PublisherCommitReplay.REPLAY -> {
                        val pg = group ?: continue
                        val outcome = runPublisherCommit(
                            entry,
                            commitPorts(
                                ds,
                                channelsClient,
                                ids,
                                pg,
                                entry.channelIdHex,
                                accountIdB64,
                                store,
                                entering = entry.kind == PublisherCommitEntry.Kind.ENTER,
                            ),
                        )
                        if (outcome == PublisherCommitOutcome.LANDED) finished += entry.channelIdHex
                    }
                }
            } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                throw e
            } catch (e: Throwable) {
                android.util.Log.w("CoPublisher", "G-50: a recorded commit did not finish; retried later", e)
            } finally {
                group?.close()
            }
        }
        return finished
    }

    /** ADR-0090 — abandon a staged commit of [publisherGroup] that no recorded
     *  entry names (its record failed before a crash): it never left the
     *  device. `true` when one was abandoned. */
    public suspend fun abandonOrphanStage(
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        accountIdB64: String,
        store: PublisherCommitStore,
    ): Boolean {
        if (!ids.mls { publisherGroup.hasStagedCommit() }) return false
        val groupHex = DsClient.pseudonymHex(publisherGroup.groupId())
        if (store.entries(accountIdB64).any { it.groupIdHex == groupHex }) return false
        ids.mls { device -> publisherGroup.abandonStagedCommit(device) }
        ids.persist()
        return true
    }

    private const val HTTP_CONFLICT: Int = 409

    /** Co-publisher keys read off PublisherGroup knock queues (ADR-0059
     *  section 1), filed under the joiner account key each binding was
     *  verified under, so the approve looks one up by the joiner key it
     *  approves. In memory: the knock queue is re-read from the start on
     *  every poll, so a restart refills it. */
    private val coPublisherBindings = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()

    /** The capability wire each pending joiner's sealed request carried
     *  (ADR-0087 section 15: the owner approves without asking a request
     *  that came by a publisher link it minted). In memory, refilled by the
     *  knock read after a restart. */
    private val joinerCapabilities = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()

    private fun keepCapability(groupId: ByteArray, joinerKey: ByteArray, capability: ByteArray?) {
        if (capability != null && capability.isNotEmpty()) joinerCapabilities[bindingSlot(groupId, joinerKey)] = capability
    }

    /** The capability wire a pending joiner's request carried, or `null`. */
    public fun capabilityOfJoiner(groupId: ByteArray, joinerKey: ByteArray): ByteArray? =
        joinerCapabilities[bindingSlot(groupId, joinerKey)]

    private fun bindingSlot(groupId: ByteArray, joinerKey: ByteArray): String =
        ChannelsComposer.bytesToHex(groupId) + ":" + ChannelsComposer.bytesToHex(joinerKey)

    private fun recordCoPublisherBinding(groupId: ByteArray, channelId: ByteArray, bytes: ByteArray) {
        val binding = try {
            decodeChannelCopublisherBinding(bytes)
        } catch (_: Throwable) {
            return
        }
        if (!binding.channelId.contentEquals(channelId)) return
        coPublisherBindings[bindingSlot(groupId, binding.accountPubkey)] = binding.copublisherPubkey
        CoPublisherBindingStore.put(groupId, binding.accountPubkey, binding.copublisherPubkey)
    }

    /**
     * ADR-0087 phase B — a binding a co-publisher posted into the journal:
     * filed only when it is signed by (and names) the account the MLS sender
     * resolves to, for this channel. Returns whether it was one.
     */
    public fun recordJournalBinding(
        groupId: ByteArray,
        channelId: ByteArray,
        senderAccount: ByteArray,
        plaintext: ByteArray,
    ): Boolean {
        val binding = try {
            decodeChannelCopublisherBinding(plaintext)
        } catch (_: Throwable) {
            return false
        }
        if (!binding.channelId.contentEquals(channelId)) return true
        if (!binding.accountPubkey.contentEquals(senderAccount)) return true
        coPublisherBindings[bindingSlot(groupId, binding.accountPubkey)] = binding.copublisherPubkey
        CoPublisherBindingStore.put(groupId, binding.accountPubkey, binding.copublisherPubkey)
        return true
    }

    /** ADR-0087 phase B — a co-publisher posts its own binding into the
     *  PublisherGroup journal, so every owner device can later withdraw its
     *  appointment, including one that did not approve it. */
    public suspend fun postBinding(
        ds: DsClient,
        ids: ChannelIdentity,
        publisherGroup: PublisherGroup,
        channelId: ByteArray,
    ) {
        // Signed by the ACCOUNT identity: the binding names the account.
        val binding = composeChannelCopublisherBinding(ids.account, channelId)
        publishApplication(ds, ids, publisherGroup, binding)
    }

    /** The co-publisher key appointed for [accountKey] in [groupId]: this
     *  process's, else the one filed on disk. */
    private fun bindingFor(groupId: ByteArray, accountKey: ByteArray): ByteArray? =
        coPublisherBindings[bindingSlot(groupId, accountKey)] ?: CoPublisherBindingStore.get(groupId, accountKey)

    /** Reject a pending co-publisher — no commit, no epoch advance, nothing on
     *  the journal. The retained request set lives in the provider's MLS
     *  storage, so the drop is written there through [identity]; the caller
     *  persists the provider snapshot afterwards or the request is back after
     *  a restart. */
    public suspend fun reject(ids: ChannelIdentity, publisherGroup: PublisherGroup, proposalRef: ByteArray) {
        ids.mls { device -> publisherGroup.rejectExternalJoin(device, proposalRef) }
        ids.persist()
    }

    // -----------------------------------------------------------------
    // Joiner side — a prospective co-publisher requests + joins
    // -----------------------------------------------------------------

    /** When this process last sent a co-publisher request, or `0`. */
    @Volatile
    private var lastCoPublishRequestMs: Long = 0L

    /**
     * ADR-0053 — a co-publisher's Welcome now lands in the account mailbox
     * the contacts inbox also reads, and the inbox cannot open it (it is sealed
     * to the channel identity's KeyPackage, not a chat one). While a request
     * is recent the inbox therefore stays quiet about a Welcome it could not
     * join, instead of telling the user a chat failed to open.
     */
    public fun awaitingCoPublishWelcome(nowMs: Long = System.currentTimeMillis()): Boolean =
        lastCoPublishRequestMs != 0L && nowMs - lastCoPublishRequestMs < CO_PUBLISH_WELCOME_WINDOW_MS

    private const val CO_PUBLISH_WELCOME_WINDOW_MS: Long = 60L * 60L * 1000L

    /** Handles a [requestToCoPublish] returns so the caller can drive the poll
     *  loop ([pollForCoPublishWelcome]). */
    public data class CoPublishRequest(
        /** The PublisherGroup's MLS `group_id` (the journal channel to poll). */
        val groupId: ByteArray,
        /** The 32-char lowercase-hex channel id the link's sealed preview
         *  carried (ADR-0078 section 6). */
        val channelIdHex: String,
        /** This device's leaf key — the one its KeyPackage carried. */
        val joinerKey: ByteArray,
        /** ADR-0090 — the owner's account key (the capability's issuer). */
        val ownerAccountKey: ByteArray,
    )

    /** ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
     *  from a `/g/` co-publisher link. Decodes + verifies the capability,
     *  reads the channel id from its sealed preview (ADR-0078 section 6: the
     *  group id is the owner's derivation and cannot be inverted), and knocks
     *  (ADR-0053 §4): an empty knock learns the epoch, a second carries a
     *  self-signed join-attribution (OQ2) and the external-join proposal
     *  (ADR-0027) for it. The journal is never read. The caller then polls
     *  with [pollForCoPublishWelcome]. */
    public suspend fun requestToCoPublish(
        ds: DsClient,
        ids: ChannelIdentity,
        link: String,
        coPublished: CoPublishedJournalMarks,
        /** The ledger fetch of a `/g/` row by its id; `null` when it is gone. */
        fetchSealed: suspend (blobId: ByteArray) -> ByteArray?,
        /** ADR-0087 section 7 — this account's profile envelope with its
         *  account id, so the owner can seal the admission seeds to it. */
        profileEnvelope: ByteArray = ByteArray(0),
        /** Called with the request after its keys are on disk and before
         *  the knock (the durable pending-request record); a throw sends
         *  nothing. */
        beforeKnock: (CoPublishRequest) -> Unit = {},
    ): CoPublishRequest {
        val idx = link.lastIndexOf(URL_PREFIX)
        if (idx < 0) throw CoPublisherException.NotACoPublisherLink(link)
        val argument = me.lakko.app.discovery.InviteLinkArgument.decode(
            link.substring(idx + URL_PREFIX.length).trim(),
        )
        // Sealed links: the fragment opens the capability; a missing or
        // malformed one is refused before any request.
        val secret = me.lakko.app.discovery.requireInviteLinkSecret(argument.fragment)
        // The slug is the ledger id; an inline slug (the form removed on
        // 2026-10-19) or a row that is gone is not a co-publisher link.
        val blobId = try {
            (me.lakko.app.chat.parseGroupInviteSlug(argument.slug.trimEnd('/')) as
                me.lakko.app.chat.ParsedGroupInvite.Pointer).blobId
        } catch (_: me.lakko.app.chat.GroupInviteUrlException) {
            throw CoPublisherException.NotACoPublisherLink(link)
        }
        val sealed = fetchSealed(blobId) ?: throw CoPublisherException.NotACoPublisherLink(link)
        val cap = me.lakko.app.discovery.openSealedInviteLink(sealed, secret) { bytes, key ->
            GroupInviteCapability.openLink(bytes, key)
        }
        val groupId: ByteArray
        val wire: ByteArray
        val previewChannelId: ByteArray?
        val knockKey: uniffi.lakko_ffi_uniffi.FfiGroupKnockKey?
        val ownerAccountKey: ByteArray
        try {
            groupId = cap.groupId()
            // ADR-0090 — the capability's issuer is the channel owner's
            // account key: the Welcome must come from one of its devices.
            ownerAccountKey = cap.issuerSignaturePubkey()
            // The knock row carries the PLAINTEXT capability.
            wire = cap.wire()
            knockKey = cap.knock()
            previewChannelId = try {
                cap.openPreview(secret)?.channelId
            } catch (_: Throwable) {
                null
            }
        } finally {
            cap.close()
        }
        val channelIdBytes = previewChannelId?.takeIf { it.size == CHANNEL_ID_LEN }
            ?: throw if (LegacyChannelV1.isLegacyV1PublisherGroupId(groupId)) {
                // A link minted under the retired v1 derivation — distinct from
                // a `/g/` link that simply targets a chat group, and it needs
                // its own copy.
                LegacyChannelException(LegacyChannelKind.INVITE_LINK)
            } else {
                CoPublisherException.NotAChannelGroup
            }
        val channelIdHex = ChannelsComposer.bytesToHex(channelIdBytes)
        // ADR-0057 — this device asks to CO-publish, so it is not the channel
        // owner: it must never claim the PublisherGroup journal. Marked before
        // the knock, so no later open of this channel can claim first.
        // ADR-0078 section 6: with the channel, since this account cannot
        // derive the owner's group id.
        coPublished.markChannel(DsClient.pseudonymHex(groupId), channelIdHex)
        // ADR-0087 section 5 (phase B) — the request is ONE sealed knock row:
        // a fresh KeyPackage of this identity, a reply grant on its own
        // mailbox and the capability, sealed to the knock key the link names.
        // It carries no epoch, so it waits through any number of commits.
        // ADR-0059 section 1 — this account's co-publisher key for the
        // channel, signed by its account identity key, rides beside it.
        val key = knockKey ?: throw CoPublisherException.NotAChannelGroup
        val binding = composeChannelCopublisherBinding(ids.account, channelIdBytes)
        // The Welcome goes to the ACCOUNT mailbox (ADR-0060: one inbound
        // mailbox per person); the KeyPackage is this device's leaf (ADR-0090).
        val replyGrant = registerReplyKey(ds, ids.account)
        val keyPackage = ids.mls { device -> device.createKeyPackage() }
        // The KeyPackage's private key is what opens the owner's Welcome: on
        // disk before the knock, fail-closed.
        ids.persist()
        val request = CoPublishRequest(
            groupId = groupId,
            channelIdHex = channelIdHex,
            joinerKey = ids.device.signaturePublicKey(),
            ownerAccountKey = ownerAccountKey,
        )
        beforeKnock(request)
        val row = uniffi.lakko_ffi_uniffi.sealKnockRow(
            key.publicKey,
            key.version,
            keyPackage,
            replyGrant,
            profileEnvelope,
            wire,
        )
        ds.knock(groupId, listOf(row, binding))
        lastCoPublishRequestMs = System.currentTimeMillis()
        return request
    }

    /** ADR-0028 OQ3 — joiner side: read this identity's own mailbox (ADR-0053)
     *  for the Welcome the owner sent after approving, join from it, and mint local
     *  publisher keys so this device can now publish to the channel. The joined
     *  group lands at the post-approve epoch — the same one the subscribers were
     *  resealed to — so the derived `channelSeed` matches. Returns the keys on
     *  success, or `null` if no Welcome has arrived yet (keep polling). */
    public suspend fun pollForCoPublishWelcome(
        ds: DsClient,
        ids: ChannelIdentity,
        groupId: ByteArray,
        ownerAccountKey: ByteArray,
        channelIdHex: String,
        coPublished: CoPublishedJournalMarks,
        // G-29 — reuse the persisted per-channel signing seed if this device
        // has co-published to the channel before (null on a first join).
        existingSigningSeed: ByteArray? = null,
    ): ChannelPublisherKeys? {
        // ADR-0053 client contract point 8 — the owner's approve sends the
        // Welcome to THIS identity's mailbox (the pseudonym of the key the
        // proposal carried), never to the journal. The mailbox is claimed, so
        // the read is signed under the identity key. Paged to the end: the
        // Welcome is among the newest rows.
        val mailboxSigner = dsMailboxSigner(ids.account)
        val pseudonym = mailboxSigner.mailboxAddress()
        val signer = FfiDsJournalSigner(mailboxSigner)
        val own = NativeOwnMailbox.of(ids.account)
        val candidates = ArrayList<ByteArray>()
        JournalHeadEpoch.scan<Unit>(
            fetchPage = { after, limit ->
                val raw = ds.fetchMailbox(pseudonym, after = after, limit = limit, waitSecs = 0, signer = signer)
                JournalHeadEpoch.Page(
                    rows = raw.messages.map { JournalHeadEpoch.Row(it.id, it.payload) },
                    hasMore = raw.hasMore,
                )
            },
            visit = { row ->
                // ADR-0078: every row is sealed to this mailbox; one that does
                // not open is dropped. The owner's approve writes a
                // WELCOME_RENDEZVOUS row; a chat WELCOME sharing the mailbox
                // is tried too (it simply fails to join below).
                welcomeBodyOf(own.open(row.payload))?.let { candidates += it }
                null
            },
        )
        // Join the PublisherGroup from the Welcome — this persists the joined
        // MLS state under the PublisherGroup `group_id` the link conveyed in
        // the identity's provider. The handle itself is throwaway (close it);
        // freshPublisherKeys re-opens the now-joined PublisherGroup under
        // that id and derives the shared content-key seed + a fresh
        // per-device signing key so this device can publish. Newest first,
        // and a Welcome this identity cannot open (a chat Welcome sharing
        // the mailbox, or one sealed to a KeyPackage it no longer holds) is
        // skipped rather than ending the wait.
        // ADR-0081 amendment — a PublisherGroup carries no role table, so it
        // joins through its own entry point: `Group.joinFromWelcome` refuses
        // its Welcome, and this one refuses a Welcome for any other group (or
        // a chat's) before anything is written.
        for (welcome in candidates.asReversed()) {
            val keys = joinCoPublishWelcome(ids, welcome, groupId, ownerAccountKey, channelIdHex, coPublished, existingSigningSeed)
            if (keys != null) return keys
            // else try an older candidate
        }
        return null
    }

    /**
     * Join the PublisherGroup [groupId] of [channelIdHex] from one [welcome]
     * (the raw Welcome a `WELCOME_RENDEZVOUS` or `WELCOME` row carries) and
     * mint this device's publisher keys; `null` when the Welcome is not this
     * request's (sealed to another KeyPackage, or for another group). Shared
     * by the Accept wait ([pollForCoPublishWelcome]) and the account mailbox
     * loop, which lands a late approval (`landPendingPublisherWelcome`).
     */
    public suspend fun joinCoPublishWelcome(
        ids: ChannelIdentity,
        welcome: ByteArray,
        groupId: ByteArray,
        ownerAccountKey: ByteArray,
        channelIdHex: String,
        coPublished: CoPublishedJournalMarks,
        existingSigningSeed: ByteArray? = null,
    ): ChannelPublisherKeys? {
        val channelId = hexToBytes(channelIdHex)
            ?: throw CoPublisherException.BadChannelId(channelIdHex)
        val joinedEpoch = joinReplacingHeld(ids, welcome, groupId, channelId, channelIdHex, ownerAccountKey, joinedGroupId = groupId)
            ?: return null
        learnEpochOffset(channelIdHex, joinedEpoch)
        // ADR-0090 section 2.3 — the owner key this device took the Welcome
        // under; written into the account's register by the reconcile, so the
        // account's other devices take a Welcome only from that account.
        coPublished.markOwner(channelIdHex, ownerAccountKey)
        // ADR-0059 section 9 — this account now publishes as a co-publisher;
        // its signed writes go out once under that key.
        ChannelPublisherRoles.rememberFor(ids.account, channelIdHex, ChannelPublisherRole.COPUBLISHER)
        // ADR-0057 — a co-publisher never claims the journal (idempotent with
        // the mark set when the request was sent).
        coPublished.markChannel(DsClient.pseudonymHex(groupId), channelIdHex)
        return ChannelsComposer.freshPublisherKeys(
            ids, channelIdHex, existingSigningSeed, joinedGroupId = groupId)
    }

    /**
     * ADR-0090 sections 2.3 / 2.5, security pass — join a PublisherGroup
     * Welcome under [ownerKey], replacing a group this device still holds
     * under [groupId] (a suspected reset keeps it) only when the Welcome is
     * for that group and its signer verifies as a device of [ownerKey]; the
     * held state is deleted immediately before the join. Returns the joined
     * group's epoch, or `null` when the Welcome is not this one's (nothing
     * is deleted then). The device posts again afterwards.
     */
    private suspend fun joinReplacingHeld(
        ids: ChannelIdentity,
        welcome: ByteArray,
        groupId: ByteArray,
        channelId: ByteArray,
        channelIdHex: String,
        ownerKey: ByteArray,
        joinedGroupId: ByteArray?,
    ): ULong? {
        val held = try {
            openPublisherGroupOn(ids, channelIdHex, joinedGroupId, allowFound = false)
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
        val suspected = SubscriberAdmission.appContext?.let { ctx ->
            ChannelEpochOffsets.boundAccount?.let { acct -> ChannelSuspensions.isSuspended(ctx, acct, channelIdHex) }
        } ?: false
        var holdForLater = false
        val epoch = try {
            ids.mls { device ->
                val join = {
                    uniffi.lakko_ffi_uniffi.publisherGroupJoinFromWelcome(device, welcome, groupId, channelId, ownerKey)
                        .use { it.epoch() }
                }
                if (held == null) return@mls join()
                // Replay guard, rule 1: an active, unsuspected held group is
                // never replaced; the Welcome is ignored for this channel.
                val heldActive = held.isActive()
                // Verified first: a Welcome that is not an owner device's for
                // this group deletes nothing (and is not held).
                val founder = uniffi.lakko_ffi_uniffi.peekWelcomeFounder(welcome, device)
                if (!welcomeReplacesHeldGroup(founder.groupId, groupId, founder.founderAccountKey, ownerKey)) {
                    return@mls null
                }
                if (!heldGroupReplaceable(heldActive, suspected)) {
                    // Part B: this group's removal may be behind it in the
                    // journal; kept and tried again by the publisher pass.
                    holdForLater = true
                    return@mls null
                }
                val heldEpoch = held.epoch()
                // The held group comes back if the join fails: the provider
                // is snapshotted first and restored on a failed join, all in
                // this one provider-lock region (nothing else ran between).
                // Replay guard, rule 2: a joined group not newer than the held
                // one counts as a failed join (restored).
                replaceHeldThenJoin(
                    snapshot = { device.snapshotStorage() },
                    delete = { held.delete(device) },
                    join = {
                        val joined = join()
                        if (!joinedGroupReplacesHeld(joined, heldEpoch, heldActive)) throw StaleWelcomeRefused()
                        joined
                    },
                    restore = { before -> device.restoreStorage(before) },
                    wipe = { before -> before.fill(0) },
                )
            }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        } finally {
            held?.close()
        }
        // Persisted only after a join that landed: a failed one restored the
        // held group in memory, and the disk still holds it.
        if (epoch != null) ids.persist()
        val context = SubscriberAdmission.appContext
        val account = ChannelEpochOffsets.boundAccount
        if (epoch != null && context != null && account != null) {
            ChannelSuspensions.clear(context, account, channelIdHex)
            HeldPublisherWelcomes.clear(context, account, channelIdHex)
        }
        if (holdForLater && context != null && account != null) {
            if (!HeldPublisherWelcomes.hold(context, account, channelIdHex, welcome)) {
                android.util.Log.w("CoPublisher", "a publisher Welcome for a held group was not kept")
            }
        }
        return epoch
    }

    /**
     * Part B - whether the PublisherGroup this device holds for
     * [channelIdHex] is still active and unsuspected (the removal of this
     * device's leaf is not applied yet), so a held Welcome waits. `false`
     * when no group is held.
     */
    public suspend fun heldGroupWaitsForRemoval(ids: ChannelIdentity, channelIdHex: String, joinedGroupId: ByteArray?): Boolean {
        val suspected = SubscriberAdmission.appContext?.let { ctx ->
            ChannelEpochOffsets.boundAccount?.let { acct -> ChannelSuspensions.isSuspended(ctx, acct, channelIdHex) }
        } ?: false
        val held = try {
            openPublisherGroupOn(ids, channelIdHex, joinedGroupId, allowFound = false)
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        } ?: return false
        return try {
            !heldGroupReplaceable(ids.mls { held.isActive() }, suspected)
        } finally {
            held.close()
        }
    }

    /** ADR-0090 decision A — the committing device resealed before it sent
     *  the Welcome, so the service's epoch now is the one the joined group's
     *  epoch serves: the offset between them is learned here. A failed read
     *  leaves none recorded (the channel was never founded again: 0). */
    private suspend fun learnEpochOffset(channelIdHex: String, groupEpoch: ULong) {
        try {
            val service = ChannelsClient().use { it.fetchEpoch(channelIdHex) }
            ChannelEpochOffsets.learn(channelIdHex, groupEpoch, service)
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("CoPublisher", "ADR-0090: the channel epoch offset was not learned: ${e.javaClass.simpleName}")
        }
    }

    /**
     * The MLS group id a [welcome] from the account mailbox is for, read
     * without joining (`peekWelcomeFounder` writes nothing); `null` when this
     * device cannot read it (sealed to another device of the account, or not
     * a Welcome). Lets the mailbox loop pick the one channel a PublisherGroup
     * Welcome is for before touching any held group.
     */
    public suspend fun welcomeGroupId(ids: ChannelIdentity, welcome: ByteArray): ByteArray? =
        try {
            ids.mls { device -> uniffi.lakko_ffi_uniffi.peekWelcomeFounder(welcome, device).groupId }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }

    /**
     * The PublisherGroup id of [channelIdHex] for this account: the owner-seed
     * derivation for an [owned] channel, [joinedGroupId] (the register's) for
     * a co-published one; `null` when none can be named.
     */
    public fun expectedPublisherGroupId(
        ids: ChannelIdentity,
        channelIdHex: String,
        owned: Boolean,
        joinedGroupId: ByteArray?,
    ): ByteArray? {
        if (!owned) return joinedGroupId?.takeIf { it.size == 32 }
        val channelId = hexToBytes(channelIdHex) ?: return null
        val seed = ids.ownerAccountSeed()
        return try {
            uniffi.lakko_ffi_uniffi.publisherGroupIdForSeed(seed, channelId)
        } catch (_: Throwable) {
            null
        } finally {
            seed.fill(0)
        }
    }

    /**
     * ADR-0090 section 2.3 — this device was admitted to an account's
     * PublisherGroup by an owner device (its own account's, or a channel it
     * co-publishes): join from the Welcome under the id it computes (owner:
     * `publisherGroupIdForSeed`) or the register names (co-publisher), and
     * mint its publisher keys. `null` when the Welcome is not for that group.
     */
    public suspend fun joinAdmissionWelcome(
        ids: ChannelIdentity,
        welcome: ByteArray,
        channelIdHex: String,
        owned: Boolean,
        /** A co-published channel's group id (the register's
         *  `publisherGroupId`); ignored for an owned one. */
        joinedGroupId: ByteArray?,
        /** A co-published channel's owner account key as the account's
         *  register records it (ADR-0090 section 2.3); ignored for an owned
         *  one. Without it this device does not join. */
        ownerAccountKey: ByteArray?,
        coPublished: CoPublishedJournalMarks?,
        existingSigningSeed: ByteArray? = null,
    ): ChannelPublisherKeys? {
        val channelId = hexToBytes(channelIdHex)
            ?: throw CoPublisherException.BadChannelId(channelIdHex)
        if (owned) {
            val seed = ids.ownerAccountSeed()
            val groupId: ByteArray
            val ownerKey: ByteArray
            try {
                groupId = uniffi.lakko_ffi_uniffi.publisherGroupIdForSeed(seed, channelId)
                ownerKey = uniffi.lakko_ffi_uniffi.publisherOwnerAccountKey(seed)
            } finally {
                seed.fill(0)
            }
            val joinedEpoch = joinReplacingHeld(ids, welcome, groupId, channelId, channelIdHex, ownerKey, joinedGroupId = null)
                ?: return null
            learnEpochOffset(channelIdHex, joinedEpoch)
            return ChannelsComposer.freshPublisherKeys(ids, channelIdHex, existingSigningSeed)
        }
        val groupId = joinedGroupId ?: return null
        // ADR-0090 section 2.3 — only under the owner key the account
        // recorded when it joined (the /g/ capability's verified issuer): the
        // core refuses a Welcome whose sender is not a device of that key.
        // Never a key read off the Welcome itself.
        val owner = ownerAccountKey?.takeIf { it.size == 32 } ?: return null
        return coPublished?.let {
            joinCoPublishWelcome(ids, welcome, groupId, owner, channelIdHex, it, existingSigningSeed)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0060 §6 — reply keys
    // -----------------------------------------------------------------

    private fun keepReplyGrant(joinerAccount: ByteArray, grant: uniffi.lakko_ffi_uniffi.FfiReachGrant) {
        if (grant.`class` != uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) return
        joinerReplyGrants[me.lakko.app.reach.reachHex(joinerAccount)] = grant
    }

    /** The mailbox key [binding] names when it verifies against [accountKey]. */
    private fun mailboxCryptoBinding(binding: ByteArray, accountKey: ByteArray): ByteArray? =
        try {
            mailboxCrypto.verifyBinding(binding, accountKey)
        } catch (_: Throwable) {
            null
        }

    /** Each pending joiner's reply grant, by its ACCOUNT key (hex). SECRET;
     *  process memory only — a restart re-reads the retained knocks. */
    private val joinerReplyGrants =
        java.util.concurrent.ConcurrentHashMap<String, uniffi.lakko_ffi_uniffi.FfiReachGrant>()

    /**
     * A `reply` key on [identity]'s own mailbox, registered (with its seven-day
     * expiry) before the request that carries it leaves. [identity] is the
     * channel publisher identity whose mailbox the owner's Welcome goes to.
     */
    private suspend fun registerReplyKey(
        ds: DsClient,
        identity: Identity,
    ): uniffi.lakko_ffi_uniffi.FfiReachGrant {
        val scope = ByteArray(me.lakko.app.reach.REPLY_SCOPE_LEN).also { java.security.SecureRandom().nextBytes(it) }
        val grant = uniffi.lakko_ffi_uniffi.deriveReachGrant(
            identity.ed25519Seed(),
            uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY,
            scope,
            0u,
        )
        val mailboxSigner = dsMailboxSigner(identity)
        ds.putMailboxReach(
            pseudonym = mailboxSigner.mailboxAddress(),
            signer = FfiDsJournalSigner(mailboxSigner),
            add = listOf(
                me.lakko.app.ds.DsReachKeyAdd(
                    uniffi.lakko_ffi_uniffi.reachVerifyingKey(grant.seed),
                    me.lakko.app.reach.replyExpiry(System.currentTimeMillis()),
                ),
            ),
            remove = emptyList(),
        )
        return grant
    }

    // -----------------------------------------------------------------
    // helpers
    // -----------------------------------------------------------------

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.isEmpty() || hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        for (i in out.indices) {
            val byte = hex.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null
            out[i] = byte.toByte()
        }
        return out
    }

    /** Bytes of a channel id. */
    private const val CHANNEL_ID_LEN: Int = 16

}
