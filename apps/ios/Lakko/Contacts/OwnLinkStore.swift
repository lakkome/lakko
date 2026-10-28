// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// One invite link this device minted (ADR-0078 section 10): which ledger it
/// lives in, its ledger id, the seed of its own revocation key, and what the
/// owner's list shows about it.
public struct OwnLink: Codable, Equatable {
    public enum Kind: String, Codable { case contact, call, group }

    public let kind: Kind
    /// The ledger id as the URL slug (base64url, 8 characters).
    public let slug: String
    /// base64url of the 32-byte seed of the link's own revocation key. The
    /// ledger row holds only the public half; this seed is the only thing
    /// that revokes it.
    public let revocationSeed: String
    public let label: String
    /// UNIX ms; 0 = never.
    public let expiryMs: Int64
    /// The `max_uses` sentinel (0 single use, `UInt32.max` unlimited).
    public let maxUses: Int64
    public let bootstrapReferral: Bool
    public let mintedAtMs: Int64

    public init(
        kind: Kind, slug: String, revocationSeed: Data, label: String, expiryMs: Int64,
        maxUses: Int64, bootstrapReferral: Bool, mintedAtMs: Int64
    ) {
        self.kind = kind
        self.slug = slug
        self.revocationSeed = base64UrlEncodeNoPad(revocationSeed)
        self.label = label
        self.expiryMs = expiryMs
        self.maxUses = maxUses
        self.bootstrapReferral = bootstrapReferral
        self.mintedAtMs = mintedAtMs
    }

    /// The 32-byte revocation seed, or `nil` for a malformed record.
    public var seed: Data? {
        guard let data = base64UrlDecodeNoPad(revocationSeed), data.count == 32 else { return nil }
        return data
    }

    /// The 6-byte ledger id, or `nil` for a malformed slug.
    public var blobId: Data? { base64UrlDecodeNoPad(slug) }

    public func isExpired(nowMs: Int64) -> Bool { expiryMs != 0 && expiryMs <= nowMs }
}

/// A fresh revocation key for one link (ADR-0078 section 10): the seed the
/// device keeps and the public key the upload carries.
public enum LinkRevocationKey {
    public static func fresh() throws -> (seed: Data, publicKey: Data) {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw ContactsError.wrap(ContactsError.simple("no randomness for a link revocation key"))
        }
        let seed = Data(bytes)
        return (seed, try linkRevocationPublicKey(seed: seed))
    }
}

/// The invite links this device minted, per account (ADR-0078 section 10).
///
/// The ledgers name no owner, so there is no server list of an account's
/// links any more: the owner's "Invite links" sheet lists what this store
/// holds, and a revoke signs with the seed kept here. A link minted on another
/// device is listed and revoked there; one whose record is lost lives until
/// it expires or is used up. In the Keychain, this device only, never synced,
/// never logged.
public final class OwnLinkStore {
    public static let shared = OwnLinkStore()

    private let backend: InviteLinkSecretBackend

    public init(
        backend: InviteLinkSecretBackend = KeychainInviteLinkSecretBackend(serviceTag: "me.lakko.own-links.v1")
    ) {
        self.backend = backend
    }

    public func all(accountIdB64: String) -> [OwnLink] {
        guard let blob = backend.load(accountIdB64: accountIdB64),
              let decoded = try? JSONDecoder().decode([OwnLink].self, from: blob)
        else { return [] }
        return decoded
    }

    public func link(accountIdB64: String, kind: OwnLink.Kind, slug: String) -> OwnLink? {
        all(accountIdB64: accountIdB64).first { $0.kind == kind && $0.slug == slug }
    }

    /// Not written once the account's session has ended (``AccountStoreGate``).
    public func put(accountIdB64: String, _ link: OwnLink) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = all(accountIdB64: accountIdB64).filter { !($0.kind == link.kind && $0.slug == link.slug) }
        current.append(link)
        write(accountIdB64: accountIdB64, current)
    }

    public func remove(accountIdB64: String, kind: OwnLink.Kind, slug: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let current = all(accountIdB64: accountIdB64)
        let next = current.filter { !($0.kind == kind && $0.slug == slug) }
        guard next.count != current.count else { return }
        write(accountIdB64: accountIdB64, next)
    }

    /// Forget device: every record of the account.
    public func clear(accountIdB64: String) {
        backend.delete(accountIdB64: accountIdB64)
    }

    private func write(accountIdB64: String, _ links: [OwnLink]) {
        if links.isEmpty {
            backend.delete(accountIdB64: accountIdB64)
            return
        }
        guard let blob = try? JSONEncoder().encode(links) else { return }
        backend.save(accountIdB64: accountIdB64, blob: blob)
    }
}

/// Revoking the links this device keeps a record of (ADR-0078 section 10).
public enum OwnLinks {
    /// Revoke every live link this device keeps a record of, each with its
    /// own revocation seed, and drop the records. Account deletion runs this
    /// first, while the session still works: the directory cannot delete an
    /// account's links any more. Best effort; returns how many could not be
    /// reached (their records are kept).
    @discardableResult
    public static func revokeAll(
        accountIdB64: String,
        discovery: DiscoveryClient,
        store: OwnLinkStore = .shared
    ) async -> Int {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        var unreachable = 0
        for link in store.all(accountIdB64: accountIdB64) {
            guard !link.isExpired(nowMs: nowMs), let blobId = link.blobId, let seed = link.seed else {
                store.remove(accountIdB64: accountIdB64, kind: link.kind, slug: link.slug)
                continue
            }
            do {
                switch link.kind {
                case .contact: _ = try await discovery.revokeInvite(blobId: blobId, signingSeed: seed)
                case .call: _ = try await discovery.revokeCallInvite(blobId: blobId, signingSeed: seed)
                case .group: _ = try await discovery.revokeGroupInvite(blobId: blobId, signingSeed: seed)
                }
                store.remove(accountIdB64: accountIdB64, kind: link.kind, slug: link.slug)
            } catch {
                unreachable += 1
            }
        }
        return unreachable
    }
}
