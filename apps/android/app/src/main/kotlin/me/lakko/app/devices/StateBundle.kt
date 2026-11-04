// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import java.util.Base64
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.profile.toFfi

/**
 * ADR-0050 section 2.9 — the state bundle a pairing hands to the new device
 * beside the history bundle: accepted contacts (with their local renames), the
 * per-contact identity pins and rotation markers, and the ids of the groups the
 * old device knows.
 *
 * Version 3 (flag day, v2 refused) adds what a new device of an existing
 * member cannot learn from the groups themselves, since joining is not
 * membership growth and nobody re-sends: the shared group names (ADR-0051,
 * `groupNames`, each the name envelope with its ORIGINAL stamp), the profiles
 * this device holds for other people (ADR-0048, `profiles`, keyed by person
 * key), and the pending contacts added through an invite link (ADR-0079
 * section 5, `pendingContacts`) with what was written to them
 * (`heldMessages`, shown on the new device as display-only copies and never
 * sent from it).
 *
 * Mirror of `apps/web/src/devices/state-bundle.ts`. The JSON shape, the row
 * magic and the exporter label are byte-identical so a web scanner and an
 * Android joiner (and the reverse) interoperate: field names are the web
 * `AcceptedContact` names, group ids are standard base64.
 *
 * Carried as ONE more row on the pairing rendezvous mailbox, tagged by a
 * plaintext magic so the history, recovery and state polls each skip the rows
 * that are not theirs. Sealed under the DeviceGroup exporter at the post-add
 * epoch; the DS stores ciphertext.
 */
public object StateBundle {

    /** Exporter label; the context is the 32-byte rendezvous pseudonym. */
    public const val EXPORTER_LABEL: String = "lakko/state-bundle/v1"

    /** Exporter output length — the ChaCha20-Poly1305 key size. */
    public const val EXPORTER_KEY_LENGTH: Int = 32

    private val ROW_MAGIC: ByteArray = "lakko/state-bundle-row/v1\u0000".toByteArray(Charsets.US_ASCII)
    private const val POLL_TIMEOUT_MS: Long = 20_000L
    private const val PER_REQUEST_LONG_POLL_SECS: Int = 5
    private const val MAX_BUNDLE_BYTES: Int = 1 shl 20

    /** One accepted contact in the web field vocabulary. */
    public data class CarriedContact(
        val signaturePubkeyB64: String,
        val displayName: String,
        val acceptedAtMs: Long,
        val accountIdHex: String? = null,
        val x25519PubkeyB64: String? = null,
        val groupIdB64: String? = null,
    )

    /** A pinned account key as carried in the bundle. */
    public data class CarriedPin(
        val accountIdB64: String,
        val pinnedSignaturePubkeyB64: String,
        val rotationMarker: Long,
        /** ADR-0074: the contact's proven predecessors as the sending device
         *  holds them, oldest first (standard base64 of 32-byte signature
         *  keys); empty when it knows of none. At most [MAX_PREDECESSORS]. */
        val provenPredecessorsB64: List<String> = emptyList(),
    )

    /**
     * ADR-0074: one earlier account key of THIS account. [seedB64] is the
     * 64-byte account phrase seed of that key (key material: it travels only
     * inside the sealed bundle and is never logged); [adoptedAtMs] is when the
     * sending device adopted the key that replaced it.
     */
    public data class CarriedOwnPredecessor(
        val seedB64: String,
        val adoptedAtMs: Long,
    ) {
        override fun toString(): String = "CarriedOwnPredecessor(<redacted>)"
    }

    /** v3 — a group's shared name: the ADR-0051 envelope composed from the
     *  stored name and its ORIGINAL stamp (standard base64), so a later rename
     *  still wins on the new device. */
    public data class CarriedGroupName(
        val groupIdB64: String,
        val envelopeB64: String,
    )

    /** v3 — a person's profile: the ADR-0048 envelope composed from the
     *  stored profile with its ORIGINAL stamp, under the 32-byte person key. */
    public data class CarriedProfile(
        val personKeyB64: String,
        val envelopeB64: String,
    )

    /** v3 — one message written to a pending contact, in queue order.
     *  [attachments] is the writing client's own form of the held refs (this
     *  client's pending queue holds text only and writes `[]`); a receiving
     *  Android device keeps only their count. */
    public data class CarriedHeldMessage(
        val text: String,
        val attachments: JsonArray = JsonArray(emptyList()),
        val replyTo: Long? = null,
        val heldAtMs: Long,
    )

    /** v3 — a person added through their invite link whose app has not
     *  answered yet (field names of the iOS `PendingContact`). */
    public data class CarriedPendingContact(
        /** 16-byte account id, 32 lower-case hex characters. */
        val accountIdHex: String,
        val signaturePubkeyB64: String,
        val x25519PubkeyB64: String,
        val displayName: String,
        val username: String,
        val addedAtMs: Long,
        /** The 16-byte invite link id, standard base64. */
        val linkIdB64: String,
        val heldMessages: List<CarriedHeldMessage> = emptyList(),
    )

    /** The bundle; `v` is always [VERSION]. */
    public data class Bundle(
        val contacts: List<CarriedContact>,
        val pins: List<CarriedPin>,
        val knownGroupIdsB64: List<String>,
        /** This account's earlier keys, OLDEST FIRST; empty when the account
         *  never rotated. At most [MAX_PREDECESSORS]. */
        val ownPredecessors: List<CarriedOwnPredecessor> = emptyList(),
        /** v3: at most [MAX_GROUP_NAMES], one per group id. */
        val groupNames: List<CarriedGroupName> = emptyList(),
        /** v3: at most [MAX_PROFILES], one per person key, never this
         *  account's own. */
        val profiles: List<CarriedProfile> = emptyList(),
        /** v3: at most [MAX_PENDING_CONTACTS]. */
        val pendingContacts: List<CarriedPendingContact> = emptyList(),
    )

    /** The one bundle version this build writes and reads (flag day: a
     *  bundle of any other version is refused). */
    public const val VERSION: Long = 3L

    /** Decode bounds of the v3 arrays (chosen, not measured). Over a bound
     *  the whole bundle is refused; the builder keeps the first N. */
    public const val MAX_GROUP_NAMES: Int = 1000
    public const val MAX_PROFILES: Int = 2000
    public const val MAX_PENDING_CONTACTS: Int = 500
    public const val MAX_HELD_MESSAGES: Int = 200

    private const val ACCOUNT_ID_LEN: Int = 16
    private const val LINK_ID_LEN: Int = 16
    private val ACCOUNT_ID_HEX = Regex("^[0-9a-f]{32}$")

    /** The bound on `ownPredecessors` and on each pin's
     *  `provenPredecessorsB64`. */
    public const val MAX_PREDECESSORS: Int = 8

    private const val SEED_LEN: Int = 64
    private const val KEY_LEN: Int = 32

    // ---- codec -------------------------------------------------------------

