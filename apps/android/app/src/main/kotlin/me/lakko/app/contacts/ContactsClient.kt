// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.util.Log
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryClientArgError
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.devices.DeviceAttestKey
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsClientArgError
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsReachAuth
import me.lakko.app.ds.DsReachRefusedError
import me.lakko.app.transparency.InMemoryLastSeenRoot
import me.lakko.app.transparency.LastSeenRootStore
import me.lakko.app.transparency.LogVerifyStatus
import me.lakko.app.transparency.TransparencyClient
import me.lakko.app.transparency.TransparencyConfig
import me.lakko.app.transparency.lookupServedKeyReset
import me.lakko.app.transparency.verifyIdentityInLog
import me.lakko.app.transparency.verifyRotationChain
import uniffi.lakko_ffi_uniffi.ContactAck
import uniffi.lakko_ffi_uniffi.ContactRequest
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.IdentityPseudonym
import uniffi.lakko_ffi_uniffi.composeContactAck
import uniffi.lakko_ffi_uniffi.composeContactRequest
import uniffi.lakko_ffi_uniffi.verifySupersession

/**
 * Orchestrates the send / poll sides of the
 * `lakko-contacts::ContactRequest` flow against the live
 * `lakko-svc-discovery` + `lakko-svc-ds` services.
 *
 * Mirror of `apps/web/src/contacts/client.ts` scoped to the
 * 1.15e5 surface — three operations:
 *
 *   * [sendContactRequest] — resolve `@username` →
 *     `account_id` via [DiscoveryClient.lookup] → fetch
 *     identity bundle → derive recipient mailbox pseudonym from
 *     their Ed25519 pubkey → compose envelope via UniFFI
 *     [composeContactRequest] → publish to recipient's mailbox
 *     via [DsClient.publishMailbox], wrapped with
 *     [KIND_CONTACT_REQUEST].
 *
 *   * [sendContactRequestToBundle] — same compose+publish path
 *     but skipping the lookup+identityFetch round-trips when
 *     the caller already has the recipient's bundle in hand
 *     (the invite-link flow in 1.15e6).
 *
 *   * [pollStrangerInbox] — fetch the user's own
 *     [IdentityPseudonym.fromIdentity]-derived mailbox via
 *     [DsClient.fetchMailbox], parse each envelope's kind
 *     byte, decode + verify each ContactRequest, surface a
 *     mixed list of [IncomingMailboxEvent] entries + a fresh
 *     cursor. Welcome envelopes pass through as
 *     [IncomingMailboxEvent.Welcome] for the chat surface
 *     (1.15f) to consume.
 *
 * The MLS-side group-creation handshake (`startGroupWith` /
 * `joinGroupFromWelcome`) lives in 1.15f's `ChatClient` — it
 * needs `Group` state which is a chat-sprint concern.
 *
 * ## Identity material
 *
 * Compose requires the sender's 32-byte Ed25519 signing seed +
 * their 16-byte `account_id`. Poll requires the recipient's
 * 32-byte Ed25519 verifying pubkey (for the mailbox pseudonym)
 * + their 32-byte X25519 secret (to open intro envelopes).
 *
 * For 1.15e5 these are passed in as `ByteArray` rather than via
 * a session-state object — the caller (eventually
 * `AuthRepository.identitySecrets` or a parallel
 * `IdentitySecrets` holder) is responsible for producing them
 * from the OPAQUE export key via
 * [uniffi.lakko_ffi_uniffi.Identity.fromSeed]. The full
 * session-state wiring lands with 1.15f's chat surface so the
 * canonical `Identity` reference can be threaded through both
 * contacts and chat.
 */
