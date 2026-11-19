// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 sections 5 and 18, reactions-and-emoji spec section 9 — the
/// group photo, the reactions a group takes, and the group's knock key.
///
/// Every setting follows the ADR-0051 group-name pattern: sent through the
/// locked application path, written to the store only after the send, gated
/// on receipt against the LIVE role table (owner or admin, resolved at
/// receipt, C-68) under the per-group name chain (C-62 sub-invariant 5),
/// ordered by a strictly newer stamp, and re-sent on growth with the STORED
/// stamp verbatim.
@MainActor
extension ChatRepository {

    /// The knock version of a group with no recorded one (a new group).
    nonisolated static var currentKnockVersion: UInt16 { 0 }

    /// ADR-0087 section 5 — the owner's knock reconcile: removing an admin
    /// bumps the group's knock version; the seed of the current version goes
    /// to every remaining admin once (sealed to its account key); new links
    /// carry the current version, and older versions stay derivable so
    /// requests sealed to them still open. Best-effort, run with the
    /// registrar reconcile.
    func reconcileKnockSeeds(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex],
              ownsGroup(groupIdHex: groupIdHex), let accountIdentity,
              let roles = (try? record.group.groupRoles()) ?? nil else { return }
        let admins = roles.admins
        var row = KnockVersionStore.load(accountIdB64, groupIdHex)
            ?? KnockVersionStore.Row(version: Self.currentKnockVersion, admins: admins, sent: [])
        let raised = ownerKnockVersion(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        if raised > row.version {
            row.version = raised
            row.sent = row.sent.filter { $0.hasPrefix("own|") }
        }
        let next = KnockVersionStore.reconciled(row, currentAdmins: admins)
        if next != row {
            let bumped = next.version != row.version
            row = next
            KnockVersionStore.save(row, accountIdB64, groupIdHex)
            // A public group's record carries the knock key: its republish is
            // owed from the moment of the bump, durably (HeldHandleStore).
            if bumped, groupHandle(groupIdHex: groupIdHex)?.isPublic == true {
                HeldHandleStore.markOwed(journalIdHex: AttachmentBridge.hex(record.groupId), accountIdB64)
                await flushOwedHandleWrites()
            }
        }
        let groupId = record.groupId
        for admin in admins where !row.sent.contains(KnockVersionStore.sentTag(admin, row.version)) {
            guard let x25519 = await memberX25519PublicKey(accountKey: admin) else { continue }
            do {
                guard var seed = knockSeedForOwner(
                    groupIdHex: groupIdHex, groupId: groupId, version: row.version) else { continue }
                defer { RootHandover.wipe(&seed) }
                let envelope = try composeKnockSeedEnvelope(
                    journalId: groupId, memberAccountKey: admin, memberX25519PublicKey: x25519,
                    knockVersion: row.version, knockSeed: seed)
                try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
                row.sent.append(KnockVersionStore.sentTag(admin, row.version))
                KnockVersionStore.save(row, accountIdB64, groupIdHex)
            } catch {
                if !ConversationReentryRules.isBehindRefusal(error) { NSLog("[knock] WARN a knock seed was not delivered; retried on the next change") }
            }
        }
        // The owner's other devices learn every version, 0 through the current
        // one, addressed to the account itself: links and records minted on
        // any of them name the current version, and older rows open there.
        guard let identity = conversationAccountIdentity else { return }
        let ownKey = identity.signaturePublicKey()
        let ownX25519 = identity.x25519PublicKey()
        for version in 0...row.version {
            let tag = "own|\(version)"
            guard !row.sent.contains(tag) else { continue }
            do {
                guard var seed = knockSeedForOwner(groupIdHex: groupIdHex, groupId: groupId, version: version)
                else { continue }
                defer { RootHandover.wipe(&seed) }
                let envelope = try composeKnockSeedEnvelope(
                    journalId: groupId, memberAccountKey: ownKey, memberX25519PublicKey: ownX25519,
                    knockVersion: version, knockSeed: seed)
                try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
                row.sent.append(tag)
                KnockVersionStore.save(row, accountIdB64, groupIdHex)
            } catch {
                if !ConversationReentryRules.isBehindRefusal(error) { NSLog("[knock] WARN a knock seed for this account's devices was not sent; retried later") }
            }
        }
        // ADR-0091 phase 2 route 3 — the knock key goes to every member, once
        // per version.
        await publishKnockKeyIfOwed(groupIdHex: groupIdHex)
    }

    /// The owner's knock seed of `version`: the one another device of this
    /// account handed over when held (it may sit on a root this device has
    /// not derived), else derived from the right root. The caller wipes.
    func knockSeedForOwner(groupIdHex: String, groupId: Data, version: UInt16) -> Data? {
        if let accountIdB64 = currentAccountIdB64,
           let given = groupSettingsStore.knockSeeds(accountIdB64: accountIdB64, groupIdHex: groupIdHex)[version] {
            return given
        }
        guard var root = knockRootSeed(groupIdHex: groupIdHex, groupId: groupId, version: version) else { return nil }
        defer { RootHandover.wipe(&root) }
        return try? groupKnockSeed(journalRootSeed: root, version: version)
    }

    /// The current knock version of an owned group: the recorded one, raised
    /// by any newer seed another device of this account handed over.
    func ownerKnockVersion(accountIdB64: String, groupIdHex: String) -> UInt16 {
        let recorded = KnockVersionStore.load(accountIdB64, groupIdHex)?.version ?? Self.currentKnockVersion
        let given = groupSettingsStore.knockSeeds(accountIdB64: accountIdB64, groupIdHex: groupIdHex).keys.max()
        return max(recorded, given ?? 0)
    }

