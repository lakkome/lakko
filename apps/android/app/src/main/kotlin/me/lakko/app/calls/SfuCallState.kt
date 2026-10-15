// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * Asking the SFU whether a call is still there (`services/sfu` call lifetime;
 * `docs/ux/calls.md` section 4.3).
 *
 * A group call's record lives in the chat's MLS group and a member whose app
 * died publishes nothing, so after a crash the call it was alone in stays "in
 * progress" on every other phone for the freshness window, and whoever taps
 * Join lands in an empty room. The one party that knows a room is empty is the
 * SFU: the call ends there five seconds after the last socket goes, and
 * `GET /v1/calls/{call_id}` (`Authorization: Bearer <call jwt>`) says so. A
 * phone showing the call
 * as joinable asks every [SFU_CALL_STATE_PROBE_MS] and, on `ENDED`, closes the
 * chat's record itself by publishing the End signal — so the banner goes on
 * every device, not only its own. Shared word-for-word with web and iOS.
 */

/** How often a device offering Join asks the SFU. Chosen: a stale banner
 *  lives at most this long past the SFU's own five-second grace. */
public const val SFU_CALL_STATE_PROBE_MS: Long = 10_000L

/** What the SFU knows about a call id. */
public enum class SfuCallState { UNKNOWN, LIVE, CLOSING, ENDED }

/**
 * The state route for [callIdHex], on the HTTP origin behind the SFU's
 * WebSocket base (`wss://host/sfu` -> `https://host/sfu`). The token never
 * rides the URL (invariant #5: a request line in a log must not name who asked
 * about which call); it goes in the header [sfuCallStateAuthorization] builds.
 */
public fun sfuCallStateUrl(sfuBaseUrl: String, callIdHex: String): String {
    val base = sfuBaseUrl
        .replace(Regex("^wss:", RegexOption.IGNORE_CASE), "https:")
        .replace(Regex("^ws:", RegexOption.IGNORE_CASE), "http:")
        .trimEnd('/')
    return "$base/v1/calls/$callIdHex"
}

/** The `Authorization` value for the state route: the same call-admission JWT
 *  the socket uses, bound to this call id, as a bearer token. */
public fun sfuCallStateAuthorization(jwt: String): String = "Bearer $jwt"

/**
 * The route's answer, or `null` for anything that is not one of the four words
 * (a proxy error page, an old server) — the caller then changes nothing, since
 * a probe that cannot be read must never end a live call.
 */
public fun parseSfuCallState(body: String): SfuCallState? {
    val word = try {
        Json.parseToJsonElement(body).jsonObject["state"]?.jsonPrimitive?.content
    } catch (_: Exception) {
        null
    } ?: return null
    return when (word) {
        "unknown" -> SfuCallState.UNKNOWN
        "live" -> SfuCallState.LIVE
        "closing" -> SfuCallState.CLOSING
        "ended" -> SfuCallState.ENDED
        else -> null
    }
}