public class ContactsClient(
    private val discovery: DiscoveryClient = DiscoveryClient(),
    private val ds: DsClient = DsClient(),
    /**
     * G-4 (ADR-0016) — per-account AccountIdentity-pubkey pin store
     * driving the fetch-path supersession gate ([verifyBundleAccountKey]).
     *
     * **Non-null and required**, by design: a `null`/absent store would
     * silently disable the changed-key fail-closed gate (security
     * invariant #4), so the type system forbids it. Production wires a
     * persistent [PeerPinStore] over the app `Context` via
     * [ContactsRepository]; tests / direct callers pass an
     * [InMemoryPinStore]. The gate therefore always runs — first-ever
     * observations TOFU-pin (safe), and a changed key always fails closed.
     */
    private val pinStore: PinStore,
    /**
     * KT-log S3e — the no-rollback high-water-mark store for the ADDITIVE
     * transparency-log inclusion check ([checkTransparencyLog]). Backed by
     * a persistent [me.lakko.app.transparency.LastSeenRoot] in production
     * (wired by [ContactsRepository]); an
     * [me.lakko.app.transparency.InMemoryLastSeenRoot] otherwise. Never
     * `null` — but the log check is OFF unless a transparency URL +
     * operator key are also build-time configured.
     */
    private val lastSeenRoot: LastSeenRootStore = InMemoryLastSeenRoot(),
    /**
     * KT-log S3e — factory for the [TransparencyClient] the log check
     * uses. Returns `null` when the integration is OFF (no URL / no pinned
     * operator key) — the default delegates to
     * [TransparencyClient.forOptionalConfig]. Tests inject a factory over
     * a `MockEngine`-backed client to exercise the verdict mapping.
     */
    private val transparencyClientFactory: () -> TransparencyClient? = {
        TransparencyClient.forOptionalConfig()
    },
    /**
     * ADR-0066 section 3.5 - told once the gate re-pinned an account to a
     * key a supersession proof authorised: `(accountIdB64, oldKeyB64,
     * newKeyB64)`, standard base64. The old key is then that account's
     * proven predecessor ([ProvenPredecessors]). Never called for a change
     * the gate refused, and never for a first observation.
     */
    private val onProvenRotation: (String, String, String) -> Unit = { _, _, _ -> },
) : AutoCloseable {

    override fun close() {
        discovery.close()
        ds.close()
    }

    // -----------------------------------------------------------------
    // Send: resolve username → compose + publish
    // -----------------------------------------------------------------

    /**
     * Send a [ContactRequest] envelope to `@recipientUsername`,
     * sealed under the recipient's published X25519 key.
     *
     * Throws [ContactsError]. Inspect [ContactsError.variant]
     * for routing.
     */
    public suspend fun sendContactRequest(
        senderSigningSeed: ByteArray,
        senderAccountId: ByteArray,
        senderSignaturePubkey: ByteArray,
        recipientUsername: String,
        introMessage: ByteArray,
        /** ADR-0048 §2 — the sender's own profile, carried inside the sealed
         *  envelope so the recipient sees who is asking. `null` = none. */
        senderProfile: FfiProfile? = null,
        /** ADR-0060 §6 — mints (and registers) the sender's `reply` grant for
         *  the recipient named by their signature key, sealed in the request
         *  so the recipient can answer into the sender's closed mailbox. */
        mintReply: (suspend (recipientSignaturePubkey: ByteArray) -> FfiReachGrant?)? = null,
        /** ADR-0060 §2 — the grant authorising a write into the recipient's
         *  mailbox (found through search), by their signature key. */
        reachFor: ((recipientSignaturePubkey: ByteArray) -> DsReachAuth?)? = null,
    ): SendResult {
        // 1. Resolve the username.
        val lookup = try {
            discovery.lookup(recipientUsername)
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryClientArgError) {
            throw ContactsError.wrap(e)
        }
        val recipientAccountId = lookup.account
            ?: throw ContactsError.usernameUnknown(lookup.normalisedUsername)

        // 2. Fetch the recipient's identity bundle.
        val bundle = try {
            discovery.identityFetch(recipientAccountId)
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } ?: throw ContactsError.noIdentityBundle(recipientAccountId)

        // G-30 #2 (ADR-0019): snapshot the pin BEFORE gate 1 runs. Gate 1
        // re-pins to the new key on a verified rotation (overwriting the
        // pin), but gate 2's rotation-chain check needs the OLD pinned key +
        // the OLD chain-version floor to anchor the chain. Capturing it here
        // keeps gate 1 untouched.
        val oldPin = pinStore.readPin(base64Encode(recipientAccountId))

        // G-4 (ADR-0016) rule #2 — verify the fetched bundle's
        // AccountIdentity key against the local pin BEFORE using its
        // keys to seal a ContactRequest. A changed key with no valid
        // supersession proof throws [SupersessionRefused] (fail-
        // closed) and we never seal to the unverified new key. The
        // supersession gate runs FIRST (and may throw) so a pin always
        // exists for the additive log check below to record on.
        try {
            verifyBundleAccountKey(recipientAccountId, bundle)
        } catch (refused: SupersessionRefused) {
            // KT-log S4 — on a fail-closed refusal of an UNPROVEN CHANGED
            // key (the moment the reset-vs-compromise distinction matters
            // most), enrich the error with a BEST-EFFORT transparency
            // lookup of the NEW key's `is_reset` marker. This NEVER weakens
            // the gate: it already threw, we re-throw it unchanged save for
            // the added context. Any log fault leaves `loggedReset` null.
            throw enrichRefusal(refused, recipientAccountId, bundle, oldPin)
        }

        // KT-log S3e — ADDITIVE equivocation-detection layer on top of the
        // supersession/pin gate. HARD-FAILS only on a ROLLBACK (the log
        // shrank — an unambiguous attack, [LogRollbackDetected]); every
        // other verdict is recorded + surfaced but does NOT block (the log
        // is eventually-consistent and the supersession/pin gate is the
        // primary trust). NEVER weakens the gate above — a
        // [SupersessionRefused] propagates before this runs at all.
        // G-30 #2 (ADR-0019): pass the OLD pin + the bundle's supersession
        // proof so the log check can route a key ROTATION through the chain
        // verifier (anchoring the transition to the operator-signed log)
        // instead of the standalone single-key inclusion check.
        checkTransparencyLog(
            recipientAccountId,
            base64Decode(bundle.signature_pubkey),
            oldPin,
            bundle.supersession_proof,
        )

        val recipientSignaturePub = base64Decode(bundle.signature_pubkey)
        val recipientX25519Pub = base64Decode(bundle.x25519_pubkey)

        val publish = composeAndPublishContactRequest(
            senderSigningSeed = senderSigningSeed,
            senderAccountId = senderAccountId,
            senderSignaturePubkey = senderSignaturePubkey,
            recipientSignaturePubkey = recipientSignaturePub,
            recipientX25519Pubkey = recipientX25519Pub,
            introMessage = introMessage,
            senderProfile = senderProfile,
            replyGrant = mintReply?.invoke(recipientSignaturePub),
            reach = reachFor?.invoke(recipientSignaturePub),
        )

        return SendResult(
            recipientUsername = lookup.normalisedUsername,
            recipientAccountId = recipientAccountId,
            messageId = publish.messageId,
            createdAt = publish.createdAt,
        )
    }

    /**
     * Resolve `@username -> (accountId, signaturePubkey)` through the SAME
     * G-4-verified gate [sendContactRequest] runs: discovery lookup ->
     * identityFetch -> the supersession/pin verify (fail-closed
     * [SupersessionRefused] on an unproven changed key) + the additive KT-log
     * rollback check. Used by the group-chat add-member flow
     * ([me.lakko.app.chat.ChatRepository.addMemberToGroup]) — adding a member
     * seals a Welcome under the resolved key exactly like a 1x1
     * ContactRequest, so this verify MUST NOT be skipped.
     *
     * Mirrors the resolve+verify sequence above (kept in lock-step by hand —
     * any change to the gate there must be reflected here). Returns
     * `(accountId, signaturePubkey)`.
     */
    public suspend fun resolveVerifiedRecipient(
        username: String,
    ): Pair<ByteArray, ByteArray> {
        val lookup = try {
            discovery.lookup(username)
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryClientArgError) {
            throw ContactsError.wrap(e)
        }
        val recipientAccountId = lookup.account
            ?: throw ContactsError.usernameUnknown(lookup.normalisedUsername)

        val bundle = try {
            discovery.identityFetch(recipientAccountId)
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } ?: throw ContactsError.noIdentityBundle(recipientAccountId)

        val oldPin = pinStore.readPin(base64Encode(recipientAccountId))
        try {
            verifyBundleAccountKey(recipientAccountId, bundle)
        } catch (refused: SupersessionRefused) {
            throw enrichRefusal(refused, recipientAccountId, bundle, oldPin)
        }
        checkTransparencyLog(
            recipientAccountId,
            base64Decode(bundle.signature_pubkey),
            oldPin,
            bundle.supersession_proof,
        )
        return Pair(recipientAccountId, base64Decode(bundle.signature_pubkey))
    }

    /**
     * ADR-0065 phase 2 / UX 16.1 point 2 — the identity check for an account
     * already known by id (the scheduled device-list arm of ADR-0050 runs it
     * for every account in every open chat): fetch the bundle, run the SAME
     * fail-closed gate [sendContactRequest] runs, then the additive log check.
     * Returns the served bundle's signature key on a clean verify; throws
     * [SupersessionRefused] (enriched with the served key, the pinned key and
     * the log marker) on a changed key without a proof. Returns `null` when the
     * account has no bundle.
     */
    public suspend fun checkPeerIdentity(accountId: ByteArray): ByteArray? {
        val bundle = try {
            discovery.identityFetch(accountId)
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } ?: return null
        val oldPin = pinStore.readPin(base64Encode(accountId))
        try {
            verifyBundleAccountKey(accountId, bundle)
        } catch (refused: SupersessionRefused) {
            throw enrichRefusal(refused, accountId, bundle, oldPin)
        }
        checkTransparencyLog(
            accountId,
            base64Decode(bundle.signature_pubkey),
            oldPin,
            bundle.supersession_proof,
        )
        return base64Decode(bundle.signature_pubkey)
    }

    /**
     * Attach to a fail-closed refusal everything a reset-aware caller needs
     * (ADR-0065 phase 2): the key the directory served, its key generation,
     * the key that was pinned, and the best-effort log marker. The refusal is
     * re-thrown unchanged otherwise — the gate already decided.
     */
    private suspend fun enrichRefusal(
        refused: SupersessionRefused,
        accountId: ByteArray,
        bundle: IdentityFetchResponse,
        oldPin: Pin?,
    ): SupersessionRefused {
        refused.servedPubkeyB64 = bundle.signature_pubkey
        refused.servedKeyGeneration = bundle.key_generation
        refused.pinnedPubkeyB64 = oldPin?.pinnedSignaturePubkeyB64
        refused.loggedReset = bestEffortServedKeyReset(
            accountId,
            base64Decode(bundle.signature_pubkey),
        )
        return refused
    }

    /**
     * ADR-0048 §3 — resolve a username to the `account_id` discovery holds for
     * it, or `null` when nobody registered it. A plain lookup: it seals
     * nothing, so it runs no gate. Network / server failures throw
     * [ContactsError].
     */
    public suspend fun lookupAccountId(username: String): ByteArray? =
        try {
            discovery.lookup(username).account
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryClientArgError) {
            throw ContactsError.wrap(e)
        }

    /**
     * ADR-0048 §3 — the signature public key the identity bundle of
     * [accountId] currently names, or `null` when there is no bundle or its
     * key is not a 32-byte value.
     *
     * Deliberately NOT [verifyBundleAccountKey]: displaying a username is a
     * read, and the pin gate PINS a first-ever key as a side effect (TOFU). A
     * username check that merely rendered a name would otherwise decide which
     * key a later contact request or add-member trusts. The G-4 gate stays on
     * the paths that seal to a key ([sendContactRequest],
     * [resolveVerifiedRecipient]); this one reads and compares, as iOS and web
     * do. The pin store is never read or written here.
     */
    public suspend fun fetchPublishedSignatureKey(accountId: ByteArray): ByteArray? {
        val bundle = try {
            discovery.identityFetch(accountId)
        } catch (e: DiscoveryHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryNetworkError) {
            throw ContactsError.wrap(e)
        } catch (e: DiscoveryClientArgError) {
            throw ContactsError.wrap(e)
        } ?: return null
        val key = try {
            base64Decode(bundle.signature_pubkey)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return key.takeIf { it.size == SIGNATURE_KEY_LEN }
    }

    /**
     * Send a [ContactRequest] directly against a known recipient
     * bundle — i.e. **without** going through the discovery
     * `lookup → identityFetch` round-trip.
     *
     * The invite-link flow (1.15e6) is the canonical caller:
     * the `InviteCapability` already carries `ownerAccountId`,
     * `ownerSignaturePubkey`, and `ownerX25519Pubkey`, so the
     * joiner has everything needed to seal an envelope without
     * touching `services/discovery`. Useful both for the happy
     * path (faster onboarding from a link) and for the
     * censored-region path (the `/discovery/` route family may
     * be blocked while the link still works because the bytes
     * are inline). The literal `&#42;/` in the path glob was
     * deliberately dropped — it closes the surrounding KDoc
     * block prematurely if written inline.
     *
     * Returns the same [SendResult] shape [sendContactRequest]
     * produces. `recipientUsername` is left empty — the joiner
     * does not have it (no discovery round-trip happened).
     */
    public suspend fun sendContactRequestToBundle(
        senderSigningSeed: ByteArray,
        senderAccountId: ByteArray,
        senderSignaturePubkey: ByteArray,
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
        recipientX25519Pubkey: ByteArray,
        introMessage: ByteArray,
        /** ADR-0048 §2 — the sender's own profile; see [sendContactRequest]. */
        senderProfile: FfiProfile? = null,
        /** ADR-0060 §6 — the sender's own `reply` grant, sealed in the request. */
        replyGrant: FfiReachGrant? = null,
        /** ADR-0060 §2 — what authorises the write (a link grant from the
         *  capability, or a grant found through search). */
        reach: DsReachAuth? = null,
        /** UX 16.1 — the recipient is a contact flagged `reset-unverified`
         *  on this device; see [gateBundleRecipient]. */
        resetUnverified: Boolean = false,
    ): SendResult {
        // Before anything is composed: these keys did not come through the
        // fetch gate.
        gateBundleRecipient(recipientAccountId, recipientSignaturePubkey, resetUnverified)
        val publish = composeAndPublishContactRequest(
            senderSigningSeed = senderSigningSeed,
            senderAccountId = senderAccountId,
            senderSignaturePubkey = senderSignaturePubkey,
            recipientSignaturePubkey = recipientSignaturePubkey,
            recipientX25519Pubkey = recipientX25519Pubkey,
            introMessage = introMessage,
            senderProfile = senderProfile,
            replyGrant = replyGrant,
            reach = reach,
        )
        return SendResult(
            recipientUsername = "",
            recipientAccountId = recipientAccountId,
            messageId = publish.messageId,
            createdAt = publish.createdAt,
        )
    }

    /**
     * UX 16.1 / ADR-0016 rule 2 — the gate for keys handed to this client
     * instead of fetched through it ([bundleRecipientVerdict]). Throws
     * [SupersessionRefused] and sends nothing when the account is a flagged
     * contact, or is pinned to another key that the directory cannot explain.
     *
     * A pinned-but-different key is first put to the real gate
     * ([checkPeerIdentity]): a rotation carrying a valid proof re-pins there
     * and the send goes on; a reset throws the enriched refusal, which is
     * what flags the contact; anything else — no bundle, or a directory that
     * names a third key — is refused here. A network failure propagates, so
     * the answer is never "send" by default. A stranger (no pin) costs no
     * request and is never refused.
     */
    public suspend fun gateBundleRecipient(
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
        resetUnverified: Boolean,
    ) {
        val accountIdB64 = base64Encode(recipientAccountId)
        val recipientKeyB64 = base64Encode(recipientSignaturePubkey)
        val pinned = pinStore.readPin(accountIdB64)?.pinnedSignaturePubkeyB64
        when (bundleRecipientVerdict(resetUnverified, pinned, recipientKeyB64)) {
            BundleRecipientVerdict.SEND -> return
            BundleRecipientVerdict.REFUSE_RESET_UNVERIFIED ->
                throw SupersessionRefused(
                    reason = SupersessionRefusedReason.ProofAbsent,
                    accountIdB64 = accountIdB64,
                    message = "this contact's account keys were reset and the new " +
                        "safety number is not verified yet — nothing is sent.",
                ).also {
                    // The state is already recorded; this only makes the UI
                    // answer `Verify ... first` with `Open contact`.
                    it.loggedReset = true
                    it.pinnedPubkeyB64 = pinned
                }
            BundleRecipientVerdict.REFUSE_KEY_CHANGED -> {
                checkPeerIdentity(recipientAccountId)
                val now = pinStore.readPin(accountIdB64)?.pinnedSignaturePubkeyB64
                if (now != recipientKeyB64) {
                    throw SupersessionRefused(
                        reason = SupersessionRefusedReason.KeyMismatch,
                        accountIdB64 = accountIdB64,
                        message = "the keys named for this account are not the " +
                            "pinned ones and the directory does not authorise " +
                            "them — refusing. Please re-verify out of band.",
                    ).also { it.pinnedPubkeyB64 = now }
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // G-4 (ADR-0016): fetch-path supersession verify + re-pin
    // -----------------------------------------------------------------

    /**
     * G-4 (ADR-0016) — verify a freshly-fetched peer identity
     * bundle's AccountIdentity key against the local pin, re-pinning
     * (via a supersession proof) when the key rotated. Called after
     * every [DiscoveryClient.identityFetch] and BEFORE the bundle's
     * keys are used to send a [ContactRequest].
     *
     * Mirror of the web `ContactsClient.verifyBundleAccountKey`
     * (`apps/web/src/contacts/client.ts`).
     *
     * Decision table (ADR-0016 rule #2, fail-closed). The [pinStore] is
     * non-null by construction, so the gate always runs:
     *   1. No existing pin → TOFU-pin the fetched key (marker `0`).
     *      Trust-on-first-use is unchanged for a first observation.
     *   2. Pin matches the fetched `signature_pubkey` → proceed (no
     *      key change; the common steady-state case).
     *   3. Pin DIFFERS → REQUIRE `bundle.supersession_proof`:
     *        a. absent → throw [SupersessionRefused] (`ProofAbsent`).
     *        b. [verifySupersession] throws → [SupersessionRefused]
     *           (`ProofInvalid`).
     *        c. the verifier's authorised new key !=
     *           `bundle.signature_pubkey` → [SupersessionRefused]
     *           (`KeyMismatch`) — defence in depth.
     *        d. re-pin advancing the monotonic marker to the proof's
     *           expiry; a backward marker → [SupersessionRefused]
     *           (`MarkerBackward`).
     *      Only on a clean verify do we proceed with the new key.
     *
     * NEVER blind-TOFU a CHANGED key — that is the whole point of
     * the gate.
     *
     * @param accountId  The peer's raw 16-byte account_id (the
     *                   stable pin key, NOT the pubkey — rule #3).
     * @param bundle     The fetched [IdentityFetchResponse].
     * @throws SupersessionRefused on any fail-closed branch.
     *
     * `internal` (not `private`) so the instrumented test suite can
     * drive the decision table directly with the real native lib +
     * an [InMemoryPinStore], without standing up the discovery +
     * DS round-trip. Not part of the public surface.
     */
    internal fun verifyBundleAccountKey(
        accountId: ByteArray,
        bundle: IdentityFetchResponse,
    ) {
        // `pinStore` is non-null by construction (see the ctor doc), so the
        // gate can never be silently skipped — it always runs.
        val store = pinStore
        val accountIdB64 = base64Encode(accountId)
        val fetchedPubkeyB64 = bundle.signature_pubkey

        val existing = store.readPin(accountIdB64)

        // Case 2 — first-ever observation. TOFU-pin and proceed.
        if (existing == null) {
            store.setPin(accountIdB64, fetchedPubkeyB64, 0L)
            return
        }

        // Case 3 — unchanged key. Nothing to verify.
        if (existing.pinnedSignaturePubkeyB64 == fetchedPubkeyB64) {
            return
        }

        // Case 4 — the AccountIdentity key changed. A proof is now
        // mandatory; absence is an immediate fail-closed refusal.
        val proofB64 = bundle.supersession_proof
        if (proofB64.isNullOrEmpty()) {
            throw SupersessionRefused(
                reason = SupersessionRefusedReason.ProofAbsent,
                accountIdB64 = accountIdB64,
                message = "the peer's account key changed and no " +
                    "supersession proof was provided — refusing to trust " +
                    "the new key. Please re-verify this contact out of band.",
            )
        }

        val proofBytes = try {
            base64Decode(proofB64)
        } catch (cause: IllegalArgumentException) {
            throw SupersessionRefused(
                reason = SupersessionRefusedReason.ProofInvalid,
                accountIdB64 = accountIdB64,
                message = "the supersession proof was not valid base64 " +
                    "($cause) — refusing the changed key. Please re-verify " +
                    "out of band.",
            )
        }
        val pinnedOldPubkey = try {
            base64Decode(existing.pinnedSignaturePubkeyB64)
        } catch (cause: IllegalArgumentException) {
            // The stored pin itself is malformed — fail closed.
            throw SupersessionRefused(
                reason = SupersessionRefusedReason.ProofInvalid,
                accountIdB64 = accountIdB64,
                message = "the locally-pinned key was malformed ($cause) — " +
                    "refusing the changed key. Please re-verify out of band.",
            )
        }
        val nowSecs = System.currentTimeMillis() / 1000L

        // The verifier returns the authorised NEW pubkey, or THROWS
        // on a malformed / wrong-old-key / expired / out-of-window
        // proof.
        val authorisedNewPubkey: ByteArray = try {
            verifySupersession(proofBytes, pinnedOldPubkey, nowSecs)
        } catch (cause: FfiException) {
            throw SupersessionRefused(
                reason = SupersessionRefusedReason.ProofInvalid,
                accountIdB64 = accountIdB64,
                message = "the peer's account key changed and the " +
                    "supersession proof did not verify against the pinned " +
                    "key ($cause) — refusing the new key. Please re-verify " +
                    "out of band.",
            )
        }

        // Defence in depth — the proof must authorise EXACTLY the
        // key the bundle served. A proof that verifies but names a
        // third key is rejected (a malicious server splicing an
        // unrelated valid proof onto a substituted key).
        val authorisedNewPubkeyB64 = base64Encode(authorisedNewPubkey)
        if (authorisedNewPubkeyB64 != fetchedPubkeyB64) {
            throw SupersessionRefused(
                reason = SupersessionRefusedReason.KeyMismatch,
                accountIdB64 = accountIdB64,
                message = "the supersession proof verified but authorised a " +
                    "different key than the bundle served — refusing. " +
                    "Please re-verify out of band.",
            )
        }

        // Advance the monotonic marker (ADR-0016 rule #4) to the
        // proof's own `expires_at`. A genuine later rotation carries
        // a strictly-newer `issued_at` (hence a newer `expires_at`
        // for a fixed emit window), so a stale older successor's
        // proof — whose `expires_at` predates the current marker —
        // is refused. Re-observing the SAME transition re-pins at
        // the same marker (`>=`, allowed). We read `expires_at` from
        // the already-verified proof bytes (the verifier confirmed
        // the signature over exactly these fields, so the parse is
        // on trusted data).
        val proofExpiresAt = readProofExpiresAtSecs(proofBytes)
        val result = store.setPin(accountIdB64, fetchedPubkeyB64, proofExpiresAt)
        if (result == SetPinResult.RejectedBackward) {
            throw SupersessionRefused(
                reason = SupersessionRefusedReason.MarkerBackward,
                accountIdB64 = accountIdB64,
                message = "the supersession proof would move the rotation " +
                    "marker backwards (an older successor) — refusing. " +
                    "Please re-verify out of band.",
            )
        }
        // ADR-0066 section 3.5 - a PROVEN rotation: the key pinned until
        // now stays the binding of the conversation leaves that exist, as
        // the same person. Nothing is flagged, closed, evicted or shown
        // (the ADR-0065 phase-2 "keys reset" state is entered only by a
        // refusal of this gate, which did not happen here).
        try {
            onProvenRotation(accountIdB64, existing.pinnedSignaturePubkeyB64, fetchedPubkeyB64)
        } catch (cause: Throwable) {
            Log.w("lakko-contacts", "proven predecessor not recorded: ${cause.javaClass.simpleName}")
        }
        Log.i(
            "lakko-contacts",
            "supersession: re-pinned account $accountIdB64 to a verified " +
                "rotated key (marker $proofExpiresAt)",
        )
    }

    /**
     * KT-log S4 — best-effort lookup of the served (refused) key's
     * `is_reset` marker, used ONLY to enrich a [SupersessionRefused].
     *
     * Mirror of the web `ContactsClient.bestEffortServedKeyReset`. Wraps
     * [lookupServedKeyReset] (which itself never throws) and swallows any
     * throw — including a factory misconfiguration — to a `null`, so the
     * enrichment can never break the already-failing refusal path. Returns
     * `true` / `false` only when the log's latest entry commits to exactly
     * the served key; `null` otherwise (off / unreachable / absent /
     * pubkey-divergence).
     *
     * `internal` (not `private`) so the instrumented test suite can drive
     * it directly against a `MockEngine`-backed transparency client.
     *
     * @param accountId    the peer's raw 16-byte account_id.
     * @param servedPubkey the 32-byte Ed25519 key discovery served (the
     *                     refused NEW key).
     */
    internal suspend fun bestEffortServedKeyReset(
        accountId: ByteArray,
        servedPubkey: ByteArray,
    ): Boolean? {
        val client = try {
            transparencyClientFactory()
        } catch (cause: Throwable) {
            // A factory that throws (misconfiguration) must NOT break the
            // refusal enrichment — the marker is simply unknown.
            Log.w(
                "lakko-contacts",
                "transparency: client factory threw during best-effort reset " +
                    "lookup (refusal stands; loggedReset stays null)",
                cause,
            )
            return null
        }
        if (client == null) return null
        return try {
            client.use { c ->
                lookupServedKeyReset(
                    accountId = accountId,
                    servedSignaturePubkey = servedPubkey,
                    client = c,
                )
            }
        } catch (cause: Throwable) {
            // lookupServedKeyReset is designed not to throw, but the .use
            // close() or any unexpected path must never break the refusal.
            Log.w(
                "lakko-contacts",
                "transparency: best-effort reset lookup threw (refusal " +
                    "stands; loggedReset stays null)",
                cause,
            )
            null
        }
    }

    /**
     * KT-log S3e — consult the operator-signed transparency log to confirm
     * the served identity is in the public append-only log (equivocation
     * detection on top of the supersession/pin gate).
     *
     * Mirror of the web `ContactsClient.checkTransparencyLog`. Runs AFTER
     * [verifyBundleAccountKey]'s supersession gate has TOFU-pinned /
     * verified the key, so a pin always exists when we record the verdict.
     * Best-effort POSTURE (ADR-0016 stays the primary, fail-closed trust):
     *
     *   * [LogVerifyStatus.ROLLBACK]  → HARD FAIL. Throw
     *     [LogRollbackDetected]. The log shrank — an unambiguous attack.
     *     This is the ONLY blocking verdict for S3e.
     *   * [LogVerifyStatus.MISMATCH]  → record + prominent [Log.w], but DO
     *     NOT block (S3e). Hard-blocking on mismatch is the natural
     *     fail-closed refinement and is flagged for `lakko-security` to
     *     rule on.
     *   * [LogVerifyStatus.ABSENT] / [LogVerifyStatus.UNREACHABLE] →
     *     record + a [Log.w] and proceed. The log is eventually-consistent;
     *     the supersession/pin gate already authorised the key.
     *   * [LogVerifyStatus.CONFIRMED] / [LogVerifyStatus.OFF] → quiet (the
     *     happy path / disabled).
     *
     * The recorded status travels on the account's pin record
     * ([PinStore.recordLogStatus]) so the UI can surface it. Any UNEXPECTED
     * throw from the log machinery (it is designed not to throw except for
     * the deliberate rollback, which it surfaces as the
     * [LogVerifyStatus.ROLLBACK] return) is swallowed + logged — the
     * additive check must never break the primary fetch path.
     *
     * G-30 #2 (ADR-0019): when the served key is a ROTATION away from the
     * pinned one AND the bundle carried a supersession proof, the check
     * routes through [verifyRotationChain] (the chain verifier — root sig +
     * no-rollback + Merkle inclusion + the supersession proof from the OLD
     * pinned key + a strictly-newer logged version + freshness) instead of
     * the standalone single-key [verifyIdentityInLog]. The POSTURE is
     * unchanged: only [LogVerifyStatus.ROLLBACK] hard-fails; a chain
     * contradiction surfaces as [LogVerifyStatus.MISMATCH] (NOT a block); a
     * not-logged / unreachable / off rotation falls back to the standalone
     * gate-1 result (no over-block). On a [LogVerifyStatus.CONFIRMED] chain
     * the proven log version is persisted as the next rotation's downgrade
     * floor.
     *
     * `internal` (not `private`) so the instrumented test suite can drive
     * the verdict→posture mapping directly against a `MockEngine`-backed
     * transparency client + the real native lib.
     *
     * @param accountId    the peer's raw 16-byte account_id.
     * @param servedPubkey the 32-byte Ed25519 key discovery served.
     * @param oldPin       the pin AS IT WAS BEFORE gate 1 re-pinned (G-30
     *                     #2) — `null` on a first-ever observation.
     * @param supersessionProofB64 the bundle's `supersession_proof` (base64),
     *                     when present — the chain's old-key authorisation.
     * @throws LogRollbackDetected ONLY on a [LogVerifyStatus.ROLLBACK].
     */
    internal suspend fun checkTransparencyLog(
        accountId: ByteArray,
        servedPubkey: ByteArray,
        oldPin: Pin? = null,
        supersessionProofB64: String? = null,
    ) {
        val accountIdB64 = base64Encode(accountId)
        // Build the transparency client (OFF → null → verdict OFF below).
        val client = try {
            transparencyClientFactory()
        } catch (cause: Throwable) {
            // A factory that throws (misconfiguration) must NOT break the
            // primary fetch path — treat as OFF.
            Log.w(
                "lakko-contacts",
                "transparency: client factory threw; skipping the additive " +
                    "log check (supersession/pin gate already authorised " +
                    "the key)",
                cause,
            )
            return
        }

        // G-30 #2 — a ROTATION is: a pin existed, its key DIFFERS from the
        // served key, and the bundle carried a supersession proof. Only then
        // can the chain verifier anchor the transition. Anything else (first
        // observation / unchanged key / no proof) keeps the existing
        // single-key inclusion check unchanged.
        val isRotation =
            oldPin != null &&
                oldPin.pinnedSignaturePubkeyB64 != base64Encode(servedPubkey) &&
                !supersessionProofB64.isNullOrEmpty()

        // The verdict triple. `version` is non-null ONLY on a CONFIRMED
        // rotation chain; the single-key inclusion path leaves it null.
        val verdict: Triple<LogVerifyStatus, Boolean?, Long?> = try {
            client.use { c ->
                if (isRotation) {
                    // `oldPin` + `supersessionProofB64` are non-null in this
                    // branch (the `isRotation` guard). A malformed-base64
                    // proof or pinned key is treated like any log fault — the
                    // chain check is best-effort and never blocks the fetch.
                    val supersessionProof = base64Decode(supersessionProofB64)
                    val pinnedOldPubkey = base64Decode(oldPin.pinnedSignaturePubkeyB64)
                    val result = verifyRotationChain(
                        accountId = accountId,
                        servedPubkey = servedPubkey,
                        supersessionProof = supersessionProof,
                        pinnedOldPubkey = pinnedOldPubkey,
                        pinnedLogVersion = oldPin.pinnedLogVersion ?: 0L,
                        client = c,
                        operatorKey = TransparencyConfig.pinnedOperatorKey(),
                        lastSeenRoot = lastSeenRoot,
                        witnessKeys = TransparencyConfig.pinnedWitnessKeys(),
                        witnessThreshold = TransparencyConfig.witnessThreshold(),
                    )
                    Triple(result.status, result.isReset, result.version)
                } else {
                    // ADR-0036 — resolve the pinned auth attest verifying key
                    // (reused for genesis-attest per §3) best-effort. A
                    // resolution failure simply skips the additive
                    // genesis-attest check; the self-sig + supersession/pin gate
                    // remain the primary trust. A build-time pin
                    // (BuildConfig.LAKKO_DEVICE_ATTEST_KEY) short-circuits the
                    // fetch, so this is a no-network path in the pinned posture.
                    val genesisAttestKey = try {
                        DeviceAttestKey.fetch()
                    } catch (cause: Throwable) {
                        Log.w(
                            "lakko-contacts",
                            "transparency: could not resolve the pinned auth " +
                                "attest key; skipping the additive ADR-0036 " +
                                "genesis-attest check (self-sig + pin gate stand)",
                            cause,
                        )
                        null
                    }
                    val result = verifyIdentityInLog(
                        accountId = accountId,
                        publishedSignaturePubkey = servedPubkey,
                        client = c,
                        operatorKey = TransparencyConfig.pinnedOperatorKey(),
                        lastSeenRoot = lastSeenRoot,
                        witnessKeys = TransparencyConfig.pinnedWitnessKeys(),
                        witnessThreshold = TransparencyConfig.witnessThreshold(),
                        genesisAttestKey = genesisAttestKey,
                    )
                    Triple(result.status, result.isReset, null)
                }
            }
        } catch (cause: Throwable) {
            // The verifier is designed to map faults to a status and only
            // signal the rollback semantics via the [LogVerifyStatus.ROLLBACK]
            // RETURN (not a throw). An unexpected throw here must NOT break
            // the additive check — log + proceed.
            Log.w(
                "lakko-contacts",
                "transparency: log check threw unexpectedly (proceeding; " +
                    "supersession/pin gate already authorised the key)",
                cause,
            )
            return
        }
        val status = verdict.first
        val isReset = verdict.second
        val version = verdict.third

        // Rollback is the one hard failure (unambiguous attack).
        if (status == LogVerifyStatus.ROLLBACK) {
            throw LogRollbackDetected(accountIdB64)
        }

        // Record the verdict on the pin record (best-effort; the pin
        // exists because the supersession gate ran first). KT-log S4 also
        // persists the `/latest` entry's `is_reset` marker next to the
        // status (a `null` clears any stale marker — no latest was read).
        pinStore.recordLogStatus(accountIdB64, status)
        pinStore.recordLogReset(accountIdB64, isReset)

        // G-30 #2 — persist the chain-confirmed log version ONLY on a
        // CONFIRMED rotation chain (where `version != null`). A transient
        // UNREACHABLE / MISMATCH / ABSENT verdict must NOT clear a
        // previously-confirmed floor, so we never pass `null` here.
        version?.let { pinStore.recordLogVersion(accountIdB64, it) }

        when (status) {
            LogVerifyStatus.MISMATCH ->
                // Surfaced-but-not-blocked for S3e. The natural fail-closed
                // refinement (hard-block on mismatch) is deferred to a
                // lakko-security ruling — see the method doc + the S3e report.
                Log.w(
                    "lakko-contacts",
                    "transparency: MISMATCH for account $accountIdB64 — the " +
                        "log's latest key differs from what discovery served " +
                        "(or the inclusion proof failed). Proceeding for S3e " +
                        "(supersession/pin gate is primary); flag for review.",
                )
            LogVerifyStatus.ABSENT, LogVerifyStatus.UNREACHABLE ->
                Log.w(
                    "lakko-contacts",
                    "transparency: check inconclusive ($status) for account " +
                        "$accountIdB64 — proceeding; the log is eventually-" +
                        "consistent and the pin gate is primary.",
                )
            // CONFIRMED / OFF are quiet (the happy path / disabled).
            LogVerifyStatus.CONFIRMED, LogVerifyStatus.OFF -> Unit
            // ROLLBACK already threw above; unreachable here.
            LogVerifyStatus.ROLLBACK -> Unit
        }
    }

    /**
     * Shared compose-and-publish path used by both public send
     * surfaces. Pulled out so the two flows don't drift on
     * subtle details (mailbox-pseudonym derivation, envelope
     * kind wrapping, sender-pseudonym choice).
     *
     * The recipient's mailbox is keyed by their public-pubkey
     * pseudonym — anyone holding the recipient's Ed25519
     * identity pubkey can compute the same 32-byte value, by
     * design. This is the routing address for cold contact
     * requests (see ADR-0005 §13.7).
     *
     * The sender's `from_pseudonym` is also their identity
     * pseudonym (public, derivable from the sender's pubkey);
     * the receiver uses it to address a future ack. Per-
     * relationship pseudonyms (which rotate after the handshake
     * via [uniffi.lakko_ffi_uniffi.PerRelationship.derive]) are
     * 1.15e6+ work.
     */
    private suspend fun composeAndPublishContactRequest(
        senderSigningSeed: ByteArray,
        senderAccountId: ByteArray,
        senderSignaturePubkey: ByteArray,
        recipientSignaturePubkey: ByteArray,
        recipientX25519Pubkey: ByteArray,
        introMessage: ByteArray,
        senderProfile: FfiProfile?,
        replyGrant: FfiReachGrant?,
        reach: DsReachAuth?,
    ): PublishedRequest {
        // Recipient mailbox pseudonym = BLAKE3(domain || their pubkey).
        val recipientPseudonym =
            IdentityPseudonym.fromIdentity(recipientSignaturePubkey).use { it.asBytes() }

        // Sender's from_pseudonym = their own identity pseudonym.
        val senderPseudonym =
            IdentityPseudonym.fromIdentity(senderSignaturePubkey).use { it.asBytes() }

        val validAfterMs = System.currentTimeMillis().toULong()

        // 1.9b2: Privacy Pass / Play Integrity attestation blob.
        // The Android dev path ships an empty token — wiring the
        // real Play Integrity round-trip belongs in the 1.20+
        // Privacy Pass sub-sprint, gated on Google's dev-account
        // provisioning. The empty value lands the request in the
        // DS ingest layer's "no attestation tier" rate-limit
        // bucket (lowest ceiling).
        val privacyPassToken = ByteArray(0)

        val wireBytes = try {
            composeContactRequest(
                senderSigningSeed = senderSigningSeed,
                senderAccountId = senderAccountId,
                recipientX25519Pub = recipientX25519Pubkey,
                fromPseudonym = senderPseudonym,
                validAfterMs = validAfterMs,
                privacyPassToken = privacyPassToken,
                // ADR-0048 §2 — sealed to the recipient with the intro; the DS
                // sees only ciphertext. A profile the composer refuses throws
                // here and the request is not sent (never a weakened form).
                profile = senderProfile,
                // ADR-0060 §6 — the sender's reply key, so the recipient can
                // write the accept (the 1:1 Welcome) and the ack back into the
                // sender's closed mailbox.
                replyGrant = replyGrant,
                introMessage = introMessage,
            )
        } catch (e: FfiException) {
            throw ContactsError.wrap(e)
        }

        val envelope = wrapEnvelope(KIND_CONTACT_REQUEST, wireBytes)
        val publish = try {
            ds.publishMailbox(recipientPseudonym, envelope, reach = reach)
        } catch (e: DsReachRefusedError) {
            throw ContactsError.reachRefused(e)
        } catch (e: DsHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DsNetworkError) {
            throw ContactsError.wrap(e)
        } catch (e: DsClientArgError) {
            throw ContactsError.wrap(e)
        }
        return PublishedRequest(messageId = publish.id, createdAt = publish.createdAt)
    }

    // -----------------------------------------------------------------
    // ContactAck (1.9b1): recipient's accept-reply
    // -----------------------------------------------------------------

    /**
     * Send a `ContactAck` envelope back to the original sender's
     * stranger inbox (Phase 1.9b1).
     *
     * Called from the recipient's "Accept" handler **after** the
     * user has decided to accept an inbound [IncomingContactRequest].
     * The ack is independent of MLS group creation — splitting
     * the two surfaces lets the original sender's UI flip from
     * "pending" to "accepted" without waiting on the joiner's
     * `startGroupWith` to complete.
     *
     * **Routing:** the ack body carries
     * `ackToPseudonym = original.fromPseudonym`. The publish target
     * is the sender's stranger-inbox mailbox — derived from
     * `original.fromPubkey` via [IdentityPseudonym.fromIdentity].
     */
    public suspend fun sendContactAck(
        accepterSigningSeed: ByteArray,
        accepterAccountId: ByteArray,
        accepterX25519Pubkey: ByteArray,
        originalRequest: IncomingContactRequest,
        /** ADR-0060 §6 — the requester's `reply` grant, which authorises the
         *  ack into their closed mailbox. */
        reach: DsReachAuth? = null,
    ): PublishedAck {
        val senderMailbox =
            IdentityPseudonym.fromIdentity(originalRequest.fromPubkey).use { it.asBytes() }

        val acceptedAtMs = System.currentTimeMillis().toULong()

        val wireBytes = try {
            composeContactAck(
                accepterSigningSeed = accepterSigningSeed,
                accepterAccountId = accepterAccountId,
                accepterX25519Pub = accepterX25519Pubkey,
                ackToPseudonym = originalRequest.fromPseudonym,
                acceptedAtMs = acceptedAtMs,
            )
        } catch (e: FfiException) {
            throw ContactsError.wrap(e)
        }

        val envelope = wrapEnvelope(KIND_CONTACT_ACK, wireBytes)
        val publish = try {
            ds.publishMailbox(senderMailbox, envelope, reach = reach)
        } catch (e: DsHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DsNetworkError) {
            throw ContactsError.wrap(e)
        } catch (e: DsClientArgError) {
            throw ContactsError.wrap(e)
        }
        return PublishedAck(messageId = publish.id, createdAt = publish.createdAt)
    }

    // -----------------------------------------------------------------
    // Poll: fetch stranger inbox + decode envelopes
    // -----------------------------------------------------------------

    /**
     * Poll the user's stranger-inbox mailbox for new
     * [ContactRequest]s + [IncomingMailboxEvent.Welcome] blobs.
     *
     * Malformed entries on the wire are **dropped silently** —
     * the verifier rejects bad signatures, decoding errors are
     * logged via [Log.w] and skipped (the cursor still
     * advances, so we won't re-process the same row). Every
     * honest client wants to keep moving past corrupted entries
     * rather than surface "garbage on the wire" to the UI.
     *
     * @param mySignaturePubkey  Caller's Ed25519 verifying key
     *                           (32 bytes). Mailbox pseudonym is
     *                           [IdentityPseudonym.fromIdentity]
     *                           of this.
     * @param myX25519Secret     Caller's 32-byte X25519 secret —
     *                           used to unseal each
     *                           ContactRequest envelope.
     * @param after              Cursor from the previous poll.
     *                           `0` on first call to fetch the
     *                           entire backlog.
     * @param waitSecs           Long-poll budget in seconds
     *                           (`0` returns immediately).
     */
    public suspend fun pollStrangerInbox(
        mySignaturePubkey: ByteArray,
        myX25519Secret: ByteArray,
        after: Long = 0,
        waitSecs: Int = 0,
        /** ADR-0053 / ADR-0060 §2 — the identity proof signer of this mailbox:
         *  every read of a personal mailbox is signed (an unsigned one is
         *  `401`), and a refusal claims the mailbox and retries once. */
        mailboxSigner: DsJournalSigner? = null,
    ): PollResult {
        val myPseudonym =
            IdentityPseudonym.fromIdentity(mySignaturePubkey).use { it.asBytes() }

        val raw = try {
            ds.fetchMailbox(myPseudonym, after = after, waitSecs = waitSecs, signer = mailboxSigner)
        } catch (e: DsHttpError) {
            throw ContactsError.wrap(e)
        } catch (e: DsNetworkError) {
            throw ContactsError.wrap(e)
        }

        val events = mutableListOf<IncomingMailboxEvent>()
        var cursor = after
        for (message in raw.messages) {
            cursor = message.id

            when (val env = parseEnvelope(message.payload)) {
                is ParsedEnvelope.Unknown -> {
                    // Future / unknown kind byte — cursor still
                    // advances so we don't re-process. No
                    // logger plumbed yet (see the
                    // ContactRequest branch below for the
                    // rationale).
                    continue
                }

                is ParsedEnvelope.Welcome -> {
                    events.add(
                        IncomingMailboxEvent.Welcome(
                            messageId = message.id,
                            welcomeBytes = env.body,
                            createdAt = message.createdAt,
                        ),
                    )
                }

                is ParsedEnvelope.ContactAck -> {
                    // 1.9b1: signature is verified inside the UniFFI
                    // surface; bad-signature / malformed-wire entries
                    // drop silently and the cursor still advances.
                    val parsed = try {
                        ContactAck.decodeAndVerify(env.body)
                    } catch (_: FfiException) {
                        continue
                    }
                    val ack = parsed.use { a ->
                        IncomingContactAck(
                            messageId = message.id,
                            accepterPubkey = a.accepterPubkey(),
                            accepterAccountId = a.accepterAccountId(),
                            accepterX25519Pubkey = a.accepterX25519Pubkey(),
                            ackToPseudonym = a.ackToPseudonym(),
                            acceptedAtMs = a.acceptedAtMs(),
                            createdAt = message.createdAt,
                        )
                    }
                    events.add(IncomingMailboxEvent.ContactAck(ack))
                }

                is ParsedEnvelope.ContactRequest -> {
                    val parsed = try {
                        ContactRequest.decodeAndVerify(env.body)
                    } catch (_: FfiException) {
                        // Bad signature, malformed wire bytes,
                        // or some other verifier failure. Drop
                        // and move on — the cursor advances so
                        // we won't re-process the same row.
                        // No logger plumbed yet; production
                        // will wire `tracing`-equivalent in
                        // 1.15d7 / 1.15f. Silence here matches
                        // the web's console.warn that
                        // production-builds-suppress.
                        continue
                    }
                    // Read every field + try to open the
                    // envelope BEFORE the handle closes. `.use`
                    // wraps the whole block so the Rust-side
                    // Arc releases on every exit (including the
                    // unseal-throw path).
                    val request = parsed.use { req ->
                        // ADR-0048 §2 — the sealed plaintext carries the
                        // sender's profile ahead of the intro. A profile that
                        // failed its bounds opens as `null` with the intro
                        // intact; the request is still authentic.
                        var senderProfile: me.lakko.app.profile.PeerProfile? = null
                        var replyGrant: FfiReachGrant? = null
                        val intro: String? = try {
                            val opened = req.openEnvelope(myX25519Secret)
                            // ADR-0060 §6 — the requester's reply key, bound
                            // to the verified `from_pubkey`: kept by the
                            // repository for the accept and the ack.
                            replyGrant = opened.replyGrant
                            senderProfile = opened.profile?.let { p ->
                                me.lakko.app.profile.PeerProfile(
                                    displayName = p.displayName,
                                    username = p.username,
                                    updatedAtMs = p.updatedAtMs,
                                    accountIdHex = me.lakko.app.profile.accountIdHexOrNull(p.accountId),
                    // ADR-0060 §5 — the username shows only on its certificate.
                    usernameCertificateHex = p.usernameCertificate?.let { me.lakko.app.reach.reachHex(it) },
                                )
                            }
                            // UTF-8 decode with replacement-on-
                            // error so malformed bytes still
                            // surface as the best-effort string
                            // we can show.
                            String(opened.introMessage, Charsets.UTF_8)
                        } catch (_: FfiException) {
                            // Envelope didn't decrypt for our
                            // X25519 secret. Could be a malicious
                            // request, could be a bug; either way
                            // we still surface the request (the
                            // outer signature is verified) but
                            // with no intro body.
                            null
                        }
                        IncomingContactRequest(
                            messageId = message.id,
                            fromPubkey = req.fromPubkey(),
                            fromAccountId = req.fromAccountId(),
                            fromPseudonym = req.fromPseudonym(),
                            validAfterMs = req.validAfterMs(),
                            intro = intro,
                            createdAt = message.createdAt,
                            profile = senderProfile,
                            replyGrant = replyGrant,
                        )
                    }
                    events.add(IncomingMailboxEvent.ContactRequest(request))
                }
            }
        }

        return PollResult(events = events, cursor = cursor, hasMore = raw.hasMore)
    }
}

// =============================================================================
// Result types
// =============================================================================

/** Internal carrier for [ContactsClient.composeAndPublishContactRequest]. */
private data class PublishedRequest(
    val messageId: Long,
    val createdAt: String,
)

/** Result of [ContactsClient.sendContactRequest] and
 *  [ContactsClient.sendContactRequestToBundle]. */
public data class SendResult(
    /** Normalised form of the username that was resolved, or
     *  `""` if the caller used [ContactsClient.sendContactRequestToBundle]
     *  (no discovery round-trip happened). */
    val recipientUsername: String,
    /** Raw 16-byte recipient account_id. */
    val recipientAccountId: ByteArray,
    /** DS-assigned monotonic id for the published envelope. */
    val messageId: Long,
    /** ISO-8601 timestamp from the DS. */
    val createdAt: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is SendResult) return false
        return recipientUsername == other.recipientUsername &&
            recipientAccountId.contentEquals(other.recipientAccountId) &&
            messageId == other.messageId &&
            createdAt == other.createdAt
    }

    override fun hashCode(): Int {
        var result = recipientUsername.hashCode()
        result = 31 * result + recipientAccountId.contentHashCode()
        result = 31 * result + messageId.hashCode()
        result = 31 * result + createdAt.hashCode()
        return result
    }
}

/** One verified incoming ContactRequest. */
public data class IncomingContactRequest(
    val messageId: Long,
    /** Sender's Ed25519 identity public key (32 bytes). The
     *  signature on the request was verified against this. */
    val fromPubkey: ByteArray,
    /** Sender's account_id (16-byte UUIDv7). `null` for legacy
     *  v1 envelopes — the Accept-driven group creation path
     *  requires this and the UI surfaces a "legacy request,
     *  please ask the sender to upgrade" message when it's
     *  missing. */
    val fromAccountId: ByteArray?,
    /** Sender's per-recipient pseudonym (32 bytes). */
    val fromPseudonym: ByteArray,
    val validAfterMs: ULong,
    /** UTF-8-decoded intro message, or `null` when there was
     *  no intro / it didn't decrypt. Empty string is a valid
     *  payload. */
    val intro: String?,
    val createdAt: String,
    /** ADR-0048 §2 — the sender's profile from the sealed envelope, bound to
     *  [fromPubkey] (which the request signature authenticates). A CLAIM: its
     *  username is shown only once verified. `null` when none was carried, it
     *  failed its bounds, or the envelope did not open. */
    val profile: me.lakko.app.profile.PeerProfile? = null,
    /** ADR-0060 §6 — the sender's `reply` grant from the sealed envelope,
     *  bound to [fromPubkey]. SECRET: kept by the reach manager, never
     *  persisted with the pending row. `null` when none was carried. */
    val replyGrant: FfiReachGrant? = null,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is IncomingContactRequest) return false
        return messageId == other.messageId &&
            fromPubkey.contentEquals(other.fromPubkey) &&
            (fromAccountId?.contentEquals(other.fromAccountId) ?: (other.fromAccountId == null)) &&
            fromPseudonym.contentEquals(other.fromPseudonym) &&
            validAfterMs == other.validAfterMs &&
            intro == other.intro &&
            createdAt == other.createdAt &&
            profile == other.profile
    }

    override fun hashCode(): Int {
        var result = messageId.hashCode()
        result = 31 * result + fromPubkey.contentHashCode()
        result = 31 * result + (fromAccountId?.contentHashCode() ?: 0)
        result = 31 * result + fromPseudonym.contentHashCode()
        result = 31 * result + validAfterMs.hashCode()
        result = 31 * result + (intro?.hashCode() ?: 0)
        result = 31 * result + createdAt.hashCode()
        result = 31 * result + (profile?.hashCode() ?: 0)
        return result
    }
}

/** One verified incoming [ContactAck] (Phase 1.9b1).
 *
 *  The accept-reply that closes the [ContactsClient.sendContactRequest]
 *  handshake from the original sender's perspective. The Ed25519
 *  signature on the ack has already been verified against
 *  [accepterPubkey]; the caller still has to match
 *  [ackToPseudonym] against the pending-sent table to pair it
 *  with the original request the user fired off. */
public data class IncomingContactAck(
    val messageId: Long,
    /** Accepter's Ed25519 identity public key (32 bytes). The
     *  signature on the ack was verified against this. */
    val accepterPubkey: ByteArray,
    /** Accepter's `account_id` (16-byte UUIDv7). */
    val accepterAccountId: ByteArray,
    /** Accepter's X25519 HPKE public key (32 bytes). Stored
     *  alongside the accepter's pubkey in the local address
     *  book so future replies can be sealed without a fresh
     *  discovery round-trip. */
    val accepterX25519Pubkey: ByteArray,
    /** The original request's `from_pseudonym` field — the
     *  receiver matches this against their own identity
     *  pseudonym (and the pending-sent table) to confirm the
     *  ack is for them. */
    val ackToPseudonym: ByteArray,
    /** When the accepter pressed Accept (ms since epoch).
     *  Sender-attested; not authoritative — the DS [createdAt]
     *  is the trust anchor. */
    val acceptedAtMs: ULong,
    val createdAt: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is IncomingContactAck) return false
        return messageId == other.messageId &&
            accepterPubkey.contentEquals(other.accepterPubkey) &&
            accepterAccountId.contentEquals(other.accepterAccountId) &&
            accepterX25519Pubkey.contentEquals(other.accepterX25519Pubkey) &&
            ackToPseudonym.contentEquals(other.ackToPseudonym) &&
            acceptedAtMs == other.acceptedAtMs &&
            createdAt == other.createdAt
    }

    override fun hashCode(): Int {
        var result = messageId.hashCode()
        result = 31 * result + accepterPubkey.contentHashCode()
        result = 31 * result + accepterAccountId.contentHashCode()
        result = 31 * result + accepterX25519Pubkey.contentHashCode()
        result = 31 * result + ackToPseudonym.contentHashCode()
        result = 31 * result + acceptedAtMs.hashCode()
        result = 31 * result + createdAt.hashCode()
        return result
    }
}