    // MARK: - Allowed reactions

    /// The reactions `groupIdHex` takes. A one-to-one always takes every
    /// reaction (section 9).
    public func allowedReactions(groupIdHex: String) -> FfiAllowedReactions {
        guard !isOneToOne(groupIdHex: groupIdHex), let accountIdB64 = currentAccountIdB64,
              let stored = groupSettingsStore.reactions(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        else { return .all }
        return stored.allowed
    }

    /// Section 9 — the owner or an admin saves the group's reactions: compose
    /// with a fresh stamp, send, and only then store. Throws what the send or
    /// the codec threw; the screen keeps itself open with the error line.
    public func setAllowedReactions(groupIdHex: String, _ value: FfiAllowedReactions) async throws {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let stamp = UInt64(max(0, Self.nowMs()))
        let bytes = try composeGroupReactionsEnvelope(
            value: FfiGroupReactions(reactions: value, updatedAtMs: stamp))
        let bound = binding.capture()
        try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
        try await groupNameLock.run(groupIdHex: groupIdHex) { [self] in
            groupSettingsStore.setReactions(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                StoredGroupReactions(value, updatedAtMs: stamp))
            guard bound.isCurrent else { return }
            groupReactionsPublisher.notePublished(groupIdHex: groupIdHex)
            groupSettingsRevision &+= 1
        }
    }

    /// The growth re-send: the STORED value with its STORED stamp, from an
    /// owner or admin only (a member's re-send would be refused).
    func groupReactionsEnvelopeForResend(groupIdHex: String) -> Data? {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil,
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              ownRole(groupIdHex: groupIdHex) != .member,
              let stored = groupSettingsStore.reactions(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        else { return nil }
        return try? composeGroupReactionsEnvelope(
            value: FfiGroupReactions(reactions: stored.allowed, updatedAtMs: stored.updatedAtMs))
    }

    // MARK: - Group photo

    /// The photo this device holds for `groupIdHex`, or `nil` (initials).
    public func groupPhoto(groupIdHex: String) -> ProfileAvatar? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return groupSettingsStore.photo(accountIdB64: accountIdB64, groupIdHex: groupIdHex)?.avatar
    }

    /// Push the photos this device holds into the avatar component's
    /// directory (``GroupPhotoDirectory``). An owed photo shows at once on
    /// this device (section 4.5.2, "Uploaded, the device went offline").
    public func refreshGroupPhotos() {
        guard let accountIdB64 = currentAccountIdB64 else {
            GroupPhotoDirectory.shared.replace([:])
            return
        }
        var map: [String: ProfileAvatar] = [:]
        for groupIdHex in groups.keys {
            let owed = groupSettingsStore.owedPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            let photo = owed?.photo ?? groupSettingsStore.photo(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            if let avatar = photo?.avatar { map[groupIdHex] = avatar }
        }
        GroupPhotoDirectory.shared.replace(map)
    }

    /// Whether a photo set on this device is uploaded and waiting to be sent
    /// (section 4.5.2, "Will apply when you're back online").
    public func groupPhotoOwed(groupIdHex: String) -> Bool {
        guard let accountIdB64 = currentAccountIdB64 else { return false }
        return groupSettingsStore.owedPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex) != nil
    }

    /// Section 4.5.2 — set (or, with `nil`, remove) the group photo. The
    /// caller has uploaded the files. The new photo is recorded as owed BEFORE
    /// the send, so a send that fails for the network is retried by
    /// ``sendOwedGroupPhotos()``, also after a restart; a replaced photo's
    /// files are deleted by this device (``BlobLifecycle``, owed and retried).
    public func setGroupPhoto(groupIdHex: String, avatar: ProfileAvatar?) async throws {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let photo = StoredGroupPhoto(avatar: avatar, updatedAtMs: UInt64(max(0, Self.nowMs())))
        // Refused here, before anything is recorded, if the core would refuse it.
        _ = try composeGroupPhotoEnvelope(photo: photo.ffi)
        groupSettingsStore.setOwedPhoto(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, OwedGroupPhoto(photo: photo))
        groupSettingsRevision &+= 1
        try await sendGroupPhoto(groupIdHex: groupIdHex, photo: photo)
    }

    /// Send one photo envelope; on success store it, clear the owed record
    /// and delete the files of the photo it replaced.
    private func sendGroupPhoto(groupIdHex: String, photo: StoredGroupPhoto) async throws {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let bytes = try composeGroupPhotoEnvelope(photo: photo.ffi)
        let bound = binding.capture()
        try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
        try await groupNameLock.run(groupIdHex: groupIdHex) { [self] in
            let previous = groupSettingsStore.photo(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            groupSettingsStore.setPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex, photo)
            groupSettingsStore.setOwedPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex, nil)
            guard bound.isCurrent else { return }
            notePublicRecordChanged(groupIdHex: groupIdHex)
            // ADR-0087 section 18.5 — a replaced photo is deleted by the
            // devices of whoever set it. Only files this device uploaded are
            // deletable at all (the delete key is the uploader's), and the
            // storage answers anything else with a refusal the lifecycle drops.
            let replaced = ProfileAvatarUpload.replacedBlobs(
                previous: previous?.avatar, current: photo.avatar)
            if !replaced.isEmpty { BlobLifecycle.shared.delete(replaced) }
            groupPhotoPublisher.notePublished(groupIdHex: groupIdHex)
            groupSettingsRevision &+= 1
        }
    }

