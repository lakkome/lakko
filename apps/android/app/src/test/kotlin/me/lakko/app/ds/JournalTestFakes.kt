// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

/** A [DsJournalSigner] at [epoch] whose verifying key is [epoch] repeated. */
public class FakeJournalSigner(
    override val epoch: ULong,
    override val verifyingKey: ByteArray = ByteArray(32) { epoch.toByte() },
) : DsJournalSigner {
    override fun sign(method: String, path: String, query: String, body: ByteArray): String = "p$epoch"

    override fun mailboxClaimBody(): String = "{}"
}

/**
 * A [DsStatementSigning] that records what it was asked to sign. [name] ends
 * up in every statement's path, so a test can tell the root from a registrar.
 */
public class RecordingStatementSigner(
    public val name: String,
    override val publicKey: ByteArray = ByteArray(32) { name.hashCode().toByte() },
) : DsStatementSigning {
    public val calls: MutableList<String> = mutableListOf()
    public var lastRevoke: List<ByteArray> = emptyList()
    public var lastRegistrars: List<JournalRegistrarChange> = emptyList()

    private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }

    override fun claim(journalId: ByteArray, epoch: ULong, epochKey: ByteArray): DsStatement {
        calls += "claim@$epoch"
        return DsStatement("/ds/group/${hex(journalId)}/claim", """{"by":"$name"}""")
    }

    override fun reset(journalId: ByteArray, epoch: ULong, epochKey: ByteArray): DsStatement {
        calls += "reset@$epoch"
        return DsStatement("/ds/group/${hex(journalId)}/reset", """{"by":"$name"}""")
    }

    override fun registerEpoch(
        journalId: ByteArray,
        epoch: ULong,
        epochKey: ByteArray,
        revoke: List<ByteArray>,
    ): DsStatement {
        calls += "register@$epoch"
        lastRevoke = revoke
        return DsStatement("/ds/group/${hex(journalId)}/access", """{"by":"$name"}""")
    }

    public var lastRegistrarSet: List<JournalRegistrarEntry> = emptyList()

    override fun replaceEpoch(
        journalId: ByteArray,
        epoch: ULong,
        epochKey: ByteArray,
        revoke: List<ByteArray>,
    ): DsStatement {
        calls += "replace@$epoch"
        lastRevoke = revoke
        return DsStatement("/ds/group/${hex(journalId)}/access", """{"by":"$name","mode":"replace"}""")
    }

    override fun registrarsSet(journalId: ByteArray, entries: List<JournalRegistrarEntry>): DsStatement {
        calls += "registrars-set"
        lastRegistrarSet = entries
        return DsStatement("/ds/group/${hex(journalId)}/registrars-set", """{"by":"$name"}""")
    }

    override fun registrars(journalId: ByteArray, changes: List<JournalRegistrarChange>): DsStatement {
        calls += "registrars"
        lastRegistrars = changes
        return DsStatement("/ds/group/${hex(journalId)}/registrars", """{"by":"$name"}""")
    }

    public var lastNewRoot: ByteArray? = null

    override fun rootTransfer(journalId: ByteArray, newRoot: ByteArray): DsStatement {
        calls += "root"
        lastNewRoot = newRoot
        return DsStatement("/ds/group/${hex(journalId)}/root", """{"by":"$name"}""")
    }
}