    public fun encode(bundle: Bundle): ByteArray {
        val obj = buildJsonObject {
            put("v", VERSION)
            put(
                "contacts",
                buildJsonArray {
                    for (c in bundle.contacts) {
                        add(
                            buildJsonObject {
                                put("signaturePubkeyB64", c.signaturePubkeyB64)
                                put("displayName", c.displayName)
                                put("acceptedAtMs", c.acceptedAtMs)
                                c.accountIdHex?.let { put("accountIdHex", it) }
                                c.x25519PubkeyB64?.let { put("x25519PubkeyB64", it) }
                                c.groupIdB64?.let { put("groupIdB64", it) }
                            },
                        )
                    }
                },
            )
            put(
                "pins",
                buildJsonArray {
                    for (p in bundle.pins) {
                        add(
                            buildJsonObject {
                                put("accountIdB64", p.accountIdB64)
                                put("pinnedSignaturePubkeyB64", p.pinnedSignaturePubkeyB64)
                                put("rotationMarker", p.rotationMarker)
                                put(
                                    "provenPredecessorsB64",
                                    buildJsonArray { p.provenPredecessorsB64.forEach { add(JsonPrimitive(it)) } },
                                )
                            },
                        )
                    }
                },
            )
            put("knownGroupIdsB64", buildJsonArray { bundle.knownGroupIdsB64.forEach { add(JsonPrimitive(it)) } })
            put(
                "ownPredecessors",
                buildJsonArray {
                    for (o in bundle.ownPredecessors) {
                        add(
                            buildJsonObject {
                                put("seedB64", o.seedB64)
                                put("adoptedAtMs", o.adoptedAtMs)
                            },
                        )
                    }
                },
            )
            put(
                "groupNames",
                buildJsonArray {
                    for (g in bundle.groupNames) {
                        add(
                            buildJsonObject {
                                put("groupIdB64", g.groupIdB64)
                                put("envelopeB64", g.envelopeB64)
                            },
                        )
                    }
                },
            )
            put(
                "profiles",
                buildJsonArray {
                    for (p in bundle.profiles) {
                        add(
                            buildJsonObject {
                                put("personKeyB64", p.personKeyB64)
                                put("envelopeB64", p.envelopeB64)
                            },
                        )
                    }
                },
            )
            put(
                "pendingContacts",
                buildJsonArray {
                    for (c in bundle.pendingContacts) {
                        add(
                            buildJsonObject {
                                put("accountIdHex", c.accountIdHex)
                                put("signaturePubkeyB64", c.signaturePubkeyB64)
                                put("x25519PubkeyB64", c.x25519PubkeyB64)
                                put("displayName", c.displayName)
                                put("username", c.username)
                                put("addedAtMs", c.addedAtMs)
                                put("linkIdB64", c.linkIdB64)
                                put(
                                    "heldMessages",
                                    buildJsonArray {
                                        for (m in c.heldMessages) {
                                            add(
                                                buildJsonObject {
                                                    put("text", m.text)
                                                    put("attachments", m.attachments)
                                                    put("replyTo", m.replyTo?.let { JsonPrimitive(it) } ?: JsonNull)
                                                    put("heldAtMs", m.heldAtMs)
                                                },
                                            )
                                        }
                                    },
                                )
                            },
                        )
                    }
                },
            )
        }
        return obj.toString().toByteArray(Charsets.UTF_8)
    }

    /** Decode a bundle, refusing anything that is not the shape above. A
     *  refused bundle is `null` — the joiner then lands without contacts. */
    public fun decode(bytes: ByteArray): Bundle? {
        if (bytes.isEmpty() || bytes.size > MAX_BUNDLE_BYTES) return null
        val root = try {
            Json.parseToJsonElement(bytes.toString(Charsets.UTF_8)) as? JsonObject
        } catch (_: Throwable) {
            null
        } ?: return null
        if (root.integer("v") != VERSION) return null
        val contactsArr = root["contacts"] as? JsonArray ?: return null
        val pinsArr = root["pins"] as? JsonArray ?: return null
        val groupsArr = root["knownGroupIdsB64"] as? JsonArray ?: return null
        val contacts = mutableListOf<CarriedContact>()
        for (el in contactsArr) {
            val r = el as? JsonObject ?: return null
            val sig = r.string("signaturePubkeyB64") ?: return null
            val name = r.string("displayName") ?: return null
            val at = r.number("acceptedAtMs") ?: return null
            contacts += CarriedContact(
                signaturePubkeyB64 = sig,
                displayName = name,
                acceptedAtMs = at,
                accountIdHex = r.string("accountIdHex"),
                x25519PubkeyB64 = r.string("x25519PubkeyB64"),
                groupIdB64 = r.string("groupIdB64"),
            )
        }
        val pins = mutableListOf<CarriedPin>()
        for (el in pinsArr) {
            val r = el as? JsonObject ?: return null
            val provenArr = r["provenPredecessorsB64"] as? JsonArray ?: return null
            if (provenArr.size > MAX_PREDECESSORS) return null
            val proven = mutableListOf<String>()
            for (k in provenArr) {
                val p = k as? JsonPrimitive ?: return null
                if (!p.isString || standardBase64OfLength(p.content, KEY_LEN) == null) return null
                proven += p.content
            }
            pins += CarriedPin(
                accountIdB64 = r.string("accountIdB64") ?: return null,
                pinnedSignaturePubkeyB64 = r.string("pinnedSignaturePubkeyB64") ?: return null,
                rotationMarker = r.number("rotationMarker") ?: return null,
                provenPredecessorsB64 = proven,
            )
        }
        val ownArr = root["ownPredecessors"] as? JsonArray ?: return null
        if (ownArr.size > MAX_PREDECESSORS) return null
        val own = mutableListOf<CarriedOwnPredecessor>()
        for (el in ownArr) {
            val r = el as? JsonObject ?: return null
            val seedB64 = r.string("seedB64") ?: return null
            standardBase64OfLength(seedB64, SEED_LEN)?.fill(0) ?: return null
            own += CarriedOwnPredecessor(seedB64 = seedB64, adoptedAtMs = r.integer("adoptedAtMs") ?: return null)
        }
        val groups = mutableListOf<String>()
        for (el in groupsArr) {
            val p = el as? JsonPrimitive ?: return null
            if (!p.isString) return null
            groups += p.content
        }
        val groupNames = decodeGroupNames(root["groupNames"] as? JsonArray ?: return null) ?: return null
        val profiles = decodeProfiles(root["profiles"] as? JsonArray ?: return null) ?: return null
        val pending = decodePendingContacts(root["pendingContacts"] as? JsonArray ?: return null) ?: return null
        return Bundle(contacts, pins, groups, own, groupNames, profiles, pending)
    }

    private fun decodeGroupNames(arr: JsonArray): List<CarriedGroupName>? {
        if (arr.size > MAX_GROUP_NAMES) return null
        val out = ArrayList<CarriedGroupName>(arr.size)
        val seen = HashSet<String>()
        for (el in arr) {
            val r = el as? JsonObject ?: return null
            val id = r.string("groupIdB64") ?: return null
            val env = r.string("envelopeB64") ?: return null
            val idBytes = nonEmptyStandardBase64(id) ?: return null
            if (nonEmptyStandardBase64(env) == null) return null
            // One entry per group id: a repeat refuses the bundle.
            if (!seen.add(hexOf(idBytes))) return null
            out += CarriedGroupName(id, env)
        }
        return out
    }

    private fun decodeProfiles(arr: JsonArray): List<CarriedProfile>? {
        if (arr.size > MAX_PROFILES) return null
        val out = ArrayList<CarriedProfile>(arr.size)
        val seen = HashSet<String>()
        for (el in arr) {
            val r = el as? JsonObject ?: return null
            val key = r.string("personKeyB64") ?: return null
            val env = r.string("envelopeB64") ?: return null
            val keyBytes = standardBase64OfLength(key, KEY_LEN) ?: return null
            if (nonEmptyStandardBase64(env) == null) return null
            // One entry per person key: a repeat refuses the bundle.
            if (!seen.add(hexOf(keyBytes))) return null
            out += CarriedProfile(key, env)
        }
        return out
    }

    private fun decodePendingContacts(arr: JsonArray): List<CarriedPendingContact>? {
        if (arr.size > MAX_PENDING_CONTACTS) return null
        val out = ArrayList<CarriedPendingContact>(arr.size)
        val seen = HashSet<String>()
        for (el in arr) {
            val r = el as? JsonObject ?: return null
            val accountIdHex = r.string("accountIdHex")?.takeIf { ACCOUNT_ID_HEX.matches(it) } ?: return null
            // One entry per pending account: a repeat refuses the bundle.
            if (!seen.add(accountIdHex)) return null
            val sig = r.string("signaturePubkeyB64")?.takeIf { standardBase64OfLength(it, KEY_LEN) != null } ?: return null
            val x = r.string("x25519PubkeyB64")?.takeIf { standardBase64OfLength(it, KEY_LEN) != null } ?: return null
            val name = r.string("displayName") ?: return null
            val username = r.string("username") ?: return null
            val addedAt = r.integer("addedAtMs") ?: return null
            val link = r.string("linkIdB64")?.takeIf { standardBase64OfLength(it, LINK_ID_LEN) != null } ?: return null
            val heldArr = r["heldMessages"] as? JsonArray ?: return null
            if (heldArr.size > MAX_HELD_MESSAGES) return null
            val held = ArrayList<CarriedHeldMessage>(heldArr.size)
            for (m in heldArr) {
                val o = m as? JsonObject ?: return null
                val text = o.string("text") ?: return null
                val attachments = o["attachments"] as? JsonArray ?: return null
                val replyEl = o["replyTo"] ?: return null
                val replyTo = if (replyEl is JsonNull) null else (o.integer("replyTo") ?: return null)
                val heldAt = o.integer("heldAtMs") ?: return null
                held += CarriedHeldMessage(text, attachments, replyTo, heldAt)
            }
            out += CarriedPendingContact(accountIdHex, sig, x, name, username, addedAt, link, held)
        }
        return out
    }

