// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import uniffi.lakko_ffi_uniffi.Identity

/**
 * ADR-0090 section 1 - where a channel's PublisherGroup lives on this device:
 * the device's bound conversation identity, in the provider that holds the
 * conversation leaves. That is the provider whose KeyPackage the directory
 * serves for this device, so the Welcome an owner device writes after
 * admitting this device (section 2.1) opens here.
 *
 * Implemented by the chat repository; every call on [locked] runs under its
 * provider lock (no network inside), and [persistDurably] writes the
 * provider snapshot under the same lock, throwing when it was not written.
 */
public interface ChannelDevicePort {
    /** This device's bound identity for [accountIdB64], or `null` while it is
     *  not bound (no device seed or binding yet, another account). */
    public fun deviceFor(accountIdB64: String): Identity?

    /** Run [block] on [device] under the provider lock. Throws when the
     *  bound identity changed. */
    public suspend fun <T> locked(device: Identity, block: (Identity) -> T): T

    /** Snapshot the provider durably; throws when it was not written. */
    public suspend fun persistDurably(device: Identity)
}

/**
 * ADR-0090 - the two identities a channel publisher path acts as.
 *
 *  * [account] - the account (conversation) identity of
 *    [ChannelPublisherIdentity]: the owner seed a PublisherGroup id is
 *    derived from, the journal root, owner and co-publisher proofs, the
 *    account mailbox, the X25519 key hand-over envelopes are sealed to.
 *  * [device] - this device's bound leaf: every PublisherGroup call that
 *    reads or writes MLS state takes it, and only through [mls].
 */
public class ChannelIdentity(
    public val account: Identity,
    public val device: Identity,
    private val port: ChannelDevicePort,
) {
    /** The owner account seed passed to `publisherGroupLoad` / create
     *  (the caller wipes it). */
    public fun ownerAccountSeed(): ByteArray = account.ed25519Seed()

    /** The owner account key `publisherOwnerAccountKey` gives for
     *  [ownerAccountSeed]: this account's signature key. */
    public fun accountKey(): ByteArray = account.signaturePublicKey()

    /** Run an MLS call on [device] under the provider lock. */
    public suspend fun <T> mls(block: (Identity) -> T): T = port.locked(device, block)

    /** Persist the provider durably (before anything leaves the device). */
    public suspend fun persist(): Unit = port.persistDurably(device)
}

/** The PublisherGroup calls need this device's bound identity and there is
 *  none yet (the binding is minted by the chat bootstrap). */
public class NoChannelDeviceException : IllegalStateException("this device's bound identity is not ready")