    /// Send every owed photo (on landing and when the network is back).
    /// Best-effort; a failure stays owed. A photo this device may no longer
    /// set (it was demoted) is dropped: every peer would refuse it.
    public func sendOwedGroupPhotos() async {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        for groupIdHex in groupSettingsStore.groupsWithOwedPhoto(accountIdB64: accountIdB64) {
            guard let owed = groupSettingsStore.owedPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            else { continue }
            guard groups[groupIdHex] != nil, ownRole(groupIdHex: groupIdHex) != .member else {
                groupSettingsStore.setOwedPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex, nil)
                continue
            }
            do {
                try await sendGroupPhoto(groupIdHex: groupIdHex, photo: owed.photo)
            } catch {
                NSLog("[group-photo] WARN an owed group photo was not sent; retried later")
            }
        }
    }

    /// The growth re-send: the STORED photo with its STORED stamp, from an
    /// owner or admin only.
    func groupPhotoEnvelopeForResend(groupIdHex: String) -> Data? {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil,
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              ownRole(groupIdHex: groupIdHex) != .member,
              let stored = groupSettingsStore.photo(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        else { return nil }
        return try? composeGroupPhotoEnvelope(photo: stored.ffi)
    }

    /// ADR-0087 section 18.5 — the files of the photos of the groups this
    /// device is owner or admin of: the `conversation-photos` keep source.
    func keptConversationPhotoBlobIds() -> [Data] {
        guard let accountIdB64 = currentAccountIdB64 else { return [] }
        var ids: [Data] = []
        for groupIdHex in groups.keys where ownRole(groupIdHex: groupIdHex) == .owner
            || ownRole(groupIdHex: groupIdHex) == .admin {
            guard let avatar = groupSettingsStore.photo(accountIdB64: accountIdB64, groupIdHex: groupIdHex)?
                .avatar else { continue }
            ids.append(avatar.still.blobId)
            if let video = avatar.video { ids.append(video.blobId) }
        }
        return ids
    }

    // MARK: - @name (ADR-0087 sections 2, 3, 11)

    /// ADR-0087 section 10 — the people (accounts, not devices) in a group.
    public func peopleCount(groupIdHex: String) -> Int {
        guard let members = try? groups[groupIdHex]?.group.members() else { return 0 }
        return PersonKey.people(members).count
    }

    /// ADR-0087 section 4 — what a public group's record says now, for a
    /// renewal or an owed republish; `nil` when this device cannot say it.
    func publicGroupFacts(journalIdHex: String) -> ConversationHandles.PublicFacts? {
        let groupIdHex = journalIdHex.lowercased()
        guard let record = groups[groupIdHex], let name = groupName(groupIdHex: groupIdHex),
              let knock = knockKey(groupIdHex: groupIdHex) else { return nil }
        return ConversationHandles.PublicFacts(
            kind: .group, id: record.groupId, key: knock.publicKey, knockVersion: knock.version, name: name,
            description: "", size: peopleCount(groupIdHex: groupIdHex), avatar: groupPhoto(groupIdHex: groupIdHex)?.ffi)
    }

    /// One answer to "does this account own the group?" for the knock key,
    /// links and handle writes: the live role table, whose owner the roster
    /// resolves through this device's leaf binding (so the conversation key of
    /// an ADR-0074 rotation and the account key both count).
    func ownsGroup(groupIdHex: String) -> Bool {
        ownRole(groupIdHex: groupIdHex) == .owner
    }

    /// The group's `@name` as this device holds it (verified), or `nil`.
    public func groupHandle(groupIdHex: String) -> StoredGroupHandle? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return groupSettingsStore.handle(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// The facts a public group's record carries, for a group not yet
    /// registered on this device (claimed before founding).
    func newGroupFacts(groupId: Data, name: String, avatar: ProfileAvatar?) -> ConversationHandles.PublicFacts? {
        guard let accountIdentity,
              var root = try? dsJournalRootSeed(accountIdentitySeed: accountIdentity.ed25519Seed(), journalId: groupId)
        else { return nil }
        defer { RootHandover.wipe(&root) }
        guard var seed = try? groupKnockSeed(journalRootSeed: root, version: Self.currentKnockVersion) else { return nil }
        defer { RootHandover.wipe(&seed) }
        guard let knock = try? groupKnockPublicKey(knockSeed: seed) else { return nil }
        return ConversationHandles.PublicFacts(
            kind: .group, id: groupId, key: knock, knockVersion: Self.currentKnockVersion, name: name,
            description: "", size: 1, avatar: avatar?.ffi)
    }

    /// Section 3.1 — claim `handle` for a group being founded, BEFORE
    /// anything about it leaves the device: a held name throws and nothing is
    /// created. Returns the signed in-group value to adopt once registered.
    ///
    /// `earlier`: the group ids of earlier New group attempts at this name,
    /// newest first (``PendingGroupHandleStore``); a retry moves the name from
    /// one of them to this group instead of claiming it.
    func claimHandleBeforeFounding(
        groupId: Data, handle: String, isPublic: Bool, name: String, avatar: ProfileAvatar?,
        earlier: [Data] = []
    ) async throws -> (value: FfiConversationHandle, leaseUntilMs: UInt64) {
        guard let accountIdentity else { throw ChatError.argument("no account identity") }
        let facts = isPublic ? newGroupFacts(groupId: groupId, name: name, avatar: avatar) : nil
        if isPublic && facts == nil { throw ChatError.argument("a public group needs a name") }
        let accountSeed = accountIdentity.ed25519Seed()
        let discovery = DiscoveryClient()
        let written = try await ConversationHandles.claimOrTakeBack(
            earlier: earlier,
            claim: {
                try await ConversationHandles.claim(
                    discovery: discovery, accountSeed: accountSeed, journalId: groupId,
                    handle: handle, isPublic: isPublic, facts: facts)
            },
            rekeyFrom: { earlierId in
                try await ConversationHandles.move(
                    discovery: discovery, accountSeed: accountSeed, fromJournalId: earlierId,
                    toJournalId: groupId, handle: handle)
            },
            update: {
                try await ConversationHandles.update(
                    discovery: discovery, accountSeed: accountSeed, journalId: groupId,
                    handle: handle, isPublic: isPublic, facts: facts)
            })
        var seed = try ConversationHandles.seed(accountSeed: accountSeed, journalId: groupId)
        defer { RootHandover.wipe(&seed) }
        let value = try signConversationHandle(
            handleSeed: seed, groupId: groupId, updatedAtMs: UInt64(max(0, Self.nowMs())),
            public: isPublic, handle: written.handle, certificate: written.certificate)
        return (value, written.leaseUntilMs)
    }

    /// Store a value this owner just signed and send it into the group.
    func adoptOwnHandle(groupIdHex: String, value: FfiConversationHandle, leaseUntilMs: UInt64, name: String) async {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        groupSettingsStore.setHandle(accountIdB64: accountIdB64, groupIdHex: groupIdHex, StoredGroupHandle(value))
        HeldHandleStore.put(HeldHandleStore.Entry(
            kind: "group", handle: value.handle, isPublic: value.public,
            journalIdHex: AttachmentBridge.hex(record.groupId), channelIdHex: nil,
            name: name, description: "", leaseUntilMs: leaseUntilMs), accountIdB64)
        groupSettingsRevision &+= 1
        if let bytes = try? composeConversationHandleEnvelope(value: value) {
            try? await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
            groupHandlePublisher.notePublished(groupIdHex: groupIdHex)
        }
    }

    /// Section 4.4 — claim a name for a group whose name lapsed (or never
    /// landed), from the Group type screen.
    public func claimGroupHandle(groupIdHex: String, handle: String, isPublic: Bool) async throws {
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        var facts: ConversationHandles.PublicFacts?
        if isPublic {
            facts = publicGroupFacts(journalIdHex: groupIdHex)
            if facts == nil { throw ChatError.argument("a public group needs a name") }
        }
        guard let accountIdentity else { throw ChatError.argument("no account identity") }
        let accountSeed = accountIdentity.ed25519Seed()
        let written = try await ConversationHandles.claim(
            discovery: DiscoveryClient(), accountSeed: accountSeed, journalId: record.groupId,
            handle: handle, isPublic: isPublic, facts: facts)
        var seed = try ConversationHandles.seed(accountSeed: accountSeed, journalId: record.groupId)
        defer { RootHandover.wipe(&seed) }
        let value = try signConversationHandle(
            handleSeed: seed, groupId: record.groupId, updatedAtMs: UInt64(max(0, Self.nowMs())),
            public: isPublic, handle: written.handle, certificate: written.certificate)
        await adoptOwnHandle(
            groupIdHex: groupIdHex, value: value, leaseUntilMs: written.leaseUntilMs,
            name: groupName(groupIdHex: groupIdHex) ?? "")
    }

    // MARK: - Group type (UX section 4.1, ADR-0087 section 8)

    /// The outcome of a Group type save.
    public enum GroupTypeOutcome: Equatable {
        case applied
        /// Recorded as owed and retried until it lands.
        case owed
    }

    /// Section 8 — change the flag and / or the name. The change is recorded
    /// as owed BEFORE the first step (durably), then run: discovery (update or
    /// replace), the in-group value, and going private re-keys the photo. A
    /// network failure leaves it owed; a refusal throws.
    public func saveGroupType(groupIdHex: String, handle: String, isPublic: Bool) async throws -> GroupTypeOutcome {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex],
              let current = groupHandle(groupIdHex: groupIdHex) else {
            try await claimGroupHandle(groupIdHex: groupIdHex, handle: handle, isPublic: isPublic)
            return .applied
        }
        let journalHex = AttachmentBridge.hex(record.groupId)
        let wantedName = ConversationHandles.canonical(handle)
        if wantedName != current.handle {
            // A rename: replace, then the value (not owed: a held name answers now).
            guard let accountIdentity else { throw ChatError.argument("no account identity") }
            let facts = isPublic ? publicGroupFacts(journalIdHex: groupIdHex) : nil
            let accountSeed = accountIdentity.ed25519Seed()
            let written = try await ConversationHandles.replace(
                discovery: DiscoveryClient(), accountSeed: accountSeed, journalId: record.groupId,
                oldHandle: current.handle, newHandle: wantedName, isPublic: isPublic, facts: facts)
            var seed = try ConversationHandles.seed(accountSeed: accountSeed, journalId: record.groupId)
            defer { RootHandover.wipe(&seed) }
            let value = try signConversationHandle(
                handleSeed: seed, groupId: record.groupId, updatedAtMs: UInt64(max(0, Self.nowMs())),
                public: isPublic, handle: written.handle, certificate: written.certificate)
            if current.isPublic && !isPublic { groupSettingsStore.setPhotoRekeyOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex, true) }
            await adoptOwnHandle(
                groupIdHex: groupIdHex, value: value, leaseUntilMs: written.leaseUntilMs,
                name: groupName(groupIdHex: groupIdHex) ?? "")
            await rekeyGroupPhotoIfOwed(groupIdHex: groupIdHex)
            return .applied
        }
        guard current.isPublic != isPublic else { return .applied }
        // The flag: owed first, durably.
        var entry = HeldHandleStore.load(accountIdB64).first { $0.journalIdHex == journalHex }
            ?? HeldHandleStore.Entry(
                kind: "group", handle: current.handle, isPublic: isPublic, journalIdHex: journalHex,
                channelIdHex: nil, name: groupName(groupIdHex: groupIdHex) ?? "", description: "", leaseUntilMs: 0)
        entry = HeldHandleStore.Entry(
            kind: "group", handle: entry.handle, isPublic: isPublic, journalIdHex: journalHex,
            channelIdHex: nil, name: groupName(groupIdHex: groupIdHex) ?? entry.name, description: "",
            leaseUntilMs: entry.leaseUntilMs, owed: true, rekeyFromB64: entry.rekeyFromB64)
        HeldHandleStore.put(entry, accountIdB64)
        if !isPublic { groupSettingsStore.setPhotoRekeyOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex, true) }
        await flushOwedHandleWrites()
        let landed = HeldHandleStore.load(accountIdB64).first { $0.journalIdHex == journalHex }?.owed != true
        return landed ? .applied : .owed
    }

    /// Whether a flag change of this group is still owed.
    public func groupTypeOwed(groupIdHex: String) -> Bool {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return false }
        let hex = AttachmentBridge.hex(record.groupId)
        return HeldHandleStore.load(accountIdB64).first { $0.journalIdHex == hex }?.owed == true
    }

    /// Run every owed group-name write: a rekey after an ADR-0074 rotation,
    /// a flag change, a republish after a knock-version bump; then any owed
    /// photo re-key. Best-effort; what fails stays owed.
    /// `group-and-channel-privacy.md` 4.5.4 — the public record carries the
    /// name and the photo, so a change to either, whoever made it, owes a
    /// republish on the device that holds the group's handle key (the
    /// owner's: ``HeldHandleStore`` has an entry for the group only there).
    /// Durable: the owed flag is stored before the write and cleared only by
    /// a write that landed; a failure is retried at the next flush (the
    /// session start and every later change).
    func notePublicRecordChanged(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64,
              groupHandle(groupIdHex: groupIdHex)?.isPublic == true,
              HeldHandleStore.load(accountIdB64).contains(where: { $0.journalIdHex == groupIdHex }) else { return }
        HeldHandleStore.markOwed(journalIdHex: groupIdHex, accountIdB64)
        Task { @MainActor [weak self] in await self?.flushOwedHandleWrites() }
    }

    public func flushOwedHandleWrites() async {
        guard let accountIdB64 = currentAccountIdB64, let accountIdentity else { return }
        let discovery = DiscoveryClient()
        for entry in HeldHandleStore.load(accountIdB64) {
            guard let journal = Self.hexToBytes(entry.journalIdHex) else { continue }
            if let fromB64 = entry.rekeyFromB64 {
                guard let oldSeed = KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64)
                    .compactMap({ try? Identity.fromSeed(seed: $0) })
                    .first(where: { $0.signaturePublicKey().base64EncodedString() == fromB64 })?.ed25519Seed()
                else { continue }
                do {
                    let written = try await ConversationHandles.rekey(
                        discovery: discovery, oldAccountSeed: oldSeed,
                        newAccountSeed: accountIdentity.ed25519Seed(), journalId: journal, handle: entry.handle)
                    var next = entry
                    next.rekeyFromB64 = nil
                    next.leaseUntilMs = max(entry.leaseUntilMs, written.leaseUntilMs)
                    HeldHandleStore.put(next, accountIdB64)
                    if entry.kind == "group", let stored = groupHandle(groupIdHex: entry.journalIdHex) {
                        var seed = try ConversationHandles.seed(accountSeed: accountIdentity.ed25519Seed(), journalId: journal)
                        defer { RootHandover.wipe(&seed) }
                        let value = try signConversationHandle(
                            handleSeed: seed, groupId: journal, updatedAtMs: UInt64(max(0, Self.nowMs())),
                            public: stored.isPublic, handle: stored.handle, certificate: written.certificate)
                        await adoptOwnHandle(
                            groupIdHex: entry.journalIdHex, value: value, leaseUntilMs: next.leaseUntilMs,
                            name: entry.name)
                    }
                } catch {
                    NSLog("[handle] WARN an owed name rekey failed; retried later")
                    continue
                }
            }
            guard entry.kind == "group", entry.owed == true, groups[entry.journalIdHex] != nil else { continue }
            let facts = entry.isPublic ? publicGroupFacts(journalIdHex: entry.journalIdHex) : nil
            if entry.isPublic && facts == nil { continue }
            do {
                let written = try await ConversationHandles.update(
                    discovery: discovery, accountSeed: accountIdentity.ed25519Seed(), journalId: journal,
                    handle: entry.handle, isPublic: entry.isPublic, facts: facts)
                var seed = try ConversationHandles.seed(accountSeed: accountIdentity.ed25519Seed(), journalId: journal)
                defer { RootHandover.wipe(&seed) }
                let value = try signConversationHandle(
                    handleSeed: seed, groupId: journal, updatedAtMs: UInt64(max(0, Self.nowMs())),
                    public: entry.isPublic, handle: written.handle, certificate: written.certificate)
                var next = HeldHandleStore.load(accountIdB64).first { $0.journalIdHex == entry.journalIdHex } ?? entry
                next.owed = nil
                next.leaseUntilMs = written.leaseUntilMs
                HeldHandleStore.put(next, accountIdB64)
                await adoptOwnHandle(
                    groupIdHex: entry.journalIdHex, value: value, leaseUntilMs: written.leaseUntilMs, name: entry.name)
            } catch {
                NSLog("[handle] WARN an owed name write failed; retried later")
            }
        }
        for groupIdHex in groups.keys { await rekeyGroupPhotoIfOwed(groupIdHex: groupIdHex) }
    }

    /// ADR-0087 section 18.3 — going private re-keys the photo: the current
    /// photo is uploaded again under a fresh key and sent as an ordinary
    /// change, so the key a public record published opens nothing current.
    func rekeyGroupPhotoIfOwed(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64,
              groupSettingsStore.photoRekeyOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex) else { return }
        guard let avatar = groupPhoto(groupIdHex: groupIdHex) else {
            groupSettingsStore.setPhotoRekeyOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex, false)
            return
        }
        do {
            let still = try await AttachmentBridge.downloadAndDecrypt(ref: avatar.still.attachmentRef)
            let upStill = try await AttachmentBridge.encryptAndUpload(bytes: still, mime: nil, name: nil)
            var video: AvatarBlobRef?
            if let ref = avatar.video {
                let bytes = try await AttachmentBridge.downloadAndDecrypt(ref: ref.attachmentRef)
                let up = try await AttachmentBridge.encryptAndUpload(bytes: bytes, mime: nil, name: nil)
                video = AvatarBlobRef(blobId: up.blobId, key: up.key, nonce: up.nonce, size: ref.size, side: ref.side)
            }
            let fresh = ProfileAvatar(
                still: AvatarBlobRef(
                    blobId: upStill.blobId, key: upStill.key, nonce: upStill.nonce,
                    size: avatar.still.size, side: avatar.still.side),
                video: video, videoDurationMs: avatar.videoDurationMs)
            try await setGroupPhoto(groupIdHex: groupIdHex, avatar: fresh)
            groupSettingsStore.setPhotoRekeyOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex, false)
        } catch AttachmentBridgeError.missing {
            // The photo is gone: nothing current to protect.
            groupSettingsStore.setPhotoRekeyOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex, false)
        } catch {
            NSLog("[group-photo] WARN the photo re-key is owed; retried later")
        }
    }

    /// Section 11 — a member received a signed `@name`: shown only once its
    /// certificate verifies against discovery's key for the lookup key of the
    /// shown name, and the handle key signed this group's value. Any member
    /// may relay it; strictly newer wins.
    func receiveGroupHandle(groupIdHex: String, value: FfiConversationHandle) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        if let stored = groupSettingsStore.handle(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
           !conversationHandleIsNewer(incomingMs: value.updatedAtMs, storedMs: stored.updatedAtMs) { return }
        let groupId = record.groupId
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self, let pair = try? await DiscoveryClient().oprfName(value.handle),
                  let writeKey = await ProfileDirectory.shared.handleWriteKey(
                      certificate: value.certificate, lookupKey: pair.lookupKey),
                  (try? verifyConversationHandle(value: value, groupId: groupId, handleKey: writeKey)) != nil,
                  bound.isCurrent else {
                NSLog("[group-handle] an @name did not verify; not shown")
                return
            }
            try? await self.groupNameLock.run(groupIdHex: groupIdHex) { [weak self] in
                guard let self, bound.isCurrent else { return }
                if let stored = self.groupSettingsStore.handle(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
                   !conversationHandleIsNewer(incomingMs: value.updatedAtMs, storedMs: stored.updatedAtMs) { return }
                self.groupSettingsStore.setHandle(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, StoredGroupHandle(value))
                self.groupSettingsRevision &+= 1
            }
        }
    }

    /// The growth re-send of the `@name`: any member may relay the stored
    /// value verbatim.
    func groupHandleEnvelopeForResend(groupIdHex: String) -> Data? {
        guard let stored = groupHandle(groupIdHex: groupIdHex),
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) else { return nil }
        return try? composeConversationHandleEnvelope(value: stored.ffi)
    }

    // MARK: - Request to join a public group (UX section 6.1)

    public enum PublicJoinOutcome: Equatable {
        case sent, queueFull, outOfRequests, offline, failed
    }

    /// Section 6.1 — a sealed KeyPackage to the group's knock key, carrying
    /// the reply grant and this person's name and photo; no capability.
    public func requestToJoinPublicGroup(groupId: Data, knockKey: Data, knockVersion: UInt16) async -> PublicJoinOutcome {
        guard let identity = canonicalIdentity, let accountIdB64 = currentAccountIdB64 else { return .failed }
        let bound = binding.capture()
        do {
            let replyGrant = try await ReachCenter.shared.newReplyGrant(purpose: ReplyPurpose.groupJoin(groupId))
            try bound.require()
            let keyPackage = try identity.createKeyPackage()
            try persistJoinRequestOrThrow(identity: identity, accountIdB64: accountIdB64)
            let row = try sealKnockRow(
                knockPublicKey: knockKey, version: knockVersion, keyPackage: keyPackage,
                replyGrant: replyGrant, profile: ProfileDirectory.shared.ownDeviceEnvelope() ?? Data(),
                capability: nil)
            _ = try await dsClient.knock(groupId: groupId, payloads: [row])
            persistSnapshotBestEffort()
            return .sent
        } catch {
            if AnonTokenUnavailable.isQuotaExhausted(error) { return .outOfRequests }
            if case DsClientError.http(let status, _, let body)? = error as? DsClientError, status == 429 {
                return body.contains("knock-queue-full") ? .queueFull : .outOfRequests
            }
            if case .network? = error as? DsClientError { return .offline }
            NSLog("[knock] a request to join from search failed: %@", String(describing: error))
            return .failed
        }
    }

    // MARK: - Join requests backlog (UX section 8)

    /// `Reject all` — drop every retained request, then delete the knock rows
    /// up to the last one this device read, so a request that arrived after
    /// stays (ADR-0087 section 5).
    public func rejectAllExternalJoins(groupIdHex: String) async throws {
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        for ref in pendingExternalJoins(groupIdHex: groupIdHex) {
            try rejectExternalJoin(groupIdHex: groupIdHex, proposalRef: ref)
        }
        let through = record.knockCursor
        guard through > 0 else { return }
        try await dsClient.deleteKnocks(
            groupId: record.groupId, through: through, auth: dsAuth(groupIdHex: groupIdHex))
    }

    // MARK: - Unseen reactions (reactions-and-emoji spec section 12)

    func noteUnseenReaction(groupIdHex: String, messageId: Int64) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        if unseenReactionsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId) {
            unseenReactionsRevision &+= 1
        }
    }

    /// The own messages of `groupIdHex` with reactions not yet on screen,
    /// oldest first.
    public func unseenReactionIds(groupIdHex: String) -> [Int64] {
        guard let accountIdB64 = currentAccountIdB64 else { return [] }
        return unseenReactionsStore.ids(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// The message was on screen: its reactions are seen.
    public func markReactionsSeen(groupIdHex: String, messageId: Int64) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        if unseenReactionsStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId) {
            unseenReactionsRevision &+= 1
        }
    }

    // MARK: - Knock key (ADR-0087 section 5)

    /// The journal root seed that derives knock version `version` of an
    /// owned group: the predecessor's root for a version from before an
    /// ADR-0074 rotation, the current account's otherwise. The caller wipes.
    func knockRootSeed(groupIdHex: String, groupId: Data, version: UInt16) -> Data? {
        guard let accountIdB64 = currentAccountIdB64, let accountIdentity else { return nil }
        let row = KnockVersionStore.load(accountIdB64, groupIdHex)
        if let predecessorB64 = KnockVersionStore.predecessorKey(for: version, row: row) {
            for seed in KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64) {
                guard let identity = try? Identity.fromSeed(seed: seed),
                      identity.signaturePublicKey().base64EncodedString() == predecessorB64 else { continue }
                return try? dsJournalRootSeed(accountIdentitySeed: identity.ed25519Seed(), journalId: groupId)
            }
            return nil
        }
        return try? dsJournalRootSeed(accountIdentitySeed: accountIdentity.ed25519Seed(), journalId: groupId)
    }

    /// ADR-0087 section 5 rotation rule — an owner's ADR-0074 rotation moved
    /// the journal root: record the boundary and bump the knock version of
    /// every owned group (durably, before anything else), then hand the new
    /// seed to admins and republish a public group's record.
    func bumpKnockVersionsAfterRotation(predecessorSeed: Data) {
        guard let accountIdB64 = currentAccountIdB64,
              let predecessor = try? Identity.fromSeed(seed: predecessorSeed) else { return }
        let predecessorB64 = predecessor.signaturePublicKey().base64EncodedString()
        var owned: [String] = []
        for groupIdHex in groups.keys where ownsGroup(groupIdHex: groupIdHex) {
            let row = KnockVersionStore.load(accountIdB64, groupIdHex)
                ?? KnockVersionStore.Row(
                    version: Self.currentKnockVersion,
                    admins: ((try? groups[groupIdHex]?.group.groupRoles()) ?? nil)?.admins ?? [], sent: [])
            KnockVersionStore.save(
                KnockVersionStore.rotated(row, predecessorKeyB64: predecessorB64), accountIdB64, groupIdHex)
            if groupHandle(groupIdHex: groupIdHex)?.isPublic == true, let record = groups[groupIdHex] {
                HeldHandleStore.markOwed(journalIdHex: AttachmentBridge.hex(record.groupId), accountIdB64)
            }
            owned.append(groupIdHex)
        }
        // The handle key derives from the journal root too: every held name
        // owes a `rekey` signed by the predecessor's key (durable).
        HeldHandleStore.markRekeyOwed(fromB64: predecessorB64, accountIdB64)
        guard !owned.isEmpty else {
            Task { @MainActor [weak self] in await self?.flushOwedHandleWrites() }
            return
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            for groupIdHex in owned { await self.reconcileKnockSeeds(groupIdHex: groupIdHex) }
            await self.flushOwedHandleWrites()
        }
    }

    /// The knock key a new link of `groupIdHex` carries: the owner derives it
    /// from the journal root; an admin uses the newest seed the owner handed
    /// it. `nil` when this device holds neither (it cannot mint a link then).
    func knockKey(groupIdHex: String) -> (version: UInt16, publicKey: Data)? {
        guard let record = groups[groupIdHex], let accountIdB64 = currentAccountIdB64 else { return nil }
        if ownsGroup(groupIdHex: groupIdHex) {
            let version = ownerKnockVersion(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            guard var seed = knockSeedForOwner(groupIdHex: groupIdHex, groupId: record.groupId, version: version)
            else { return nil }
            defer { RootHandover.wipe(&seed) }
            guard let pk = try? groupKnockPublicKey(knockSeed: seed) else { return nil }
            return (version, pk)
        }
        let held = groupSettingsStore.knockSeeds(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard let (version, seed) = held.max(by: { $0.key < $1.key }),
              let pk = try? groupKnockPublicKey(knockSeed: seed) else { return nil }
        return (version, pk)
    }

    // MARK: - Receive

    /// A group-setting envelope arrived on the group journal. Fire-and-forget:
    /// the whole read-decide-write runs under the per-group name chain.
    func receiveGroupSetting(groupIdHex: String, setting: GroupSettingEnvelope, senderLeaf: UInt32?) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self else { return }
            try? await self.groupNameLock.run(groupIdHex: groupIdHex) { [weak self] in
                guard bound.isCurrent else { return }
                self?.performReceiveGroupSetting(
                    groupIdHex: groupIdHex, setting: setting, senderLeaf: senderLeaf)
            }
        }
    }

    /// The locked body of ``receiveGroupSetting(groupIdHex:setting:senderLeaf:)``.
    func performReceiveGroupSetting(groupIdHex: String, setting: GroupSettingEnvelope, senderLeaf: UInt32?) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        switch setting {
        case .photo(let incoming):
            // A one-to-one has no group photo (its circle is the person's).
            guard !isOneToOne(groupIdHex: groupIdHex) else { return }
            let resolved = PushGroupRouting.resolveSenderAuthority(group: record.group, senderLeaf: senderLeaf)
            let stored = groupSettingsStore.photo(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            let candidate = StoredGroupPhoto(avatar: ProfileAvatar(incoming.avatar), updatedAtMs: incoming.updatedAtMs)
            let verdict = GroupSettingGate.classify(
                identical: stored == candidate,
                incomingUpdatedAtMs: incoming.updatedAtMs,
                storedUpdatedAtMs: stored?.updatedAtMs,
                senderKey: resolved.senderKey,
                authority: resolved.authority,
                isNewer: { groupPhotoIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) })
            switch verdict {
            case .accept:
                groupSettingsStore.setPhoto(accountIdB64: accountIdB64, groupIdHex: groupIdHex, candidate)
                groupSettingsRevision &+= 1
                // An admin's photo reaches the owner as this envelope.
                notePublicRecordChanged(groupIdHex: groupIdHex)
            case .acceptIdentical:
                return
            case .rejectNotAuthorized, .rejectStale, .rejectUnverifiable:
                NSLog("[group-photo] a group photo was dropped: %@", String(describing: verdict))
            }
        case .reactions(let incoming):
            guard !isOneToOne(groupIdHex: groupIdHex) else { return }
            let resolved = PushGroupRouting.resolveSenderAuthority(group: record.group, senderLeaf: senderLeaf)
            let stored = groupSettingsStore.reactions(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            let candidate = StoredGroupReactions(incoming.reactions, updatedAtMs: incoming.updatedAtMs)
            let verdict = GroupSettingGate.classify(
                identical: stored == candidate,
                incomingUpdatedAtMs: incoming.updatedAtMs,
                storedUpdatedAtMs: stored?.updatedAtMs,
                senderKey: resolved.senderKey,
                authority: resolved.authority,
                isNewer: { groupReactionsIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) })
            switch verdict {
            case .accept:
                groupSettingsStore.setReactions(accountIdB64: accountIdB64, groupIdHex: groupIdHex, candidate)
                groupSettingsRevision &+= 1
            case .acceptIdentical:
                return
            case .rejectNotAuthorized, .rejectStale, .rejectUnverifiable:
                NSLog("[group-reactions] a reactions setting was dropped: %@", String(describing: verdict))
            }
        case .handle(let value):
            // ADR-0087 section 11 — no role check: the certificate and the
            // handle key's signature are the gate (checked off the lock).
            receiveGroupHandle(groupIdHex: groupIdHex, value: value)
        case .knockSeed(let plaintext):
            receiveKnockSeed(accountIdB64: accountIdB64, record: record, groupIdHex: groupIdHex,
                             plaintext: plaintext, senderLeaf: senderLeaf)
        case .knockKey(let key):
            receiveKnockKey(groupIdHex: groupIdHex, key: key, senderLeaf: senderLeaf)
        }
    }

    /// ADR-0087 section 5 — kept only when it names this group and this
    /// account, and its MLS sender resolves at receipt to the owner.
    private func receiveKnockSeed(
        accountIdB64: String, record: GroupRecord, groupIdHex: String, plaintext: Data, senderLeaf: UInt32?
    ) {
        guard let identity = conversationAccountIdentity,
              let decoded = try? decodeKnockSeedEnvelope(plaintext: plaintext),
              decoded.journalId == record.groupId,
              decoded.memberAccountKey == identity.signaturePublicKey() else { return }
        let owner = (try? record.group.groupRoles())?.owner
        let sender = senderLeaf.flatMap { leaf in
            (try? record.group.members())?.first(where: { $0.leafIndex == leaf })?.accountKey
        }
        guard let owner, let sender, sender == owner,
              let seed = try? openKnockSeedEnvelope(plaintext: plaintext, accountIdentity: identity)
        else {
            NSLog("[knock] a knock seed was dropped")
            return
        }
        groupSettingsStore.keepKnockSeed(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, version: decoded.knockVersion, seed: seed)
    }
}