    /** [b64] decoded as non-empty STANDARD base64, else `null`. */
    private fun nonEmptyStandardBase64(b64: String): ByteArray? =
        try {
            Base64.getDecoder().decode(b64).takeIf { it.isNotEmpty() }
        } catch (_: IllegalArgumentException) {
            null
        }

    /** [b64] decoded as STANDARD base64 when it is exactly [length] bytes,
     *  else `null`. */
    private fun standardBase64OfLength(b64: String, length: Int): ByteArray? {
        val bytes = try {
            Base64.getDecoder().decode(b64)
        } catch (_: IllegalArgumentException) {
            return null
        }
        if (bytes.size != length) {
            bytes.fill(0)
            return null
        }
        return bytes
    }

    /** A JSON number that is an integer; a fraction, an exponent form, a
     *  string or a boolean is `null`. */
    private fun JsonObject.integer(key: String): Long? {
        val p = this[key] as? JsonPrimitive ?: return null
        if (p.isString || p.booleanOrNull != null) return null
        return p.longOrNull
    }

    private fun JsonObject.string(key: String): String? {
        val p = this[key] as? JsonPrimitive ?: return null
        return if (p.isString) p.content else null
    }

    private fun JsonObject.number(key: String): Long? {
        val p = this[key] as? JsonPrimitive ?: return null
        if (p.isString || p.booleanOrNull != null) return null
        return p.longOrNull ?: p.doubleOrNull?.toLong()
    }

    // ---- build / apply ------------------------------------------------------

    /** Read [accountIdB64]'s contacts and pins and this device's group ids
     *  into a bundle. Only that account's list is read: a bundle never
     *  carries another account's contacts to a new device. */
    public fun build(context: Context, accountIdB64: String, knownGroupIdsHex: Collection<String>): Bundle {
        val owner = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        val links = me.lakko.app.contacts.SharedPrefsProvenLinkStore(context).load(owner)
        val ownRows = try {
            me.lakko.app.auth.OwnPredecessorStore.onDevice(context).load(owner)
        } catch (_: Throwable) {
            emptyList()
        }
        val ownKeys = HashSet<String>()
        val own = try {
            ownRows.forEach { ownKeys += Base64.getEncoder().encodeToString(it.signaturePubkey) }
            carriedOwnPredecessors(ownRows)
        } finally {
            ownRows.forEach { it.zero() }
        }
        currentAccountKeyB64(context, owner)?.let { ownKeys += it }
        val pending = try {
            me.lakko.app.contacts.PendingContactsFiles.onDevice(context).load(owner)
        } catch (_: Throwable) {
            emptyList()
        }
        val held = try {
            me.lakko.app.chat.SealedHeldSendBackend(context).load(owner)
        } catch (_: Throwable) {
            emptyList()
        }
        val copies = try {
            me.lakko.app.chat.HeldCopyStore.onDevice(context).load(owner)
        } catch (_: Throwable) {
            emptyMap()
        }
        return build(
            context = context,
            accountIdB64 = accountIdB64,
            knownGroupIdsHex = knownGroupIdsHex,
            ownPredecessors = own,
            links = links,
            ownKeysB64 = ownKeys,
            pendingContacts = pending,
            heldSends = held,
            heldCopies = copies,
        )
    }

