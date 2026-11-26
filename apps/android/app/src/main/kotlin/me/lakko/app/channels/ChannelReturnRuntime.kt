// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.contacts.sealFor
import uniffi.lakko_ffi_uniffi.ChannelRecoveryKey
import uniffi.lakko_ffi_uniffi.FfiChannelEpochSeed
import uniffi.lakko_ffi_uniffi.FfiChannelReturn
import uniffi.lakko_ffi_uniffi.FfiChannelReturnRole

/*
 * ADR-0101 section 7 - the channel runtime of a key reset: the recovery key
 * every owned channel registers, the returns a peer sends a reset person, and
 * what the reset device does with the returns it read. The rules are in
 * `ChannelReturn.kt`; this is the I/O around them. Mirror of
 * `apps/web/src/channels/channel-return-runtime.ts`, driven from its own
 * app-wide loop in the session shell (the publisher loop runs only on a
 * device that publishes, and a reset device publishes nothing yet).
 */

/** What the runtime needs from the contacts layer (the accepted resets). */
public interface ResetContactsPort {
    /** Every key an accepted reset retired -> its account id (base64). */
    public fun supersededKeys(): Map<String, String>

    /** The key this device trusts for an account now, or `null`. */
    public fun pinnedKeyB64Of(accountIdB64: String): String?

    /** The accepted reset's generation while its continuity key is usable. */
    public fun liveResetGeneration(accountIdB64: String): Long?

    /** Fetch and keep the account's continuity mailbox; whether one is held. */
    public suspend fun refreshContinuityTarget(accountIdB64: String): Boolean
}

/** Auth has no channel recovery configured (`503`). */
public class ChannelRecoveryDisabled : Exception("channel recovery is not configured on this server")

/** What a recovery attempt came to. */
public enum class RecoveryOutcome { OWNED, REFUSED, RETRY }

public object ChannelReturnRuntime {
    private const val TAG = "ChannelReturn"

    /** How many card pages a channel is read for returns (chosen). */
    private const val RETURN_CARD_PAGES: Int = 10

    /** How long a publisher Accept is watched before the return is counted
     *  handled (chosen). */
    private const val ACCEPT_SETTLE_MS: Long = 20_000L

    private val _wakes = MutableStateFlow(0L)

    /** Moves when the continuity mailbox held a return: run a pass now. */
    public val wakes: StateFlow<Long> = _wakes.asStateFlow()

    /** The mailbox kept a channel return. */
    public fun notifyHeld() {
        _wakes.value = _wakes.value + 1
    }

    private val mutex = Mutex()