/** Receipt returned by [ContactsClient.sendContactAck]. */
public data class PublishedAck(
    val messageId: Long,
    val createdAt: String,
)

/** Discriminated mailbox event surfaced by
 *  [ContactsClient.pollStrangerInbox]. */
public sealed interface IncomingMailboxEvent {
    public data class ContactRequest(val request: IncomingContactRequest) : IncomingMailboxEvent
    public data class Welcome(
        val messageId: Long,
        val welcomeBytes: ByteArray,
        val createdAt: String,
    ) : IncomingMailboxEvent {
        override fun equals(other: Any?): Boolean =
            other is Welcome && messageId == other.messageId &&
                welcomeBytes.contentEquals(other.welcomeBytes) &&
                createdAt == other.createdAt
        override fun hashCode(): Int =
            messageId.hashCode() * 31 + welcomeBytes.contentHashCode() * 31 + createdAt.hashCode()
    }
    /** 1.9b1 — original recipient's accept-reply. */
    public data class ContactAck(val ack: IncomingContactAck) : IncomingMailboxEvent
}

/** Result of [ContactsClient.pollStrangerInbox]. */
public data class PollResult(
    val events: List<IncomingMailboxEvent>,
    /** Fresh cursor for the next poll. Pass back as
     *  [ContactsClient.pollStrangerInbox]'s `after` argument. */
    val cursor: Long,
    /** `true` if the DS reported more rows past the last
     *  returned id — caller should poll again with the new
     *  cursor immediately. */
    val hasMore: Boolean,
)