    /** The account key of [accountIdB64] on this device, standard base64, or
     *  `null` when its seed is not held (or cannot be read). */
    private fun currentAccountKeyB64(context: Context, accountIdB64: String): String? {
        val seed = try {
            me.lakko.app.auth.AccountIdentitySeedStore.getAccountIdentitySeed(context, accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        return try {
            uniffi.lakko_ffi_uniffi.Identity.fromSeed(seed).use { Base64.getEncoder().encodeToString(it.signaturePublicKey()) }
        } catch (_: Throwable) {
            null
        } finally {
            seed.fill(0)
        }
    }

    /** [build] over the own predecessors and proven steps already read (the
     *  seam a host test drives: the predecessor store needs the Keystore). */
    internal fun build(
        context: Context,
        accountIdB64: String,
        knownGroupIdsHex: Collection<String>,
        ownPredecessors: List<CarriedOwnPredecessor>,
        links: List<me.lakko.app.contacts.ProvenLink>,
        ownKeysB64: Set<String> = emptySet(),
        pendingContacts: List<me.lakko.app.contacts.PendingContact> = emptyList(),
        heldSends: List<me.lakko.app.chat.PersistedHeldSend> = emptyList(),
        heldCopies: Map<String, List<me.lakko.app.chat.HeldCopy>> = emptyMap(),
        codecs: EnvelopeCodecs = FfiEnvelopeCodecs,
    ): Bundle {
        val accepted = me.lakko.app.contacts.AcceptedContactsStore.load(context, accountIdB64)
        val pinStore = me.lakko.app.contacts.PeerPinStore(
            context,
            me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64),
        )
        val enc = Base64.getEncoder()
        val contacts = accepted.map { c ->
            CarriedContact(
                signaturePubkeyB64 = enc.encodeToString(c.request.fromPubkey),
                displayName = c.displayName.orEmpty(),
                acceptedAtMs = c.request.validAfterMs.toLong(),
                accountIdHex = c.request.fromAccountId?.let { hexOf(it) },
                groupIdB64 = c.groupIdHex?.let { bytesOfHex(it) }?.let { enc.encodeToString(it) },
            )
        }
        val pins = accepted.mapNotNull { c ->
            val accountIdB64 = c.request.fromAccountId?.let { enc.encodeToString(it) } ?: return@mapNotNull null
            val pin = try {
                pinStore.readPin(accountIdB64)
            } catch (_: Throwable) {
                null
            } ?: return@mapNotNull null
            CarriedPin(
                accountIdB64,
                pin.pinnedSignaturePubkeyB64,
                pin.rotationMarker,
                provenPredecessorsOf(pin.pinnedSignaturePubkeyB64, links),
            )
        }
        val groups = knownGroupIdsHex.mapNotNull { bytesOfHex(it)?.let { b -> enc.encodeToString(b) } }
        val owner = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        val groupNames = carriedGroupNames(
            knownGroupIdsHex,
            load = { hex -> me.lakko.app.chat.GroupNameStore.load(context, accountIdB64, hex) },
            compose = codecs.composeGroupName,
        )
        val storedProfiles = try {
            me.lakko.app.profile.SharedPrefsProfilePersistence(context).loadProfiles(owner)
        } catch (_: Throwable) {
            emptyMap()
        }
        val profiles = carriedProfiles(
            storedProfiles,
            ownKeysB64 = ownKeysB64,
            ownAccountIdHex = accountIdHexOf(owner),
            compose = codecs.composeProfile,
        )
        val pending = carriedPendingContacts(pendingContacts, heldSends, heldCopies)
        return fitToSizeCap(Bundle(contacts, pins, groups, ownPredecessors, groupNames, profiles, pending))
    }

    /** The encoded bundle size the decoder accepts. Visible for tests. */
    internal const val SIZE_CAP_BYTES: Int = MAX_BUNDLE_BYTES

    /**
     * Keep the encoded [bundle] within [cap] bytes (the decoder's own cap),
     * so a big account is not refused whole. Trimmed in this order until it
     * fits: profiles from the end, then group names from the end, then the
     * held messages of the pending contacts (the last contact's last message
     * first). Contacts, pins, group ids and keys are never trimmed. Mirror of
     * the web `fitToSizeCap`.
     */
    internal fun fitToSizeCap(bundle: Bundle, cap: Int = MAX_BUNDLE_BYTES): Bundle {
        var size = encode(bundle).size
        if (size <= cap) return bundle
        // A comma and the entry's JSON text: what dropping it saves.
        fun nameBytes(x: CarriedGroupName) = 32 + x.groupIdB64.length + x.envelopeB64.length
        fun profileBytes(x: CarriedProfile) = 34 + x.personKeyB64.length + x.envelopeB64.length
        fun heldBytes(x: CarriedHeldMessage) =
            60 + x.text.toByteArray(Charsets.UTF_8).size * 2 + x.attachments.toString().length
        val profiles = bundle.profiles.toMutableList()
        val names = bundle.groupNames.toMutableList()
        val pending = bundle.pendingContacts.map { it to it.heldMessages.toMutableList() }
        fun current() = bundle.copy(
            profiles = profiles.toList(),
            groupNames = names.toList(),
            pendingContacts = pending.map { (c, held) -> c.copy(heldMessages = held.toList()) },
        )
        while (true) {
            var over = size - cap
            if (over <= 0) break
            var trimmed = false
            while (over > 0 && profiles.isNotEmpty()) {
                over -= profileBytes(profiles.removeAt(profiles.lastIndex)); trimmed = true
            }
            while (over > 0 && names.isNotEmpty()) {
                over -= nameBytes(names.removeAt(names.lastIndex)); trimmed = true
            }
            for ((_, held) in pending.asReversed()) {
                while (over > 0 && held.isNotEmpty()) {
                    over -= heldBytes(held.removeAt(held.lastIndex)); trimmed = true
                }
            }
            size = encode(current()).size
            if (!trimmed) break
        }
        return current()
    }

    // ---- v3: group names, profiles, pending contacts (pure) ---------------

    /** The envelope codecs the v3 arrays need. The FFI ones in production;
     *  a host test injects its own (the JVM suite has no native library). */
    internal class EnvelopeCodecs(
        val composeGroupName: (me.lakko.app.chat.StoredGroupName) -> ByteArray?,
        val decodeGroupName: (ByteArray) -> me.lakko.app.chat.StoredGroupName?,
        val groupNameIsNewer: (ULong, ULong) -> Boolean,
        val composeProfile: (me.lakko.app.profile.PeerProfile) -> ByteArray?,
        val decodeProfile: (ByteArray) -> me.lakko.app.profile.PeerProfile?,
    )

    internal val FfiEnvelopeCodecs: EnvelopeCodecs = EnvelopeCodecs(
        composeGroupName = { stored ->
            try {
                uniffi.lakko_ffi_uniffi.composeGroupNameEnvelope(
                    uniffi.lakko_ffi_uniffi.FfiGroupName(name = stored.name, updatedAtMs = stored.updatedAtMs),
                )
            } catch (_: Throwable) {
                null
            }
        },
        decodeGroupName = { bytes ->
            try {
                uniffi.lakko_ffi_uniffi.decodeGroupNameEnvelope(bytes)
                    .let { me.lakko.app.chat.StoredGroupName(name = it.name, updatedAtMs = it.updatedAtMs) }
            } catch (_: Throwable) {
                null
            }
        },
        groupNameIsNewer = { a, b -> uniffi.lakko_ffi_uniffi.groupNameIsNewer(a, b) },
        composeProfile = ::composeProfileOrNull,
        decodeProfile = { bytes ->
            try {
                me.lakko.app.profile.peerProfileOf(uniffi.lakko_ffi_uniffi.decodeProfileEnvelope(bytes))
            } catch (_: Throwable) {
                null
            }
        },
    )

    /** A stored profile as the ADR-0048 envelope, with its ORIGINAL stamp;
     *  without the certificate when the composer refuses it with one, and
     *  `null` when it refuses the profile. No reach grants are carried. */
    private fun composeProfileOrNull(p: me.lakko.app.profile.PeerProfile): ByteArray? {
        fun ffi(withCertificate: Boolean) = uniffi.lakko_ffi_uniffi.FfiProfile(
            displayName = p.displayName,
            username = p.username,
            updatedAtMs = p.updatedAtMs,
            accountId = p.accountIdHex?.let { bytesOfHex(it) }?.takeIf { it.size == ACCOUNT_ID_LEN },
            usernameCertificate = if (withCertificate && p.username != null) {
                p.usernameCertificateHex?.let { bytesOfHex(it) }
            } else {
                null
            },
            grants = emptyList(),
            avatar = p.avatar?.toFfi(),
        )
        return try {
            uniffi.lakko_ffi_uniffi.composeProfileEnvelope(ffi(withCertificate = true))
        } catch (_: Throwable) {
            try {
                uniffi.lakko_ffi_uniffi.composeProfileEnvelope(ffi(withCertificate = false))
            } catch (_: Throwable) {
                null
            }
        }
    }

    /** The group names to carry: one per known group this device holds a
     *  stored name row for (a cleared name too, so its stamp orders), in a
     *  stable order (by group id), at most [MAX_GROUP_NAMES]. */
    internal fun carriedGroupNames(
        knownGroupIdsHex: Collection<String>,
        load: (String) -> me.lakko.app.chat.StoredGroupName?,
        compose: (me.lakko.app.chat.StoredGroupName) -> ByteArray?,
    ): List<CarriedGroupName> {
        val enc = Base64.getEncoder()
        val out = ArrayList<CarriedGroupName>()
        for (hex in knownGroupIdsHex.map { it.lowercase() }.distinct().sorted()) {
            if (out.size >= MAX_GROUP_NAMES) break
            val id = bytesOfHex(hex)?.takeIf { it.isNotEmpty() } ?: continue
            val stored = try {
                load(hex)
            } catch (_: Throwable) {
                null
            } ?: continue
            val envelope = compose(stored) ?: continue
            out += CarriedGroupName(enc.encodeToString(id), enc.encodeToString(envelope))
        }
        return out
    }

    /** The profiles to carry: every stored profile under a 32-byte person
     *  key that is not this account's own (by key, or by the account id the
     *  profile names), in a stable order (by key), at most [MAX_PROFILES].
     *  A name a search found goes as the name alone
     *  ([me.lakko.app.profile.PeerProfile.forStateBundle]). */
    internal fun carriedProfiles(
        stored: Map<String, me.lakko.app.profile.PeerProfile>,
        ownKeysB64: Set<String>,
        ownAccountIdHex: String?,
        compose: (me.lakko.app.profile.PeerProfile) -> ByteArray?,
    ): List<CarriedProfile> {
        val enc = Base64.getEncoder()
        val out = ArrayList<CarriedProfile>()
        val seen = HashSet<String>()
        for ((rawKey, profile) in stored.entries.sortedBy { it.key }) {
            if (out.size >= MAX_PROFILES) break
            val keyBytes = standardBase64OfLength(rawKey, KEY_LEN) ?: continue
            val key = enc.encodeToString(keyBytes)
            if (key in ownKeysB64 || !seen.add(key)) continue
            if (ownAccountIdHex != null && profile.accountIdHex == ownAccountIdHex) continue
            val carried = profile.forStateBundle ?: continue
            val envelope = compose(carried) ?: continue
            out += CarriedProfile(key, enc.encodeToString(envelope))
        }
        return out
    }

    /** The pending contacts to carry, each with what was written to it: this
     *  device's own held messages for its queue key and the display copies it
     *  holds there itself, in held order, at most [MAX_HELD_MESSAGES] each. A
     *  record whose link id is not 16 bytes is left out (nothing is
     *  invented). This client's pending queue holds text only, so the
     *  attachments are always `[]` and `replyTo` is `null`. */
    internal fun carriedPendingContacts(
        pending: List<me.lakko.app.contacts.PendingContact>,
        heldSends: List<me.lakko.app.chat.PersistedHeldSend>,
        heldCopies: Map<String, List<me.lakko.app.chat.HeldCopy>>,
    ): List<CarriedPendingContact> {
        val enc = Base64.getEncoder()
        val out = ArrayList<CarriedPendingContact>()
        for (c in pending) {
            if (out.size >= MAX_PENDING_CONTACTS) break
            if (!ACCOUNT_ID_HEX.matches(c.accountIdHex)) continue
            if (standardBase64OfLength(c.signaturePubkeyB64, KEY_LEN) == null) continue
            if (standardBase64OfLength(c.x25519PubkeyB64, KEY_LEN) == null) continue
            val link = bytesOfHex(c.linkIdHex)?.takeIf { it.size == LINK_ID_LEN } ?: continue
            val own = heldSends
                .filter { it.conversationHex == c.queueKey && it.text.isNotEmpty() }
                .map { CarriedHeldMessage(text = it.text, heldAtMs = it.heldAtMs) }
            val copies = heldCopies[c.queueKey].orEmpty()
                .filter { it.attachmentCount == 0 }
                .map { CarriedHeldMessage(text = it.text, replyTo = it.replyTo, heldAtMs = it.heldAtMs) }
            out += CarriedPendingContact(
                accountIdHex = c.accountIdHex,
                signaturePubkeyB64 = c.signaturePubkeyB64,
                x25519PubkeyB64 = c.x25519PubkeyB64,
                displayName = c.displayName,
                username = c.username,
                addedAtMs = c.addedAtMs,
                linkIdB64 = enc.encodeToString(link),
                heldMessages = (own + copies).sortedBy { it.heldAtMs }.take(MAX_HELD_MESSAGES),
            )
        }
        return out
    }

    /** The receiving half of `groupNames`: each name through the group-name
     *  store's own entry, strictly newer only; a name that does not decode is
     *  skipped. No authority check (the bundle is this account's own device's,
     *  sealed under the DeviceGroup exporter) and nothing is posted. Returns
     *  how many were stored. */
    internal fun applyGroupNames(
        entries: List<CarriedGroupName>,
        decode: (ByteArray) -> me.lakko.app.chat.StoredGroupName?,
        isNewer: (ULong, ULong) -> Boolean,
        load: (String) -> me.lakko.app.chat.StoredGroupName?,
        save: (String, me.lakko.app.chat.StoredGroupName) -> Unit,
    ): Int {
        var stored = 0
        for (e in entries) {
            val id = nonEmptyStandardBase64(e.groupIdB64) ?: continue
            val bytes = nonEmptyStandardBase64(e.envelopeB64) ?: continue
            val name = decode(bytes) ?: continue
            val hex = hexOf(id)
            val held = load(hex)
            if (held != null && !isNewer(name.updatedAtMs, held.updatedAtMs)) continue
            save(hex, name)
            stored += 1
        }
        return stored
    }

    /** The receiving half of `profiles`: each through the profile store's
     *  strictly-newer offer under its person key, never under one of
     *  [ownKeysB64] or naming [ownAccountIdHex]; a profile that does not
     *  decode is skipped. Returns how many were stored. */
    internal fun applyProfiles(
        entries: List<CarriedProfile>,
        ownKeysB64: Set<String>,
        ownAccountIdHex: String?,
        decode: (ByteArray) -> me.lakko.app.profile.PeerProfile?,
        offer: (String, me.lakko.app.profile.PeerProfile) -> me.lakko.app.profile.ProfileOffer,
    ): Int {
        val enc = Base64.getEncoder()
        var stored = 0
        for (e in entries) {
            val key = standardBase64OfLength(e.personKeyB64, KEY_LEN)?.let { enc.encodeToString(it) } ?: continue
            if (key in ownKeysB64) continue
            val bytes = nonEmptyStandardBase64(e.envelopeB64) ?: continue
            val profile = decode(bytes) ?: continue
            if (ownAccountIdHex != null && profile.accountIdHex == ownAccountIdHex) continue
            if (offer(key, profile) == me.lakko.app.profile.ProfileOffer.STORED) stored += 1
        }
        return stored
    }

    /** The carried pending contacts this device holds neither as an accepted
     *  contact nor as a pending one (by account key or account id), as
     *  records, in order. */
    public fun freshPendingContacts(
        carried: List<me.lakko.app.contacts.PendingContact>,
        accepted: List<me.lakko.app.contacts.AcceptedContact>,
        pending: List<me.lakko.app.contacts.PendingContact>,
    ): List<me.lakko.app.contacts.PendingContact> {
        val enc = Base64.getEncoder()
        val keys = HashSet<String>()
        val ids = HashSet<String>()
        for (a in accepted) {
            keys += enc.encodeToString(a.request.fromPubkey)
            a.request.fromAccountId?.let { ids += hexOf(it) }
        }
        for (p in pending) {
            keys += p.signaturePubkeyB64
            ids += p.accountIdHex
        }
        val out = ArrayList<me.lakko.app.contacts.PendingContact>()
        for (c in carried) {
            if (c.signaturePubkeyB64 in keys || c.accountIdHex in ids) continue
            keys += c.signaturePubkeyB64
            ids += c.accountIdHex
            out += c
        }
        return out
    }

    /** A carried pending contact as this client's record. */
    internal fun pendingRecordOf(c: CarriedPendingContact): me.lakko.app.contacts.PendingContact? {
        val link = standardBase64OfLength(c.linkIdB64, LINK_ID_LEN) ?: return null
        return me.lakko.app.contacts.PendingContact(
            accountIdHex = c.accountIdHex,
            signaturePubkeyB64 = c.signaturePubkeyB64,
            x25519PubkeyB64 = c.x25519PubkeyB64,
            displayName = c.displayName,
            username = c.username,
            addedAtMs = c.addedAtMs,
            linkIdHex = hexOf(link),
        )
    }

    /** A carried held message as a display-only copy. */
    internal fun heldCopyOf(m: CarriedHeldMessage): me.lakko.app.chat.HeldCopy =
        me.lakko.app.chat.HeldCopy(
            text = m.text,
            attachmentCount = m.attachments.size,
            replyTo = m.replyTo,
            heldAtMs = m.heldAtMs,
        )

    /** The account id of [accountIdB64] as lower-case hex, or `null`. */
    private fun accountIdHexOf(accountIdB64: String): String? =
        try {
            Base64.getDecoder().decode(accountIdB64).takeIf { it.size == ACCOUNT_ID_LEN }?.let { hexOf(it) }
        } catch (_: IllegalArgumentException) {
            null
        }

    // ---- ADR-0074: predecessors (pure) ---------------------------------------

    /** The sending half: this device's own previous identities as the bundle
     *  carries them, oldest first. The caller zeroes [rows]. */
    internal fun carriedOwnPredecessors(rows: List<me.lakko.app.auth.OwnPredecessor>): List<CarriedOwnPredecessor> =
        rows.take(MAX_PREDECESSORS).map {
            CarriedOwnPredecessor(Base64.getEncoder().encodeToString(it.phraseSeed), it.supersededAtMs)
        }

    /** The sending half: the proven predecessors of [pinnedKeyB64], oldest
     *  first. A chain longer than the bound keeps its LATEST steps, so what
     *  is carried still ends at the pinned key. */
    internal fun provenPredecessorsOf(
        pinnedKeyB64: String,
        links: List<me.lakko.app.contacts.ProvenLink>,
    ): List<String> =
        me.lakko.app.contacts.keysOfPerson(pinnedKeyB64, links)
            .takeWhile { it != pinnedKeyB64 }
            .takeLast(MAX_PREDECESSORS)

    /** What [receiveOwnPredecessors] did. */
    public class OwnPredecessorReceipt(
        /** How many of this account's earlier keys are held after it. */
        public val held: Int,
        /** Whether the key this account's conversation leaves are bound to
         *  (the EARLIEST own predecessor) is another one than before. */
        public val conversationKeyChanged: Boolean,
    )

    /**
     * The predecessor rows a device holds after a bundle: the carried ones,
     * oldest first, then any row this device already held that the bundle
     * does not name (a key it adopted itself after the sender built the
     * bundle). A row this device already holds for a carried key is kept as
     * it is (its own adoption time and mailbox cursor).
     */
    internal fun mergedOwnPredecessors(
        held: List<me.lakko.app.auth.OwnPredecessor>,
        carried: List<me.lakko.app.auth.OwnPredecessor>,
    ): List<me.lakko.app.auth.OwnPredecessor> {
        val out = ArrayList<me.lakko.app.auth.OwnPredecessor>()
        for (row in carried) {
            if (out.any { it.signaturePubkey.contentEquals(row.signaturePubkey) }) continue
            out += held.firstOrNull { it.signaturePubkey.contentEquals(row.signaturePubkey) } ?: row
        }
        for (row in held) {
            if (out.none { it.signaturePubkey.contentEquals(row.signaturePubkey) }) out += row
        }
        if (out.size <= me.lakko.app.auth.MAX_OWN_PREDECESSORS) return out
        // The earliest is the one the conversation leaves name; the oldest
        // of the rest go.
        return listOf(out.first()) + out.takeLast(me.lakko.app.auth.MAX_OWN_PREDECESSORS - 1)
    }

    /**
     * The receiving half of `ownPredecessors`: store this account's earlier
     * keys exactly as a kept device that adopted each rotation holds them
     * (public key, seed, the SENDER's adoption time as the start of that
     * mailbox's 30-day read window, cursor 0), and record the proven steps
     * between them. The step from the latest predecessor to the current key
     * is recorded when [currentAccountKey] is known; a device that does not
     * hold its seed yet records it when the chat layer first loads
     * ([ownChainLinks]).
     *
     * [pubkeyOfSeed] is `Identity.fromSeed(seed).signaturePublicKey()`.
     * Returns `null` when the rows could not be stored (nothing is recorded).
     */
    internal fun receiveOwnPredecessors(
        store: me.lakko.app.auth.OwnPredecessorStore,
        links: me.lakko.app.contacts.ProvenLinkStore,
        accountIdB64: String,
        carried: List<CarriedOwnPredecessor>,
        currentAccountKey: ByteArray?,
        pubkeyOfSeed: (ByteArray) -> ByteArray,
        nowMs: Long = System.currentTimeMillis(),
    ): OwnPredecessorReceipt? {
        val seeds = decodedOwnPredecessors(carried) ?: return null
        return try {
            receiveOwnPredecessorSeeds(store, links, accountIdB64, seeds, currentAccountKey, pubkeyOfSeed, nowMs)
        } finally {
            seeds.forEach { it.seed.fill(0) }
        }
    }

    /** One of this account's earlier keys as it arrived, the seed raw. */
    public class OwnPredecessorSeed(
        public val seed: ByteArray,
        /** When the account adopted the key that replaced it (unix ms). */
        public val adoptedAtMs: Long,
    ) {
        override fun toString(): String = "OwnPredecessorSeed(<redacted>)"
    }

    /** [carried] decoded, or `null` when one of them is not a seed. */
    private fun decodedOwnPredecessors(carried: List<CarriedOwnPredecessor>): List<OwnPredecessorSeed>? {
        val seeds = ArrayList<OwnPredecessorSeed>()
        for (c in carried.take(MAX_PREDECESSORS)) {
            val seed = standardBase64OfLength(c.seedB64, SEED_LEN)
            if (seed == null) {
                seeds.forEach { it.seed.fill(0) }
                return null
            }
            seeds += OwnPredecessorSeed(seed, c.adoptedAtMs)
        }
        return seeds
    }

    /**
     * [receiveOwnPredecessors] over raw seeds: what a pairing bundle and a
     * restored backup (ADR-0074 section 3.7) both end in. [seeds] stay the
     * caller's to zero.
     */
    internal fun receiveOwnPredecessorSeeds(
        store: me.lakko.app.auth.OwnPredecessorStore,
        links: me.lakko.app.contacts.ProvenLinkStore,
        accountIdB64: String,
        seeds: List<OwnPredecessorSeed>,
        currentAccountKey: ByteArray?,
        pubkeyOfSeed: (ByteArray) -> ByteArray,
        nowMs: Long = System.currentTimeMillis(),
    ): OwnPredecessorReceipt? {
        val held = store.load(accountIdB64)
        if (seeds.isEmpty()) {
            held.forEach { it.zero() }
            return OwnPredecessorReceipt(held.size, conversationKeyChanged = false)
        }
        val rows = ArrayList<me.lakko.app.auth.OwnPredecessor>()
        try {
            for (c in seeds.take(MAX_PREDECESSORS)) {
                if (c.seed.size != SEED_LEN) return null
                val seed = c.seed.copyOf()
                rows += me.lakko.app.auth.OwnPredecessor(pubkeyOfSeed(seed), seed, c.adoptedAtMs)
            }
            val merged = mergedOwnPredecessors(held, rows)
            if (!store.save(accountIdB64, merged)) return null
            val keys = ownChainLinks(
                predecessorKeysB64 = rows.map { Base64.getEncoder().encodeToString(it.signaturePubkey) },
                currentKeyB64 = currentAccountKey?.let { Base64.getEncoder().encodeToString(it) },
            )
            for ((older, newer) in keys) {
                me.lakko.app.contacts.ProvenPredecessors.recordProven(
                    store = links,
                    ownerAccountIdB64 = accountIdB64,
                    accountIdB64 = accountIdB64,
                    oldKeyB64 = older,
                    newKeyB64 = newer,
                    nowMs = nowMs,
                )
            }
            val before = held.firstOrNull()?.signaturePubkey
            val after = merged.first().signaturePubkey
            return OwnPredecessorReceipt(
                held = merged.size,
                conversationKeyChanged = before == null || !before.contentEquals(after),
            )
        } catch (_: Throwable) {
            return null
        } finally {
            rows.forEach { it.zero() }
            held.forEach { it.zero() }
        }
    }

    /** The proven steps of this account's own key history: each predecessor
     *  to the next, and the latest to [currentKeyB64] when it is known and is
     *  not itself the latest. Oldest first. */
    internal fun ownChainLinks(predecessorKeysB64: List<String>, currentKeyB64: String?): List<Pair<String, String>> {
        val chain = predecessorKeysB64 + listOfNotNull(currentKeyB64?.takeIf { it != predecessorKeysB64.lastOrNull() })
        return chain.zipWithNext().filter { (a, b) -> a != b }
    }

    /**
     * The receiving half of a pin's `provenPredecessorsB64`: each carried key
     * is a proven predecessor of the next, and the last of the pinned key.
     * Recorded only when the pin this device now holds for the account IS the
     * carried one ([heldPinnedKeyB64]): a pin the monotone setter refused
     * names another key, and the sender's history is not this device's then.
     * Returns how many steps were recorded.
     */
    internal fun receiveContactPredecessors(
        links: me.lakko.app.contacts.ProvenLinkStore,
        ownerAccountIdB64: String,
        pin: CarriedPin,
        heldPinnedKeyB64: String?,
        nowMs: Long = System.currentTimeMillis(),
    ): Int {
        if (pin.provenPredecessorsB64.isEmpty() || heldPinnedKeyB64 != pin.pinnedSignaturePubkeyB64) return 0
        val account = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(pin.accountIdB64)
        val before = links.load(ownerAccountIdB64).size
        for ((older, newer) in (pin.provenPredecessorsB64 + pin.pinnedSignaturePubkeyB64).zipWithNext()) {
            me.lakko.app.contacts.ProvenPredecessors.recordProven(links, ownerAccountIdB64, account, older, newer, nowMs)
        }
        return links.load(ownerAccountIdB64).size - before
    }

    /**
     * The receiving half: set by the process-lived contacts repository so a
     * bundle's contacts reach the live list (the store alone would be
     * overwritten by the repository's next persist). Called with the account
     * the bundle belongs to; returns `null` when the repository is not bound
     * to that account, and the bundle is then written to that account's
     * store directly.
     */
    @Volatile
    public var contactsSink: ((String, List<me.lakko.app.contacts.AcceptedContact>) -> Int?)? = null

    /**
     * v3 — the receiving half for pending contacts, set like [contactsSink]
     * by the process-lived contacts repository so they reach its live list.
     * Returns how many were added, or `null` when the repository is not bound
     * to that account (the bundle then writes that account's store directly).
     */
    @Volatile
    public var pendingContactsSink: ((String, List<me.lakko.app.contacts.PendingContact>) -> Int?)? = null

    /** What [apply] did. Destructures as `(contacts, pins)`. */
    public data class Applied(
        val contacts: Int,
        val pins: Int,
        /** ADR-0074: this account's earlier keys held after the bundle. */
        val ownPredecessors: Int = 0,
        /** ADR-0074: the key the conversation leaves are bound to changed;
         *  a chat layer already loaded must be rebuilt under it. */
        val conversationKeyChanged: Boolean = false,
        /** v3: group names stored (strictly newer than what was held). */
        val groupNames: Int = 0,
        /** v3: profiles stored (strictly newer than what was held). */
        val profiles: Int = 0,
        /** v3: pending contacts added. */
        val pendingContacts: Int = 0,
    )

    /**
     * ADR-0074: run after a bundle moved the account's conversation key on a
     * device whose chat layer may already be loaded under the current key,
     * with the account the bundle was for. Set by the process-lived contacts
     * repository, which rebuilds the chat layer's identity under the
     * conversation key and republishes the KeyPackage.
     */
    @Volatile
    public var conversationKeySink: (suspend (String) -> Unit)? = null

    /**
     * Store [seeds] on this device for [accountIdB64]: the one place a bundle
     * and a restored backup do it. The step to the current key is recorded
     * when the account's seed is on the device. `null` when not stored.
     */
    public fun storeOwnPredecessorSeeds(
        context: Context,
        accountIdB64: String,
        seeds: List<OwnPredecessorSeed>,
    ): OwnPredecessorReceipt? {
        val owner = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        return try {
            val current = me.lakko.app.auth.AccountIdentitySeedStore.getAccountIdentitySeed(context, owner)
            val currentKey = try {
                current?.let { seed -> uniffi.lakko_ffi_uniffi.Identity.fromSeed(seed).use { it.signaturePublicKey() } }
            } finally {
                current?.fill(0)
            }
            receiveOwnPredecessorSeeds(
                store = me.lakko.app.auth.OwnPredecessorStore.onDevice(context),
                links = me.lakko.app.contacts.SharedPrefsProvenLinkStore(context),
                accountIdB64 = owner,
                seeds = seeds,
                currentAccountKey = currentKey,
                pubkeyOfSeed = { seed -> uniffi.lakko_ffi_uniffi.Identity.fromSeed(seed).use { it.signaturePublicKey() } },
            )
        } catch (cause: Throwable) {
            android.util.Log.w("StateBundle", "own predecessors not stored: ${cause.javaClass.simpleName}")
            null
        }
    }

    /** Tell [conversationKeySink] when [applied] moved the conversation key.
     *  Best-effort: a device whose chat layer is not loaded reads the
     *  predecessor store when it loads. */
    public suspend fun afterApply(accountIdB64: String, applied: Applied) {
        if (!applied.conversationKeyChanged) return
        try {
            conversationKeySink?.invoke(accountIdB64)
        } catch (cause: CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("StateBundle", "conversation identity not rebuilt: ${cause.javaClass.simpleName}")
        }
    }

    /** Merge [bundle] into [accountIdB64]'s state on this device: contacts
     *  not here yet, pins through the monotone setter, the known group ids
     *  for the chat list, and (ADR-0074) this account's earlier keys and each
     *  contact's proven predecessors. */
    public fun apply(context: Context, accountIdB64: String, bundle: Bundle): Applied =
        apply(context, accountIdB64, bundle, FfiEnvelopeCodecs, ownKeysOverride = null, profileOffer = null)

    /** [apply] with the envelope codecs, this account's own keys, the profile
     *  store and the pending stores injected (the seams a host test drives). */
    internal fun apply(
        context: Context,
        accountIdB64: String,
        bundle: Bundle,
        codecs: EnvelopeCodecs,
        ownKeysOverride: Set<String>?,
        profileOffer: ((String, me.lakko.app.profile.PeerProfile) -> me.lakko.app.profile.ProfileOffer)?,
        pendingStore: me.lakko.app.contacts.PendingContactsStore? = null,
        copyStore: me.lakko.app.chat.HeldCopyStore? = null,
    ): Applied {
        val owner = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        val linkStore = me.lakko.app.contacts.SharedPrefsProvenLinkStore(context)
        // The own predecessors FIRST: nothing below may leave the device
        // bound under the current key because a later step threw.
        val receipt = if (bundle.ownPredecessors.isEmpty()) {
            null
        } else {
            decodedOwnPredecessors(bundle.ownPredecessors)?.let { seeds ->
                try {
                    storeOwnPredecessorSeeds(context, owner, seeds)
                } finally {
                    seeds.forEach { it.seed.fill(0) }
                }
            }
        }
        val dec = Base64.getDecoder()
        val incoming = bundle.contacts.mapIndexedNotNull { idx, c ->
            val pubkey = try {
                dec.decode(c.signaturePubkeyB64)
            } catch (_: Throwable) {
                return@mapIndexedNotNull null
            }
            me.lakko.app.contacts.AcceptedContact(
                request = me.lakko.app.contacts.IncomingContactRequest(
                    // A negative sentinel id never collides with a DS id; offset
                    // past the backup-restore sentinels.
                    messageId = -(BUNDLE_ID_BASE + idx),
                    fromPubkey = pubkey,
                    fromAccountId = c.accountIdHex?.let { bytesOfHex(it) }?.takeIf { it.size == 16 },
                    fromPseudonym = ByteArray(32),
                    validAfterMs = c.acceptedAtMs.coerceAtLeast(0L).toULong(),
                    intro = null,
                    createdAt = "",
                ),
                groupIdHex = c.groupIdB64?.let {
                    try {
                        hexOf(dec.decode(it))
                    } catch (_: Throwable) {
                        null
                    }
                },
                displayName = c.displayName.ifEmpty { null },
            )
        }
        val contactsApplied = contactsSink?.invoke(accountIdB64, incoming) ?: run {
            val existing = me.lakko.app.contacts.AcceptedContactsStore.load(context, accountIdB64)
            val known = existing.map { it.request.fromPubkey.toList() }.toHashSet()
            val fresh = incoming.filter { known.add(it.request.fromPubkey.toList()) }
            if (fresh.isNotEmpty()) {
                me.lakko.app.contacts.AcceptedContactsStore.save(context, accountIdB64, existing + fresh)
            }
            fresh.size
        }
        val pinStore = me.lakko.app.contacts.PeerPinStore(
            context,
            me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64),
        )
        var pins = 0
        for (p in bundle.pins) {
            try {
                val result = pinStore.setPin(p.accountIdB64, p.pinnedSignaturePubkeyB64, p.rotationMarker)
                if (result == me.lakko.app.contacts.SetPinResult.Set) pins += 1
            } catch (_: Throwable) {
                // A pin the setter refuses is not ours to force.
            }
            try {
                receiveContactPredecessors(
                    links = linkStore,
                    ownerAccountIdB64 = owner,
                    pin = p,
                    heldPinnedKeyB64 = pinStore.readPin(p.accountIdB64)?.pinnedSignaturePubkeyB64,
                )
            } catch (_: Throwable) {
                // The step is then learned from the contact's own proof.
            }
        }
        saveKnownGroupIds(
            context,
            accountIdB64,
            bundle.knownGroupIdsB64.mapNotNull {
                try {
                    hexOf(dec.decode(it))
                } catch (_: Throwable) {
                    null
                }
            },
        )
        // v3 — the pending contacts, after the accepted ones (a person carried
        // as both is an accepted contact), with what was written to them as
        // display-only copies.
        val pendingApplied = try {
            applyPendingContacts(
                context = context,
                accountIdB64 = owner,
                carried = bundle.pendingContacts,
                store = pendingStore,
                copies = copyStore,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("StateBundle", "pending contacts not stored: ${cause.javaClass.simpleName}")
            0
        }
        // v3 — the shared group names, through the group-name store.
        val namesApplied = try {
            applyGroupNames(
                entries = bundle.groupNames,
                decode = codecs.decodeGroupName,
                isNewer = codecs.groupNameIsNewer,
                load = { hex -> me.lakko.app.chat.GroupNameStore.load(context, accountIdB64, hex) },
                save = { hex, value -> me.lakko.app.chat.GroupNameStore.save(context, accountIdB64, hex, value) },
            )
        } catch (cause: Throwable) {
            android.util.Log.w("StateBundle", "group names not stored: ${cause.javaClass.simpleName}")
            0
        }
        // v3 — the profiles, never under this account's own key.
        val profilesApplied = try {
            val ownKeys = ownKeysOverride ?: ownKeysOnDevice(context, owner, bundle.ownPredecessors)
            applyProfiles(
                entries = bundle.profiles,
                ownKeysB64 = ownKeys,
                ownAccountIdHex = accountIdHexOf(owner),
                decode = codecs.decodeProfile,
                offer = profileOffer ?: { key, profile ->
                    me.lakko.app.profile.ProfileDirectory.get(context).offerForAccount(owner, key, profile)
                },
            )
        } catch (cause: Throwable) {
            android.util.Log.w("StateBundle", "profiles not stored: ${cause.javaClass.simpleName}")
            0
        }
        return Applied(
            contacts = contactsApplied,
            pins = pins,
            ownPredecessors = receipt?.held ?: 0,
            conversationKeyChanged = receipt?.conversationKeyChanged == true,
            groupNames = namesApplied,
            profiles = profilesApplied,
            pendingContacts = pendingApplied,
        )
    }

    /** This account's own keys as this device knows them: the current key
     *  (when its seed is held), the earlier keys it holds, and the earlier
     *  keys the bundle carried. Each part best-effort. */
    private fun ownKeysOnDevice(context: Context, owner: String, carried: List<CarriedOwnPredecessor>): Set<String> {
        val enc = Base64.getEncoder()
        val out = HashSet<String>()
        currentAccountKeyB64(context, owner)?.let { out += it }
        try {
            val rows = me.lakko.app.auth.OwnPredecessorStore.onDevice(context).load(owner)
            try {
                rows.forEach { out += enc.encodeToString(it.signaturePubkey) }
            } finally {
                rows.forEach { it.zero() }
            }
        } catch (_: Throwable) {
            // not readable: the carried ones and the current key remain
        }
        for (c in carried) {
            val seed = standardBase64OfLength(c.seedB64, SEED_LEN) ?: continue
            try {
                out += uniffi.lakko_ffi_uniffi.Identity.fromSeed(seed).use { enc.encodeToString(it.signaturePublicKey()) }
            } catch (_: Throwable) {
                // skipped
            } finally {
                seed.fill(0)
            }
        }
        return out
    }

    /** The receiving half of `pendingContacts`: the records this device holds
     *  neither as accepted nor as pending go through the contacts
     *  repository's add (or that account's store when it is not bound); the
     *  messages written to a pending contact that is not an accepted contact
     *  here become display-only copies under its queue key. */
    private fun applyPendingContacts(
        context: Context,
        accountIdB64: String,
        carried: List<CarriedPendingContact>,
        store: me.lakko.app.contacts.PendingContactsStore?,
        copies: me.lakko.app.chat.HeldCopyStore?,
    ): Int {
        if (carried.isEmpty()) return 0
        val records = carried.mapNotNull { c -> pendingRecordOf(c)?.let { it to c } }
        val accepted = me.lakko.app.contacts.AcceptedContactsStore.load(context, accountIdB64)
        val incoming = records.map { it.first }
        val added = (if (store == null) pendingContactsSink?.invoke(accountIdB64, incoming) else null) ?: run {
            val pendingStore = store ?: me.lakko.app.contacts.PendingContactsFiles.onDevice(context)
            val held = pendingStore.load(accountIdB64)
            val fresh = freshPendingContacts(incoming, accepted, held)
            if (fresh.isNotEmpty()) {
                pendingStore.save(accountIdB64, fresh.fold(held) { acc, c -> me.lakko.app.contacts.PendingContacts.adding(c, acc) })
            }
            fresh.size
        }
        val copyStore = copies ?: me.lakko.app.chat.HeldCopyStore.onDevice(context)
        val acceptedKeys = accepted.map { Base64.getEncoder().encodeToString(it.request.fromPubkey) }.toHashSet()
        val acceptedIds = accepted.mapNotNull { a -> a.request.fromAccountId?.let { hexOf(it) } }.toHashSet()
        for ((record, c) in records) {
            if (c.heldMessages.isEmpty()) continue
            if (record.signaturePubkeyB64 in acceptedKeys || record.accountIdHex in acceptedIds) continue
            try {
                copyStore.addCarried(accountIdB64, record.queueKey, c.heldMessages.map(::heldCopyOf))
            } catch (_: Throwable) {
                // best-effort: the original device still sends them
            }
        }
        return added
    }

    private const val BUNDLE_ID_BASE: Long = 1L shl 40

    // ---- known group ids (drives the one "Synchronizing..." label) ---------
    //
    // The list belongs to the ACCOUNT the pairing was for, not to the device:
    // until 2026-09-30 it lived under one device-wide key, so a second account
    // signed in on the same phone showed the first account's
    // "Synchronizing..." for groups it would never join. The key is
    // `known-groups.v2:<accountIdB64>` (standard base64); with no account a
    // read is empty and a write is dropped.
    //
    // It is the ONLY thing behind the label. Until 2026-10-02 a second rule
    // showed it for any account that landed through a recovery route and held
    // no chat yet (a `restored.v2:<accountIdB64>` marker); that rule is gone -
    // a device restored from the phrase with no other device alive has nobody
    // synchronising it - and so is its marker. The group ids a restored BACKUP
    // named live in `backup.BackupRestoreStore` and never feed the label.
    //
    // The retired rows (`known-groups.v1`, `restored.v1`, `restored.v2:...`)
    // are never read; [forgetAccount] removes them.

    private const val PREFS = "lakko-pairing-known-groups"
    private const val KNOWN_PREFIX = "known-groups.v2:"
    private const val RETIRED_RESTORED_PREFIX = "restored.v2:"
    private const val RETIRED_KNOWN_KEY = "known-groups.v1"
    private const val RETIRED_RESTORED_KEY = "restored.v1"

    /** The row key of [accountIdB64]'s known group ids. Visible for tests. */
    internal fun knownKey(accountIdB64: String): String =
        KNOWN_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    /** The row key of the retired restored marker; only ever removed. */
    private fun retiredRestoredKey(accountIdB64: String): String =
        RETIRED_RESTORED_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    public fun saveKnownGroupIds(context: Context, accountIdB64: String, idsHex: Collection<String>) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                if (idsHex.isEmpty()) {
                    prefs.edit().remove(knownKey(accountIdB64)).apply()
                } else {
                    prefs.edit().putStringSet(knownKey(accountIdB64), idsHex.toSet()).apply()
                }
            } catch (_: Throwable) {
                // storage unavailable: the label falls back to "no chats yet"
            }
        }
    }

    /** The known groups [accountIdB64] has not joined on this device yet; `0`
     *  with no account. Clears the stored list once every known group is
     *  held, so the label cannot outlive its cause. */
    public fun pendingKnownGroups(context: Context, accountIdB64: String?, joinedIdsHex: Collection<String>): Int {
        if (accountIdB64 == null) return 0
        val known = try {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .getStringSet(knownKey(accountIdB64), emptySet()).orEmpty()
        } catch (_: Throwable) {
            emptySet()
        }
        if (known.isEmpty()) return 0
        val joined = joinedIdsHex.toSet()
        val pending = known.count { it !in joined }
        if (pending == 0) saveKnownGroupIds(context, accountIdB64, emptyList())
        return pending
    }

    /**
     * Whether the chat list shows the single "Synchronizing..." label for
     * [accountIdB64]: the pairing bundle named groups this device has not
     * joined yet. The whole rule - there is no other clause.
     */
    public fun showsSynchronizing(
        context: Context,
        accountIdB64: String?,
        joinedIdsHex: Collection<String>,
    ): Boolean = pendingKnownGroups(context, accountIdB64, joinedIdsHex) > 0

    /** Forget device: drop [accountIdB64]'s known group ids, and the retired
     *  rows. Other accounts' stay. */
    public fun forgetAccount(context: Context, accountIdB64: String) {
        try {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                .remove(knownKey(accountIdB64))
                .remove(retiredRestoredKey(accountIdB64))
                .remove(RETIRED_KNOWN_KEY)
                .remove(RETIRED_RESTORED_KEY)
                .apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    // ---- the tagged mailbox row --------------------------------------------

    public fun wrapRow(sealed: ByteArray): ByteArray = ROW_MAGIC + sealed

    public fun isStateRow(row: ByteArray): Boolean {
        if (row.size <= ROW_MAGIC.size) return false
        for (i in ROW_MAGIC.indices) if (row[i] != ROW_MAGIC[i]) return false
        return true
    }

    public fun unwrapRow(row: ByteArray): ByteArray? =
        if (isStateRow(row)) row.copyOfRange(ROW_MAGIC.size, row.size) else null

    public suspend fun publish(rendezvousPseudonym: ByteArray, sealed: ByteArray) {
        DsClient().use { it.publishRendezvous(rendezvousPseudonym, wrapRow(sealed)) }
    }

    /** Joiner side: the state row past the Welcome cursor, skipping the
     *  history and recovery rows. Soft timeout; an older scanner sends none. */
    public suspend fun poll(rendezvousPseudonym: ByteArray, afterCursor: Long): ByteArray? =
        DsClient().use { client ->
            val deadline = System.nanoTime() + POLL_TIMEOUT_MS * 1_000_000L
            var cursor = afterCursor
            while (true) {
                if (!currentCoroutineContext().isActive) return@use null
                val remaining = deadline - System.nanoTime()
                if (remaining <= 0L) return@use null
                val waitSecs = minOf(
                    PER_REQUEST_LONG_POLL_SECS,
                    (remaining / 1_000_000_000L).toInt().coerceAtLeast(1),
                )
                val result = try {
                    client.fetchRendezvous(rendezvousPseudonym, after = cursor, limit = 1, waitSecs = waitSecs)
                } catch (c: CancellationException) {
                    throw c
                } catch (http: DsHttpError) {
                    if (http.status == 404) continue
                    throw http
                } catch (_: DsNetworkError) {
                    continue
                }
                val first = result.messages.firstOrNull()
                if (first != null) {
                    unwrapRow(first.payload)?.let { return@use it }
                    cursor = first.id
                    continue
                }
            }
            @Suppress("UNREACHABLE_CODE")
            null
        }

    private fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    private fun bytesOfHex(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        for (i in out.indices) {
            val hi = Character.digit(hex[i * 2], 16)
            val lo = Character.digit(hex[i * 2 + 1], 16)
            if (hi < 0 || lo < 0) return null
            out[i] = ((hi shl 4) or lo).toByte()
        }
        return out
    }
}