    /** Where a publisher Accept goes on after the settle time. */
    private val acceptScope = kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.SupervisorJob() + kotlinx.coroutines.Dispatchers.IO)

    /**
     * One pass: register the recovery key of every owned channel that has
     * none recorded, act on the returns read from the continuity mailbox,
     * send the returns this device owes reset people, and settle recovered
     * channels whose type is known again. Each step never throws.
     */
    public suspend fun pass(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        contacts: ResetContactsPort?,
        ownKeysB64: Set<String>,
        sessionToken: suspend () -> String?,
    ) {
        mutex.withLock {
            val account = state.boundAccount ?: return
            step("recovery keys") { registerOwedRecoveryKeys(context, state, client, account, sessionToken) }
            step("own cards") { keepPublishingCardsCurrent(context, state, client) }
            step("held returns") { runHeldReturns(context, state, client, account, sessionToken) }
            if (contacts != null) step("send returns") { sendReturns(context, state, client, account, contacts, ownKeysB64) }
            step("recovered channels") { settleRecovered(context, state, account) }
        }
    }

    private suspend fun step(name: String, block: suspend () -> Unit) {
        try {
            block()
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w(TAG, "$name: ${e.javaClass.simpleName}")
        }
    }

    // ------------------------------------------------------------------
    // The recovery key
    // ------------------------------------------------------------------

    /**
     * The channel's recovery key: the channel recovery input blinded here,
     * evaluated by auth under this account's per-account key (auth never
     * learns the channel id), finalised here. Throws
     * [ChannelRecoveryDisabled] on `503`, the auth error otherwise.
     */
    public suspend fun deriveRecoveryKey(idHex: String, sessionToken: String): ChannelRecoveryKey {
        uniffi.lakko_ffi_uniffi.OprfBlinder.blindChannelRecovery(ChannelsComposer.channelIdFromHex(idHex)).use { blinder ->
            val evaluation = try {
                me.lakko.app.auth.AuthClient().use { c ->
                    c.channelRecoveryEvaluate(sessionToken, java.util.Base64.getDecoder().decode(blinder.blindedB64()))
                }
            } catch (e: me.lakko.app.auth.AuthHttpError) {
                if (e.status == HTTP_UNAVAILABLE) throw ChannelRecoveryDisabled()
                throw e
            }
            val output = blinder.finalize(java.util.Base64.getEncoder().encodeToString(evaluation)).output
            try {
                return ChannelRecoveryKey.fromOprfOutput(output)
            } finally {
                output.fill(0)
            }
        }
    }

    /** The channels this device owns: it publishes them and no co-publisher
     *  group id marks them. */
    private fun ownedChannels(state: ChannelsUiState): List<String> =
        state.keysByChannel.value.keys.filter { state.publishesHere(it) && state.joinedGroupIdOf(it) == null }

    /**
     * Register the recovery key of every channel this device owns that has
     * none recorded (a new channel is picked up at the first pass after its
     * creation, so new and existing channels take one path). A `503` stops
     * the pass quietly; any other failure leaves the channel owing.
     */
    private suspend fun registerOwedRecoveryKeys(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        account: String,
        sessionToken: suspend () -> String?,
    ) {
        val store = RecoveryRegisteredStore.onDevice(context)
        val owing = channelsOwingRecoveryKey(ownedChannels(state), store.read(account))
        if (owing.isEmpty()) return
        val token = sessionToken() ?: return
        val pub = state.publisher ?: return
        for (idHex in owing) {
            val key = try {
                deriveRecoveryKey(idHex, token)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (_: ChannelRecoveryDisabled) {
                return
            } catch (_: Throwable) {
                continue
            }
            try {
                pub.withIdentity(persist = false) { ChannelsClient.ownerSigner(it, idHex) }.use { owner ->
                    client.putRecoveryKey(idHex, owner, key.verifyingKey())
                }
                store.mark(account, idHex)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                Log.i(TAG, "a channel recovery key was not registered yet: ${e.javaClass.simpleName}")
            } finally {
                key.close()
            }
        }
    }

    /**
     * Live walk 2026-11-26, D1 - the owner's and every publisher's own member
     * card, app-wide: an owner return is planned from the owner's card, so a
     * channel whose owner never opened it after the account's reader key was
     * registered had none and never came back after the owner's reset. Each
     * channel this device publishes ([channelsOwingPublishingCard]) uploads
     * its card under the account's reader key ([ChannelMemberCards.refreshPublisherCard],
     * the "Members can find me" rule applied there); an unchanged card is
     * not uploaded again ([cardUploadDue]).
     */
    private suspend fun keepPublishingCardsCurrent(context: Context, state: ChannelsUiState, client: ChannelsClient) {
        val due = channelsOwingPublishingCard(
            channels = state.channels.value.map { it.id },
            publishesHere = state::publishesHere,
            removed = RemovedChannels::isRemoved,
        )
        for (idHex in due) {
            try {
                ChannelMemberCards.refreshPublisherCard(context, state, client, idHex)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                Log.i(TAG, "a publishing device's card was not uploaded yet: ${e.javaClass.simpleName}")
            }
        }
    }

    // ------------------------------------------------------------------
    // The peer side: returns to send
    // ------------------------------------------------------------------

    /** One card this device opened, with what a return carries of it. */
    private class OpenedReturnCard(
        val card: ReturnCard,
        val subscriberPk: ByteArray,
        val bindingSig: ByteArray,
        val profileHash: ByteArray,
    )

    private suspend fun openCards(state: ChannelsUiState, client: ChannelsClient, idHex: String): List<OpenedReturnCard> {
        val out = ArrayList<OpenedReturnCard>()
        val channelId = ChannelsComposer.channelIdFromHex(idHex)
        val enc = java.util.Base64.getEncoder()
        var after: String? = null
        for (page in 0 until RETURN_CARD_PAGES) {
            val res = client.listMemberCards(idHex, after)
            for (m in res.members) {
                try {
                    val seed = ChannelMemberCards.seedForEpoch(state, idHex, uniffi.lakko_ffi_uniffi.memberCardEpoch(m.card))
                        ?: continue
                    val opened = uniffi.lakko_ffi_uniffi.openMemberCard(seed, m.card, channelId, m.subscriberPk)
                    out += OpenedReturnCard(
                        ReturnCard(enc.encodeToString(opened.identityPk), enc.encodeToString(m.subscriberPk)),
                        m.subscriberPk,
                        opened.bindingSig,
                        opened.profileHash,
                    )
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (_: Throwable) {
                    // A card this device cannot open names nobody.
                }
            }
            after = res.nextAfter ?: break
        }
        return out
    }

    /**
     * For every channel this device publishes or reads, and every reset it
     * accepted whose window is open, send the returns [channelReturnsFor]
     * plans that were not sent for that reset yet. Cards are read only when
     * there is a live reset at all.
     */
    private suspend fun sendReturns(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        account: String,
        contacts: ResetContactsPort,
        ownKeysB64: Set<String>,
    ) {
        val live = contacts.supersededKeys().filter { (_, acct) -> contacts.liveResetGeneration(acct) != null }
        if (live.isEmpty()) return
        val sentStore = SentReturnsStore.onDevice(context)
        val sent = sentStore.read(account).toMutableMap()
        val enc = java.util.Base64.getEncoder()
        val registers = try {
            ChannelSubscriptionSync.activePort()?.registers().orEmpty()
        } catch (_: Throwable) {
            emptyList()
        }
        for (handle in state.channels.value) {
            val idHex = handle.id
            val publishing = state.publishesHere(idHex)
            val ownRole = when {
                !publishing -> ReturnOwnRole.SUBSCRIBER
                state.joinedGroupIdOf(idHex) == null -> ReturnOwnRole.OWNER
                else -> ReturnOwnRole.PUBLISHER
            }
            val ownerKeyB64 = registers.firstOrNull { it.subscribed && it.channelIdHex.equals(idHex, ignoreCase = true) }
                ?.ownerAccountKey?.takeIf { it.size == 32 }?.let { enc.encodeToString(it) }
            // A subscriber only ever sends an owner return.
            if (ownRole == ReturnOwnRole.SUBSCRIBER && (ownerKeyB64 == null || ownerKeyB64 !in live)) continue
            val publishers = ChannelPublisherLoop.peek(state)?.publishers?.value?.get(idHex).orEmpty()
                .filter { !it.owner }.map { enc.encodeToString(it.accountKey) }
            val cards = try {
                openCards(state, client, idHex)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                continue
            }
            val planned = unsentReturns(
                channelReturnsFor(
                    listOf(ReturnChannel(idHex, ownRole, cards.map { it.card }, ownerKeyB64, publishers)),
                    live,
                    ownKeysB64,
                ),
                sent,
                contacts::liveResetGeneration,
            )
            for (r in planned) {
                val card = cards.firstOrNull { it.card.identityPkB64 == r.card.identityPkB64 } ?: continue
                val ok = try {
                    sendOne(context, state, client, contacts, handle, r, card)
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (e: Throwable) {
                    Log.i(TAG, "a channel return was not sent yet: ${e.javaClass.simpleName}")
                    false
                }
                val gen = contacts.liveResetGeneration(r.accountIdB64)
                if (ok && gen != null) {
                    sent[sentReturnKey(r)] = gen
                    sentStore.write(account, sent)
                }
            }
        }
    }

    private suspend fun sendOne(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        contacts: ResetContactsPort,
        handle: ChannelHandle,
        r: PlannedReturn,
        card: OpenedReturnCard,
    ): Boolean {
        val newKeyB64 = contacts.pinnedKeyB64Of(r.accountIdB64) ?: return false
        if (!contacts.refreshContinuityTarget(r.accountIdB64)) return false
        val newKey = java.util.Base64.getDecoder().decode(newKeyB64)
        // Written through the continuity mailbox ALONE: the person's real
        // mailbox drops a channel return.
        val dest = me.lakko.app.reach.ReachManager.get(context).continuityDestFor(newKey) ?: return false
        val invitation: ByteArray = when (r.role) {
            ChannelReturnRoleName.SUBSCRIBER ->
                ChannelInviteLinks.mintFull(context, state, client, handle, public = handle.sharedListed, oneUse = true).card
                    ?: return false
            // A one-use publisher invitation, recorded so the request it
            // brings is approved without asking (the owner's "Add publisher").
            ChannelReturnRoleName.PUBLISHER ->
                me.lakko.app.ui.channels.mintCoPublisherLink(state, handle.id, context).card ?: return false
            ChannelReturnRoleName.OWNER -> ByteArray(0)
        }
        val held = HashMap<UInt, ByteArray>(state.subscribedSeeds(handle.id))
        for ((epoch, seed) in state.heldEpochSeeds(handle.id)) held.putIfAbsent(epoch, seed)
        val seeds = newestSeeds(held.map { HeldSeed(it.key, it.value) })
        // ADR-0101 section 8 - the newest profile this device holds for the
        // person, verbatim (signed by their previous key).
        val retired = contacts.supersededKeys().filter { it.value == r.accountIdB64 }.keys.sorted()
        val profile = me.lakko.app.profile.newestHeldEnvelope(retired) {
            me.lakko.app.profile.ProfileDirectory.get(context).heldEnvelopeOf(it)
        }
        val envelope = uniffi.lakko_ffi_uniffi.composeChannelReturnEnvelope(
            FfiChannelReturn(
                channelId = ChannelsComposer.channelIdFromHex(handle.id),
                role = ffiRole(r.role),
                oldBindingSig = card.bindingSig,
                oldSubscriberPk = card.subscriberPk,
                profileHash = card.profileHash,
                invitation = invitation,
                epochSeeds = seeds.map { FfiChannelEpochSeed(it.epoch, it.seed) },
                profile = profile,
            ),
        )
        val sealed = me.lakko.app.contacts.NativeMailboxCrypto.sealFor(dest, me.lakko.app.contacts.MailboxRowKind.CHANNEL_RETURN, envelope)
        me.lakko.app.ds.DsClient().use { it.publishMailbox(sealed.address, sealed.row, reach = dest.auth) }
        if (r.role == ChannelReturnRoleName.PUBLISHER) {
            // The retired identity's leaves leave the PublisherGroup (a commit
            // and a rotation): its devices are gone, and its key must not
            // publish again. A failure is left for the owner to do by hand.
            try {
                ChannelPublisherLoop.peek(state)?.removePublisher(handle.id, java.util.Base64.getDecoder().decode(r.card.identityPkB64))
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                Log.w(TAG, "a reset publisher's old leaves were not removed: ${e.javaClass.simpleName}")
            }
        }
        return true
    }

    private fun ffiRole(role: ChannelReturnRoleName): FfiChannelReturnRole = when (role) {
        ChannelReturnRoleName.SUBSCRIBER -> FfiChannelReturnRole.SUBSCRIBER
        ChannelReturnRoleName.PUBLISHER -> FfiChannelReturnRole.PUBLISHER
        ChannelReturnRoleName.OWNER -> FfiChannelReturnRole.OWNER
    }

    private fun roleName(role: FfiChannelReturnRole): ChannelReturnRoleName = when (role) {
        FfiChannelReturnRole.SUBSCRIBER -> ChannelReturnRoleName.SUBSCRIBER
        FfiChannelReturnRole.PUBLISHER -> ChannelReturnRoleName.PUBLISHER
        FfiChannelReturnRole.OWNER -> ChannelReturnRoleName.OWNER
    }

    // ------------------------------------------------------------------
    // The reset side: returns read from the continuity mailbox
    // ------------------------------------------------------------------

    /** Act on every held return. One that needs the network and could not
     *  reach it is kept; every other is dropped once handled. */
    private suspend fun runHeldReturns(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        account: String,
        sessionToken: suspend () -> String?,
    ) {
        val store = HeldReturnsStore.onDevice(context)
        val held = store.read(account)
        if (held.isEmpty()) return
        val previous = me.lakko.app.auth.ResetContinuityStore.onDevice(context).load(account)?.previousKeyB64
        val done = ArrayList<String>()
        for (envB64 in held) {
            val env = try {
                java.util.Base64.getDecoder().decode(envB64)
            } catch (_: IllegalArgumentException) {
                done += envB64
                continue
            }
            var verified = false
            val decoded = try {
                if (previous != null) {
                    try {
                        uniffi.lakko_ffi_uniffi.verifyChannelReturnCardBinding(env, java.util.Base64.getDecoder().decode(previous))
                            .also { verified = true }
                    } catch (_: Throwable) {
                        uniffi.lakko_ffi_uniffi.decodeChannelReturnEnvelope(env)
                    }
                } else {
                    uniffi.lakko_ffi_uniffi.decodeChannelReturnEnvelope(env)
                }
            } catch (_: Throwable) {
                done += envB64
                continue
            }
            try {
                if (actOnReturn(context, state, client, account, decoded, verified, sessionToken) != RecoveryOutcome.RETRY) {
                    done += envB64
                }
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                // Kept: retried at the next pass.
                Log.i(TAG, "a channel return was not handled yet: ${e.javaClass.simpleName}")
            }
        }
        store.drop(account, done)
    }

    /** The result of one return: [RecoveryOutcome.RETRY] keeps it held. */
    private suspend fun actOnReturn(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        account: String,
        ret: FfiChannelReturn,
        verified: Boolean,
        sessionToken: suspend () -> String?,
    ): RecoveryOutcome {
        val idHex = ChannelsComposer.bytesToHex(ret.channelId)
        val role = roleName(ret.role)
        val register = try {
            ChannelSubscriptionSync.activePort()?.registers()?.firstOrNull {
                it.subscribed && it.channelIdHex.equals(idHex, ignoreCase = true)
            }
        } catch (_: Throwable) {
            null
        }
        val action = channelReturnAction(
            verified = verified,
            role = role,
            hasInvitation = ret.invitation.isNotEmpty(),
            alreadyInRole = if (role == ChannelReturnRoleName.PUBLISHER) {
                state.publishesHere(idHex)
            } else {
                state.channels.value.any { it.id == idHex }
            },
            ownsAlready = register?.role == ChannelAccountRole.OWNER,
        )
        val addSeeds = {
            state.addCarriedSeeds(mapOf(idHex to ret.epochSeeds.filter { it.seed.size == 32 }.associate { it.epoch to it.seed }))
        }
        if (verified) {
            ret.profile?.let { me.lakko.app.profile.offerReturnedOwnProfile(context, java.util.Base64.getDecoder().decode(account), it) }
        }
        return when (action) {
            ChannelReturnAction.DROP -> RecoveryOutcome.REFUSED
            ChannelReturnAction.SHOW_INVITATION -> {
                // Shown as an ordinary invitation card; a storage refusal
                // throws, which keeps the return held.
                ReturnedInvitationsStore.onDevice(context).keep(account, idHex, java.util.Base64.getEncoder().encodeToString(ret.invitation))
                RecoveryOutcome.OWNED
            }
            ChannelReturnAction.ADD_SEEDS -> {
                addSeeds()
                RecoveryOutcome.OWNED
            }
            ChannelReturnAction.SUBSCRIBE -> {
                val (facts, _) = me.lakko.app.ui.channels.openReturnedInvitationCard(ret.invitation)
                    ?: return RecoveryOutcome.REFUSED
                val invite = facts.preview.subscriber
                if (facts.publisherInvite || facts.channelIdHex != idHex || invite == null) return RecoveryOutcome.REFUSED
                try {
                    ChannelInviteLinks.subscribeFromLink(context, state, client, ret.channelId, facts.name, invite)
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (_: Throwable) {
                    return RecoveryOutcome.RETRY
                }
                addSeeds()
                RecoveryOutcome.OWNED
            }
            ChannelReturnAction.ACCEPT_PUBLISHER -> {
                val (facts, link) = me.lakko.app.ui.channels.openReturnedInvitationCard(ret.invitation)
                    ?: return RecoveryOutcome.REFUSED
                if (!facts.publisherInvite || facts.channelIdHex != idHex) return RecoveryOutcome.REFUSED
                val actions = me.lakko.app.ui.channels.ChannelInviteActionsHolder.actions ?: return RecoveryOutcome.RETRY
                addSeeds()
                // The ordinary Accept: the request is recorded and knocked, the
                // owner's device approves it, the Welcome lands through the
                // mailbox. A failure inside the first seconds (offline) keeps
                // the return; after that it is counted handled.
                // Its own wait is not awaited past the settle time: it goes on
                // in the background (the request has no deadline).
                val accepting = acceptScope.async {
                    try {
                        actions.acceptPublisher(facts, link) {}
                        RecoveryOutcome.OWNED
                    } catch (cancel: kotlinx.coroutines.CancellationException) {
                        throw cancel
                    } catch (_: Throwable) {
                        RecoveryOutcome.RETRY
                    }
                }
                kotlinx.coroutines.withTimeoutOrNull(ACCEPT_SETTLE_MS) { accepting.await() } ?: RecoveryOutcome.OWNED
            }
            ChannelReturnAction.RECOVER_OWNER -> {
                val outcome = recoverOwnedChannel(context, state, client, account, idHex, sessionToken)
                if (outcome == RecoveryOutcome.OWNED) addSeeds()
                outcome
            }
        }
    }

    /**
     * ADR-0101 section 7, owner - take a channel back with its recovery key:
     * evaluate it again for this account, move the owner key to the current
     * identity's with the recovery proof, and on `204` list the channel, keep
     * every reader the service lists now ([RecoveredChannelsStore], the
     * admission mark), mint a fresh admission seed, write the register as
     * owner and mark this device as founding alone, so the publisher pass
     * founds the PublisherGroup again at the id the new seed derives
     * (ADR-0090 decision A) and reseals. `401`: dropped silently.
     */
    public suspend fun recoverOwnedChannel(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        account: String,
        idHex: String,
        sessionToken: suspend () -> String?,
    ): RecoveryOutcome {
        val token = sessionToken() ?: return RecoveryOutcome.RETRY
        val pub = state.publisher ?: return RecoveryOutcome.RETRY
        val key = try {
            deriveRecoveryKey(idHex, token)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: ChannelRecoveryDisabled) {
            return RecoveryOutcome.REFUSED
        } catch (_: Throwable) {
            return RecoveryOutcome.RETRY
        }
        try {
            val newOwnerPk = pub.withIdentity(persist = false) { identity ->
                ChannelsClient.ownerSignerExact(identity, idHex).use { it.verifyingKey() }
            }
            val header = key.signOwnerChange(ChannelsComposer.channelIdFromHex(idHex), newOwnerPk, System.currentTimeMillis().toULong())
            client.replaceOwnerByRecovery(idHex, header, newOwnerPk)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: ChannelsApiException) {
            return if (e.status >= HTTP_SERVER_ERROR) RecoveryOutcome.RETRY else RecoveryOutcome.REFUSED
        } catch (_: Throwable) {
            return RecoveryOutcome.RETRY
        } finally {
            key.close()
        }
        if (state.channels.value.none { it.id == idHex }) {
            state.addChannel(ChannelHandle(id = idHex, localName = "", localDescription = "", addedAtMs = System.currentTimeMillis().toULong()))
        }
        // Every reader the service lists now stays let in: the re-founding's
        // reseal reaches each of them. A fresh admission seed verifies the
        // tickets of invitations made from now on.
        var throughSeq = 0L
        try {
            var after = 0L
            while (true) {
                val page = ChannelPublisherSigners.derive(pub, idHex).use { client.subscriberPage(idHex, it, after) }
                throughSeq = highestSeq(page.rows.map { it.seq }, throughSeq)
                if (page.rows.isEmpty() || page.nextAfter <= after) break
                after = page.nextAfter
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            // The owner key is ours now; the mark only ever rises at a later
            // return of this channel.
        }
        RecoveredChannelsStore.onDevice(context).mark(account, idHex, throughSeq)
        ChannelAdmissionMark.raise(context, account, idHex, throughSeq, markSet = true)
        ChannelAdmissionKeys.currentOrMint(context, account, idHex).second.fill(0)
        val keys = ChannelSubscriptionSync.keyForSubscribing(state, idHex)
        state.setAccountRole(idHex, ChannelAccountRole.OWNER)
        try {
            ChannelSubscriptionSync.activePort()?.write(
                ChannelRegisterView(
                    channelIdHex = idHex,
                    hlc = 0uL,
                    subscribed = true,
                    role = ChannelAccountRole.OWNER,
                    subscriberSecret = keys.secret.copyOf(),
                    readKey = ByteArray(0),
                    publisherGroupId = ByteArray(0),
                    name = "",
                    handle = "",
                ),
            )
        } catch (e: Throwable) {
            Log.w(TAG, "the recovered channel's register was not written: ${e.javaClass.simpleName}")
        }
        // This device holds the account's DeviceGroup alone after the reset:
        // the publisher pass may found the channel's group again here.
        ChannelRefounding.markFoundedAlone(context, account)
        Log.i(TAG, "ADR-0101: an owned channel was taken back")
        return RecoveryOutcome.OWNED
    }

    /** A recovered channel whose newest metadata record is held again has a
     *  known type: the recovery record goes (the admission mark stays). */
    private fun settleRecovered(context: Context, state: ChannelsUiState, account: String) {
        val store = RecoveredChannelsStore.onDevice(context)
        for (idHex in store.read(account).keys) {
            val handle = state.channels.value.firstOrNull { it.id == idHex } ?: continue
            if (recoveredVisibility(handle.sharedUpdatedAtMs?.let { handle.sharedListed }) != null) store.clear(account, idHex)
        }
    }

    /** Whether [idHex] is a recovered channel whose type is still unknown:
     *  its admission pass lets every row in (web `settleRecoveredChannels`). */
    public fun recoveredTypeUnknown(context: Context, state: ChannelsUiState, idHex: String): Boolean {
        val account = state.boundAccount ?: return false
        if (idHex !in RecoveredChannelsStore.onDevice(context).read(account)) return false
        val handle = state.channels.value.firstOrNull { it.id == idHex } ?: return true
        return handle.sharedUpdatedAtMs == null
    }

    private const val HTTP_UNAVAILABLE: Int = 503
    private const val HTTP_SERVER_ERROR: Int = 500
}