// =============================================================================
// Errors
// =============================================================================

/** Single error type carrying a discriminated cause variant so
 *  the UI can branch on the kind without instanceof-walking the
 *  underlying client error trees. */
public class ContactsError(
    /** The cause shape. */
    public val variant: Variant,
    message: String,
) : RuntimeException(message) {

    /** Discriminated variant — match on this in the UI to choose
     *  the user-facing copy. */
    public sealed interface Variant {
        /** `@<normalisedUsername>` is not registered. */
        public data class UsernameUnknown(val normalisedUsername: String) : Variant

        /** The recipient's identity bundle is missing from the
         *  discovery service. They've claimed the username but
         *  haven't uploaded a bundle — typically a stale account
         *  or a client mid-rollout. */
        public data class NoIdentityBundle(val accountId: ByteArray) : Variant {
            override fun equals(other: Any?): Boolean =
                other is NoIdentityBundle && accountId.contentEquals(other.accountId)
            override fun hashCode(): Int = accountId.contentHashCode()
        }

        /** Any other failure — wraps a [DiscoveryHttpError] /
         *  [DiscoveryNetworkError] / [DsHttpError] /
         *  [DsNetworkError] / UniFFI [FfiException]. The UI
         *  surfaces the wrapped message verbatim. */
        public data class Wrapped(val cause: Throwable) : Variant

        /** ADR-0060 §2 — the recipient's mailbox refused the grant the
         *  request was signed with: they changed who can find them (or the
         *  link was revoked) since this device found them. */
        public data object ReachRefused : Variant
    }

    public companion object {
        public fun usernameUnknown(normalised: String): ContactsError =
            ContactsError(
                variant = Variant.UsernameUnknown(normalised),
                message = "username @$normalised is not registered",
            )

        public fun noIdentityBundle(accountId: ByteArray): ContactsError =
            ContactsError(
                variant = Variant.NoIdentityBundle(accountId),
                message = "no identity bundle for the given account id",
            )

        public fun reachRefused(cause: Throwable): ContactsError =
            ContactsError(
                variant = Variant.ReachRefused,
                // Diagnostic only; the screen renders `search.people.request_refused`
                // from the variant (`docs/ux/who-can-find-me.md` 6.1, "Request refused").
                message = "recipient mailbox refused the reach grant",
            ).also { it.initCause(cause) }

        public fun wrap(cause: Throwable): ContactsError =
            ContactsError(
                variant = Variant.Wrapped(cause),
                message = cause.message ?: cause::class.simpleName ?: "contacts error",
            )
    }
}

