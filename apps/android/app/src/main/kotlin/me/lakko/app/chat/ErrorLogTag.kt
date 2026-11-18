// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * A log tag for a failure that carries no identifier (ADR-0091 phase 1b).
 *
 * A throwable's message is not safe to log: a DS error names its endpoint, and
 * the endpoint names the journal (`/ds/group/<hex>/...`). The tag keeps what
 * explains a failure and nothing that names a conversation, a row, a key or an
 * account: the class names along the cause chain, the HTTP status of a DS
 * refusal, and a typed membership refusal code. Never the message, the body
 * or the endpoint. Mirrors web `chat/log-tag.ts`.
 */

private const val MAX_TAG_DEPTH: Int = 6

/** `Name[>Name...][ status=N][ refusal=code]`, never the message text. */
internal fun errorLogTag(error: Throwable): String {
    val chain = generateSequence(error) { it.cause }.take(MAX_TAG_DEPTH).toList()
    val names = chain.joinToString(">") { it.javaClass.simpleName.ifEmpty { "Throwable" } }
    val status = chain.firstNotNullOfOrNull { (it as? me.lakko.app.ds.DsHttpError)?.status }
    val refusal = try {
        membershipRefusalReason(error)?.let { refusalCode(it.name) }
    } catch (_: Throwable) {
        null
    }
    return names +
        (status?.let { " status=$it" } ?: "") +
        (refusal?.let { " refusal=$it" } ?: "")
}
