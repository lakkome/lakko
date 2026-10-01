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

/**
 * ADR-0050 section 2.9 — the state bundle a pairing hands to the new device
 * beside the history bundle: accepted contacts (with their local renames), the
 * per-contact identity pins and rotation markers, and the ids of the groups the
 * old device knows.
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
    )

    /** The bundle; `v` is always 1. */
    public data class Bundle(
        val contacts: List<CarriedContact>,
        val pins: List<CarriedPin>,
        val knownGroupIdsB64: List<String>,
    )

    // ---- codec -------------------------------------------------------------

    public fun encode(bundle: Bundle): ByteArray {
        val obj = buildJsonObject {
            put("v", 1)
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
                            },
                        )
                    }
                },
            )
            put("knownGroupIdsB64", buildJsonArray { bundle.knownGroupIdsB64.forEach { add(JsonPrimitive(it)) } })
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
        if (root.number("v") != 1L) return null
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
            pins += CarriedPin(
                accountIdB64 = r.string("accountIdB64") ?: return null,
                pinnedSignaturePubkeyB64 = r.string("pinnedSignaturePubkeyB64") ?: return null,
                rotationMarker = r.number("rotationMarker") ?: return null,
            )
        }
        val groups = mutableListOf<String>()
        for (el in groupsArr) {
            val p = el as? JsonPrimitive ?: return null
            if (!p.isString) return null
            groups += p.content
        }
        return Bundle(contacts, pins, groups)
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
            CarriedPin(accountIdB64, pin.pinnedSignaturePubkeyB64, pin.rotationMarker)
        }
        val groups = knownGroupIdsHex.mapNotNull { bytesOfHex(it)?.let { b -> enc.encodeToString(b) } }
        return Bundle(contacts, pins, groups)
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

    /** Merge [bundle] into [accountIdB64]'s state on this device: contacts
     *  not here yet, pins through the monotone setter, the known group ids
     *  for the chat list. Returns the number of contacts and pins applied. */
    public fun apply(context: Context, accountIdB64: String, bundle: Bundle): Pair<Int, Int> {
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
        return contactsApplied to pins
    }

    private const val BUNDLE_ID_BASE: Long = 1L shl 40

    // ---- known group ids (drives the one "Synchronizing..." label) ---------
    //
    // Both facts below belong to the ACCOUNT the pairing or recovery was for,
    // not to the device: until 2026-09-30 they lived under one device-wide
    // key, so a second account signed in on the same phone showed the first
    // account's "Synchronizing..." for groups it would never join. Keys are
    // `known-groups.v2:<accountIdB64>` and `restored.v2:<accountIdB64>`
    // (standard base64); with no account a read is empty and a write is
    // dropped. The device-wide `known-groups.v1` / `restored.v1` rows are
    // never read; [forgetAccount] removes them.

    private const val PREFS = "lakko-pairing-known-groups"
    private const val KNOWN_PREFIX = "known-groups.v2:"
    private const val RESTORED_PREFIX = "restored.v2:"
    private const val RETIRED_KNOWN_KEY = "known-groups.v1"
    private const val RETIRED_RESTORED_KEY = "restored.v1"

    /** The row key of [accountIdB64]'s known group ids. Visible for tests. */
    internal fun knownKey(accountIdB64: String): String =
        KNOWN_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    /** The row key of [accountIdB64]'s restored marker. Visible for tests. */
    internal fun restoredKey(accountIdB64: String): String =
        RESTORED_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    public fun saveKnownGroupIds(context: Context, accountIdB64: String, idsHex: Collection<String>) {
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

    // ---- the restored-device marker ------------------------------------------

    /** Record that [accountIdB64] landed on this device through a recovery
     *  route (pairing join, recover): until its first chat arrives it is being
     *  admitted by its other devices or its contacts, which the chat list says
     *  as "Synchronizing...". */
    public fun markRestored(context: Context, accountIdB64: String) {
        try {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                .putBoolean(restoredKey(accountIdB64), true)
                .apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    /** Whether [accountIdB64]'s marker is set (`false` with no account);
     *  clears it once [joinedCount] is non-zero. */
    public fun restoredAwaitingChats(context: Context, accountIdB64: String?, joinedCount: Int): Boolean {
        if (accountIdB64 == null) return false
        val prefs = try {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        } catch (_: Throwable) {
            return false
        }
        val key = restoredKey(accountIdB64)
        val set = prefs.getBoolean(key, false)
        if (set && joinedCount > 0) prefs.edit().remove(key).apply()
        return set && joinedCount == 0
    }

    /** Forget device: drop [accountIdB64]'s known group ids and restored
     *  marker, and the retired device-wide rows. Other accounts' stay. */
    public fun forgetAccount(context: Context, accountIdB64: String) {
        try {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                .remove(knownKey(accountIdB64))
                .remove(restoredKey(accountIdB64))
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