// ---------------------------------------------------------------------
// G-4 (ADR-0016): supersession fail-closed refusal
// ---------------------------------------------------------------------

/**
 * G-4 (ADR-0016) — discriminates the supersession fail-closed
 * failure mode for diagnostics. The UX is the same OOB-reverify
 * prompt regardless. Mirror of the web
 * `SupersessionRefusedReason`.
 */
public enum class SupersessionRefusedReason {
    /** The bundle carried no `supersession_proof` at all. */
    ProofAbsent,

    /** [uniffi.lakko_ffi_uniffi.verifySupersession] threw, or the
     *  proof / pinned key was malformed base64 (wrong-old-key /
     *  expired / out-of-range window / parse failure). */
    ProofInvalid,

    /** The proof verified but authorised a DIFFERENT key than the
     *  one the bundle actually served (defence in depth). */
    KeyMismatch,

    /** The marker would move backwards — a within-window rollback
     *  to an older successor (rule #4). */
    MarkerBackward,
}

/**
 * G-4 (ADR-0016) — thrown when a peer's fetched discovery identity
 * bundle carries an AccountIdentity `signature_pubkey` that DIFFERS
 * from the locally-pinned key AND the change cannot be authorised
 * by a valid supersession proof.
 *
 * This is the **fail-closed** path (ADR-0016 rule #2): we refuse to
 * send a [ContactRequest] to the unverified new key. The UI is
 * expected to catch this and prompt the user to re-verify the peer
 * **out of band** (compare safety numbers, re-scan a QR, etc.)
 * before proceeding. We never blind-TOFU a CHANGED key.
 *
 * Mirror of the web `SupersessionRefused`.
 */
