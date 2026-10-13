// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0073 R.5 - the composer's unsent text belongs to the CONVERSATION.
 *
 * The chat screen's view model is bound to one group, the conversation's
 * current one. When the conversation moves to its successor (the other
 * person reset their account keys) the screen is rebuilt on the new group
 * with a new view model, and what the person had typed and not sent would go
 * with the old one: nothing they did, and nothing they were told about (R.0).
 *
 * So the text is kept here, per conversation id (the lineage's first group,
 * which no move changes), for as long as the process lives. The view model
 * writes every change and reads once when it is built. The cross-device
 * draft register does the same between devices, half a second late and only
 * while the sync runtime is attached; this is what holds in between.
 *
 * In memory only: an unsent text is message plaintext, and it is not written
 * to disk by this.
 */
public class ComposerCarry {
    private val texts = java.util.concurrent.ConcurrentHashMap<String, String>()

    /** The composer of [conversationHex] now holds [text]. An empty text
     *  (sent, or cleared) forgets the conversation. */
    public fun note(conversationHex: String, text: String) {
        val key = conversationHex.lowercase()
        if (text.isEmpty()) texts.remove(key) else texts[key] = text
    }

    /** The unsent text of [conversationHex], or `null`. */
    public fun textOf(conversationHex: String): String? = texts[conversationHex.lowercase()]

    /** The session ended. */
    public fun clear() {
        texts.clear()
    }
}

/**
 * What a composer starts with.
 *
 * | the composer already holds text | the conversation has unsent text | it starts with |
 * |---------------------------------|----------------------------------|----------------|
 * | yes                             | any                              | its own        |
 * | no                              | yes                              | the unsent one |
 * | no                              | no                               | nothing        |
 */
public fun composerSeed(current: String, carried: String?): String =
    if (current.isNotEmpty() || carried.isNullOrEmpty()) current else carried
