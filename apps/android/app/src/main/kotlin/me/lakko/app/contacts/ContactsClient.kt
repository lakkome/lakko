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
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsClientArgError
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import uniffi.lakko_ffi_uniffi.ContactAck
import uniffi.lakko_ffi_uniffi.ContactRequest
import uniffi.lakko_ffi_uniffi.FfiException
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

        // G-4 (ADR-0016) rule #2 — verify the fetched bundle's
        // AccountIdentity key against the local pin BEFORE using its
        // keys to seal a ContactRequest. A changed key with no valid
        // supersession proof throws [SupersessionRefused] (fail-
        // closed) and we never seal to the unverified new key.
        verifyBundleAccountKey(recipientAccountId, bundle)

        val recipientSignaturePub = base64Decode(bundle.signature_pubkey)
        val recipientX25519Pub = base64Decode(bundle.x25519_pubkey)

        val publish = composeAndPublishContactRequest(
            senderSigningSeed = senderSigningSeed,
            senderAccountId = senderAccountId,
            senderSignaturePubkey = senderSignaturePubkey,
            recipientSignaturePubkey = recipientSignaturePub,
            recipientX25519Pubkey = recipientX25519Pub,
            introMessage = introMessage,
        )

        return SendResult(
            recipientUsername = lookup.normalisedUsername,
            recipientAccountId = recipientAccountId,
            messageId = publish.messageId,
            createdAt = publish.createdAt,
        )
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
    ): SendResult {
        val publish = composeAndPublishContactRequest(
            senderSigningSeed = senderSigningSeed,
            senderAccountId = senderAccountId,
            senderSignaturePubkey = senderSignaturePubkey,
            recipientSignaturePubkey = recipientSignaturePubkey,
            recipientX25519Pubkey = recipientX25519Pubkey,
            introMessage = introMessage,
        )
        return SendResult(
            recipientUsername = "",
            recipientAccountId = recipientAccountId,
            messageId = publish.messageId,
            createdAt = publish.createdAt,
        )
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
        Log.i(
            "lakko-contacts",
            "supersession: re-pinned account $accountIdB64 to a verified " +
                "rotated key (marker $proofExpiresAt)",
        )
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
                introMessage = introMessage,
            )
        } catch (e: FfiException) {
            throw ContactsError.wrap(e)
        }

        val envelope = wrapEnvelope(KIND_CONTACT_REQUEST, wireBytes)
        val publish = try {
            ds.publishMailbox(recipientPseudonym, envelope)
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
            ds.publishMailbox(senderMailbox, envelope)
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
    ): PollResult {
        val myPseudonym =
            IdentityPseudonym.fromIdentity(mySignaturePubkey).use { it.asBytes() }

        val raw = try {
            ds.fetchMailbox(myPseudonym, after = after, waitSecs = waitSecs)
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
                        val intro: String? = try {
                            val plaintext = req.openEnvelope(myX25519Secret)
                            // UTF-8 decode with replacement-on-
                            // error so malformed bytes still
                            // surface as the best-effort string
                            // we can show.
                            String(plaintext, Charsets.UTF_8)
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
            createdAt == other.createdAt
    }

    override fun hashCode(): Int {
        var result = messageId.hashCode()
        result = 31 * result + fromPubkey.contentHashCode()
        result = 31 * result + (fromAccountId?.contentHashCode() ?: 0)
        result = 31 * result + fromPseudonym.contentHashCode()
        result = 31 * result + validAfterMs.hashCode()
        result = 31 * result + (intro?.hashCode() ?: 0)
        result = 31 * result + createdAt.hashCode()
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
) : RuntimeException(message)

/** Fixed serialized length of a supersession proof — mirrors
 *  `lakko_transparency::supersession::SUPERSESSION_PROOF_LEN`
 *  (`version(1) ‖ old(32) ‖ new(32) ‖ issued(8) ‖ expires(8) ‖
 *  sig(64)` = 145). */
private const val SUPERSESSION_PROOF_LEN = 1 + 32 + 32 + 8 + 8 + 64

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