public class SupersessionRefused(
    /** The cause shape — discriminates the failure mode. */
    public val reason: SupersessionRefusedReason,
    /** base64 of the account_id whose key change was refused. */
    public val accountIdB64: String,
    message: String,
) : RuntimeException(message) {
    /**
     * KT-log S4 — best-effort transparency-log context for the NEW
     * (refused) key, attached so the UI can distinguish an
     * owner-initiated identity RESET from an unexplained key change.
     *
     *   * `true`  — the log's latest entry commits to EXACTLY the served
     *     new key AND records it as an owner-initiated reset (ADR-0017
     *     lost-everything fresh-start).
     *   * `false` — the log's latest entry commits to the served new key
     *     but does NOT record a reset (an unexplained change → possible
     *     compromise).
     *   * `null`  — the log is off / unreachable / the account is absent /
     *     the log's latest pubkey does not match the served key (the marker
     *     is then about a different key, so it is unknown).
     *
     * This is **best-effort enrichment only**: a log failure NEVER changes
     * the refusal — gate 1 (the supersession/pin gate) throws regardless.
     * It only colours the OOB-reverify prompt. Mirror of the web
     * `SupersessionRefused.loggedReset`.
     */
    public var loggedReset: Boolean? = null

    /** ADR-0065 phase 2 — the key the directory served (standard base64), the
     *  `pendingKey` of UX 16.8 when [loggedReset] is `true`. `null` when the
     *  refusal was raised outside the fetch path. */
    public var servedPubkeyB64: String? = null

    /** ADR-0065 phase 2 — the directory's `key_generation` for the served
     *  key, when known. */
    public var servedKeyGeneration: Long? = null

    /** ADR-0065 phase 2 — the key pinned before the refusal (standard base64),
     *  the `previousKey` of UX 16.8. */
    public var pinnedPubkeyB64: String? = null
}

/**
 * KT-log S3e — thrown when the transparency log's signed root reports a
 * `log_size` SMALLER than the highest this client has ever seen. An
 * append-only log cannot shrink, so a rollback is an unambiguous attack (a
 * server trying to hide a leaf it later appended). This is the ONE
 * transparency-log verdict that hard-fails the identity-fetch path; all
 * other verdicts (`ABSENT` / `UNREACHABLE` / `OFF` / `MISMATCH`) are
 * recorded + surfaced but do NOT block, because the log is
 * eventually-consistent and the supersession/pin gate is the primary
 * trust (see [ContactsClient.checkTransparencyLog]).
 *
 * Mirror of the web `LogRollbackDetected`.
 */
public class LogRollbackDetected(
    /** base64 of the account_id whose fetch tripped the rollback. */
    public val accountIdB64: String,
) : RuntimeException(
    "the transparency log appears to have shrunk (a smaller log_size than " +
        "previously observed) — an append-only log cannot do this, so this " +
        "is an unambiguous rollback attack. Refusing to proceed.",
)

/** Fixed serialized length of a supersession proof — mirrors
 *  `lakko_transparency::supersession::SUPERSESSION_PROOF_LEN`
 *  (`version(1) ‖ old(32) ‖ new(32) ‖ issued(8) ‖ expires(8) ‖
 *  sig(64)` = 145). */
private const val SUPERSESSION_PROOF_LEN = 1 + 32 + 32 + 8 + 8 + 64

/** Length of an Ed25519 signature public key in an identity bundle. */
private const val SIGNATURE_KEY_LEN = 32

/** Byte offset of the big-endian i64 `expires_at` field within a
 *  supersession proof: after version(1) + old(32) + new(32) +
 *  issued(8) = 73. */
private const val SUPERSESSION_EXPIRES_AT_OFFSET = 1 + 32 + 32 + 8

/**
 * Read the big-endian i64 `expires_at_unix_secs` field out of a
 * supersession proof's wire bytes (G-4). Used ONLY after
 * [uniffi.lakko_ffi_uniffi.verifySupersession] has authenticated
 * the proof, so the parse runs on trusted, fixed-length data — we
 * still bound-check the length defensively. Mirror of the web
 * `readProofExpiresAtSecs`.
 */
internal fun readProofExpiresAtSecs(proofBytes: ByteArray): Long {
    require(proofBytes.size >= SUPERSESSION_PROOF_LEN) {
        // Should be unreachable — the verifier already accepted
        // these bytes — but never index past the buffer.
        "supersession proof too short (${proofBytes.size} bytes)"
    }
    var value = 0L
    for (i in 0 until 8) {
        value = (value shl 8) or
            (proofBytes[SUPERSESSION_EXPIRES_AT_OFFSET + i].toLong() and 0xFF)
    }
    return value
}
