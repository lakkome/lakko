// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Chat attachment bridge — file bytes ↔ wasm encryption (via
/// UniFFI) ↔ storage HTTP. Mirror of
/// `apps/web/src/chat/attachments.ts` (1.19c2).
///
/// Three Phase-1.19 layers join here on the iOS client:
///
///   1. **Photo / document picker** — yields raw `Data` plus an
///      optional filename + MIME.
///   2. **`LakkoCore` (UniFFI)** — `encryptAttachment` /
///      `decryptAttachment` / `composeEnvelope` / `parseEnvelope`
///      from the Rust-side 1.19a primitives exposed in 1.19b.
///   3. **`StorageClient`** — `lakko-svc-storage` HTTP wrapper
///      added in 1.19c1 that PUTs / GETs ciphertext blobs by
///      content hash.
///
/// The two flows:
///
///   * **Upload** (`encryptAndUpload`) — hand-rolled `Data` →
///     `encryptAttachment` → `StorageClient.putBlob` → return
///     an `AttachmentRef` ready to embed in the next MLS
///     Application message envelope.
///
///   * **Download** (`downloadAndDecrypt`) — `StorageClient.getBlob`
///     → `decryptAttachment` → plaintext bytes.
///
/// Plus `composeEnvelopeBytes` + `parseEnvelopeBytes` that wrap
/// the UniFFI free functions with friendlier ergonomics
/// (`Data` body inputs).
///
/// ## Memory ceiling
///
/// `maxAttachmentBytes = 50 MB` mirrors the
/// `MAX_ATTACHMENT_BYTES` constant in `crates/lakko-attachments`
/// and `services/storage` — surface here so a misbehaving
/// caller fails fast with a friendly error before either the
/// UniFFI AEAD seal or the storage PUT spends cycles.
public enum AttachmentBridge {

    /// Per-attachment ceiling — must match the
    /// `MAX_ATTACHMENT_BYTES` constant in
    /// `crates/lakko-attachments` and `services/storage`.
    public static let maxAttachmentBytes: Int = 50 * 1024 * 1024

    /// Encrypt `bytes` under a fresh random CEK + nonce, PUT
    /// the ciphertext to `lakko-svc-storage`, return the
    /// `AttachmentRef` the caller will embed into the next
    /// outgoing MLS Application message envelope via
    /// ``composeEnvelopeBytes(text:attachments:)``.
    ///
    /// The CEK + nonce are generated **inside Rust** via the
    /// UniFFI `encryptAttachment` free function (which routes
    /// to `lakko_crypto`'s CSPRNG); Swift never holds them as
    /// persistent state, only as the in-flight ref returned
    /// here. Once that ref makes it into the MLS application
    /// message and the message ships, the Swift struct goes
    /// out of scope and is freed.
    ///
    /// - Parameters:
    ///   - bytes:   The raw plaintext bytes (a file body, a
    ///              `UIImage`'s JPEG data, etc.).
    ///   - mime:    Optional MIME type (e.g. `"image/jpeg"`).
    ///              Empty / unsniffed ⇒ pass `nil`.
    ///   - name:    Optional human-visible filename. Empty
    ///              ⇒ pass `nil`.
    ///   - storage: A `StorageClient` to drive the PUT — falls
    ///              back to a default-base-URL instance so most
    ///              callers don't have to thread one through.
    public static func encryptAndUpload(
        bytes: Data,
        mime: String?,
        name: String?,
        /// ADR-0047 — the optional media metadata (kind, duration, waveform,
        /// dimensions, poster thumb) carried on the returned ref. `nil` for a
        /// plain file.
        media: MediaAttachmentMeta? = nil,
        storage: StorageClient = StorageClient(),
        /// Polish 1.x — coarse-grained stage callback the
        /// composer surface drives off to render
        /// "Encrypting…" → "Uploading…" captions instead of a
        /// generic spinner. Invoked synchronously from this
        /// function's thread context; callers that mutate
        /// `@MainActor` state must hop back to MainActor
        /// inside their closure.
        onStage: (@Sendable (UploadStage) -> Void)? = nil,
        /// ADR-0047 — fraction of the ciphertext PUT sent so far (0...1),
        /// for the timeline's determinate upload ring. Off the main actor.
        onProgress: (@Sendable (Double) -> Void)? = nil
    ) async throws -> AttachmentRef {
        guard bytes.count <= maxAttachmentBytes else {
            throw AttachmentBridgeError.tooLarge(
                got: bytes.count,
                max: maxAttachmentBytes
            )
        }
        onStage?(.encrypting)
        // 1) Encrypt via UniFFI — fresh CEK + nonce + SHA-256
        // blob_id all computed inside Rust.
        let enc: EncryptedAttachment
        do {
            enc = try encryptAttachment(plaintext: bytes)
        } catch {
            throw AttachmentBridgeError.encryption(error)
        }

        // 2) PUT the ciphertext under the content-addressed
        // blob_id.
        onStage?(.uploading)
        do {
            try await storage.putBlob(
                blobId: enc.blobId,
                ciphertext: enc.ciphertext,
                progress: onProgress
            )
        } catch {
            throw AttachmentBridgeError.upload(error)
        }

        // 3) Build the wire-shape ref. `size` is the plaintext
        // size (not ciphertext) so the recipient UI can decide
        // whether to fetch on a metered connection without
        // first knowing the +16-byte AEAD overhead.
        return AttachmentRef(
            blobId: enc.blobId,
            key: enc.key,
            nonce: enc.nonce,
            mime: (mime?.isEmpty ?? true) ? nil : mime,
            size: UInt64(bytes.count),
            name: (name?.isEmpty ?? true) ? nil : name,
            kind: media?.kind,
            durationMs: media?.durationMs,
            waveform: media?.waveform,
            width: media?.width,
            height: media?.height,
            thumb: media?.thumb
        )
    }

    /// ADR-0047 — the file-backed sibling of
    /// ``encryptAndUpload(bytes:mime:name:media:storage:onStage:onProgress:)``
    /// for a recording this device just made. The size ceiling is checked
    /// from the file's attributes before any byte is read.
    public static func encryptAndUpload(
        fileURL: URL,
        mime: String?,
        name: String?,
        media: MediaAttachmentMeta?,
        storage: StorageClient = StorageClient(),
        onProgress: (@Sendable (Double) -> Void)? = nil
    ) async throws -> AttachmentRef {
        let attributes = try? FileManager.default.attributesOfItem(atPath: fileURL.path)
        if let size = (attributes?[.size] as? NSNumber)?.intValue, size > maxAttachmentBytes {
            throw AttachmentBridgeError.tooLarge(got: size, max: maxAttachmentBytes)
        }
        let bytes = try Data(contentsOf: fileURL, options: .mappedIfSafe)
        return try await encryptAndUpload(
            bytes: bytes,
            mime: mime,
            name: name,
            media: media,
            storage: storage,
            onProgress: onProgress
        )
    }

    /// ADR-0047 §1 — `ThumbRef.size` is at most this many bytes; the core
    /// drops a larger thumb, so a client refuses to upload one.
    public static let maxThumbBytes = 256 * 1024

    /// ADR-0047 — encrypt and upload a video-note poster frame as its own blob
    /// under its own CEK, returning the ``ThumbRef`` to embed on the note.
    public static func encryptAndUploadThumb(
        bytes: Data,
        mime: String,
        width: UInt32?,
        height: UInt32?,
        storage: StorageClient = StorageClient()
    ) async throws -> ThumbRef {
        guard !bytes.isEmpty, bytes.count <= maxThumbBytes else {
            throw AttachmentBridgeError.tooLarge(got: bytes.count, max: maxThumbBytes)
        }
        let ref = try await encryptAndUpload(bytes: bytes, mime: mime, name: nil, storage: storage)
        return ThumbRef(
            blobId: ref.blobId,
            key: ref.key,
            nonce: ref.nonce,
            mime: mime,
            size: UInt64(bytes.count),
            width: width,
            height: height
        )
    }

    /// ADR-0047 — download and authenticate a poster thumb. Same AEAD open as
    /// any attachment: a forged or truncated blob fails before a byte returns.
    public static func downloadAndDecrypt(
        thumb: ThumbRef,
        storage: StorageClient = StorageClient()
    ) async throws -> Data {
        try await downloadAndDecrypt(
            ref: AttachmentRef(
                blobId: thumb.blobId,
                key: thumb.key,
                nonce: thumb.nonce,
                mime: thumb.mime,
                size: thumb.size,
                name: nil
            ),
            storage: storage
        )
    }

    /// GET the ciphertext for `ref` from storage, hand to
    /// `decryptAttachment`, return the plaintext bytes.
    ///
    /// Throws ``AttachmentBridgeError/missing(blobId:)`` on 404
    /// from storage — the blob has been GC'd or never made it
    /// onto the server (a sender whose PUT failed but whose
    /// MLS message still shipped). The caller decides whether
    /// to surface this as "attachment unavailable" or to retry.
    public static func downloadAndDecrypt(
        ref: AttachmentRef,
        storage: StorageClient = StorageClient()
    ) async throws -> Data {
        let ciphertext: Data
        do {
            guard let body = try await storage.getBlob(blobId: ref.blobId)
            else {
                throw AttachmentBridgeError.missing(blobId: ref.blobId)
            }
            ciphertext = body
        } catch let err as AttachmentBridgeError {
            throw err
        } catch {
            throw AttachmentBridgeError.download(error)
        }
        do {
            return try decryptAttachment(
                ciphertext: ciphertext,
                key: ref.key,
                nonce: ref.nonce
            )
        } catch {
            throw AttachmentBridgeError.decryption(error)
        }
    }

    /// Build the MLS Application-message JSON envelope from
    /// `(text, attachments)`. Empty `text` is valid
    /// (attachment-only message); empty `attachments` is valid
    /// (text-only message — same wire shape as the pre-1.19
    /// plain-UTF-8 path).
    ///
    /// Hand the resulting bytes to
    /// `Group.createApplicationMessage(identity:plaintext:)`
    /// exactly like the pre-1.19 plain-UTF-8 path did.
    public static func composeEnvelopeBytes(
        text: String,
        attachments: [AttachmentRef]
    ) throws -> Data {
        try composeEnvelope(text: text, attachments: attachments)
    }

    /// Parse an incoming MLS Application-message body. Returns
    /// the canonical `(text, attachments)` pair from the
    /// UniFFI envelope decoder.
    ///
    /// **Never throws.** Legacy pre-1.19 plain-UTF-8 bytes
    /// collapse to `(text: <utf-8 lossy>, attachments: [])`,
    /// unknown-version envelopes collapse to
    /// `(text: "[unsupported v$N message]", attachments: [])`,
    /// malformed individual refs are silently skipped. Same
    /// posture as the UniFFI `parse_envelope` documented
    /// fallback paths (1.19a + 1.19b).
    public static func parseEnvelopeBytes(
        _ bytes: Data
    ) -> (text: String, attachments: [AttachmentRef]) {
        let parsed = parseEnvelope(bytes: bytes)
        return (parsed.text, parsed.attachments)
    }

    /// ADR-0023 — parse an incoming MLS Application-message body
    /// into the structured `MessageBody` shape (chat / edit /
    /// delete / reaction). Wraps the UniFFI `parseBodyEnvelope`
    /// free function, which returns a `ParsedBody` carrying a
    /// `kind` discriminator + the per-kind fields.
    ///
    /// **Never throws.** The Rust parser is bounded + fail-closed:
    /// a malformed, ambiguous (more than one op field set), or
    /// unknown-version envelope degrades to `.chat` with the raw
    /// text, never panics. The chat receive loop branches on
    /// `kind` to fold ops into their target rather than rendering
    /// them as bubbles.
    public static func parseBodyEnvelopeBytes(_ bytes: Data) -> ParsedBody {
        parseBodyEnvelope(bytes: bytes)
    }

    /// ADR-0052 decision 1 — compose a **reply**: an ordinary chat envelope
    /// (`text` + `attachments`, exactly as ``composeEnvelopeBytes``) that also
    /// carries the target's DS `messageId` and the snapshot of the quoted
    /// author and excerpt taken when Reply was chosen. The core truncates an
    /// over-long snapshot on a character boundary and drops an empty one, so
    /// the caller passes what the spec says and nothing is pre-trimmed here.
    public static func composeReplyBytes(
        text: String,
        attachments: [AttachmentRef],
        reply: ReplyRef
    ) throws -> Data {
        try composeReplyEnvelope(
            text: text,
            attachments: attachments,
            replyTo: reply.target,
            replyAuthor: reply.author,
            replyExcerpt: reply.excerpt
        )
    }

    /// ADR-0052 decision 2 — compose a **forward**: an ordinary chat envelope
    /// (`text` + `attachments`, already re-encrypted under fresh keys by the
    /// caller) that also carries the original author's display name and,
    /// when the forwarder held the original, its conversation id and DS
    /// `messageId`. The core truncates an over-long name on a character
    /// boundary and drops an over-long origin, so nothing is pre-trimmed
    /// here. The origin group id crosses as STANDARD base64 of the raw id
    /// (the wire shape), converted from the app's hex by
    /// ``ForwardPayload/groupIdB64(fromHex:)``.
    public static func composeForwardBytes(
        text: String,
        attachments: [AttachmentRef],
        forward: ForwardRef
    ) throws -> Data {
        let originB64 = forward.originGroupIdHex.flatMap(ForwardPayload.groupIdB64(fromHex:))
        return try composeForwardEnvelope(
            text: text,
            attachments: attachments,
            forwardedFrom: forward.author,
            originGroupIdB64: originB64,
            originMessageId: originB64 == nil ? nil : forward.originMessageId
        )
    }

    /// ADR-0023 — compose an **edit** op envelope: rewrite the
    /// text of the message identified by the DS `target` id to
    /// `newText`. The result is a normal MLS Application message
    /// body; the caller publishes it via the existing application
    /// send path. Only the original author's edit is accepted by
    /// peers (the receive-side author-only gate enforces it).
    public static func composeEditBytes(
        target: Int64,
        newText: String
    ) throws -> Data {
        try composeEditEnvelope(target: target, newText: newText)
    }

    /// ADR-0023 — compose a **delete** op envelope: tombstone the
    /// message identified by the DS `target` id. Author-only on
    /// the receive side.
    public static func composeDeleteBytes(target: Int64) throws -> Data {
        try composeDeleteEnvelope(target: target)
    }

    /// ADR-0023 — compose a **reaction** op envelope: react to the
    /// message identified by the DS `target` id with `emoji`. Any
    /// member may react; re-sending the same emoji toggles it off
    /// on the receive side. Throws if `emoji` is empty or exceeds
    /// the Rust-side `MAX_EMOJI_BYTES` cap (64 bytes).
    public static func composeReactionBytes(
        target: Int64,
        emoji: String
    ) throws -> Data {
        try composeReactionEnvelope(target: target, emoji: emoji)
    }

    /// Format an attachment size for UI display — `42 B`,
    /// `1.2 kB`, `4.7 MB`. SI-decimal (matches the iOS Files
    /// app); base-2 conversion is too pedantic for chat UI.
    public static func formatSize(_ bytes: UInt64) -> String {
        if bytes < 1_000 { return "\(bytes) B" }
        if bytes < 1_000_000 {
            return String(format: "%.1f kB", Double(bytes) / 1_000)
        }
        if bytes < 1_000_000_000 {
            return String(format: "%.1f MB", Double(bytes) / 1_000_000)
        }
        return String(format: "%.1f GB", Double(bytes) / 1_000_000_000)
    }

    /// `true` when the ref's MIME type is one the chat UI knows
    /// how to inline-preview as an image. Decided by the core's
    /// ADR-0047 allowlist (`inlineMediaType(mime:)`) — the four web
    /// image formats, parameters ignored. SVG is never on it — an
    /// attacker-supplied SVG can carry script / runtime markup.
    public static func isInlinePreviewable(_ ref: AttachmentRef) -> Bool {
        InlineMedia.inlineType(mime: ref.mime) == .image
    }

    /// A filename that is safe to join onto a local directory, derived from
    /// the ref's `name`.
    ///
    /// `name` is chosen by the SENDER and arrives inside an MLS application
    /// message, so every member of a group controls it. Joined onto a
    /// directory verbatim, `../Library/Preferences/x` walks out of the temp
    /// directory and overwrites app-private files when the recipient taps
    /// Save. This keeps only the last path component (splitting on both `/`
    /// and `\`), drops control and format characters (the latter include the
    /// right-to-left override that disguises an extension), trims, caps the
    /// length, and falls back when nothing usable is left — including `.` and
    /// `..`, which are path components rather than names.
    public static func safeFileName(_ raw: String?, fallback: String) -> String {
        guard let raw else { return fallback }
        let lastComponent = raw.split(
            omittingEmptySubsequences: false,
            whereSeparator: { $0 == "/" || $0 == "\\" }
        ).last.map(String.init) ?? ""
        var cleaned = String.UnicodeScalarView()
        for scalar in lastComponent.unicodeScalars {
            switch scalar.properties.generalCategory {
            case .control, .format:
                continue
            default:
                cleaned.append(scalar)
            }
        }
        var name = String(cleaned).trimmingCharacters(in: .whitespacesAndNewlines)
        if name.count > maxSafeFileNameLength {
            name = String(name.prefix(maxSafeFileNameLength))
        }
        if name.isEmpty || name == "." || name == ".." {
            return fallback
        }
        return name
    }

    /// Upper bound on `safeFileName`'s output, in characters.
    public static let maxSafeFileNameLength = 128

    /// Lowercase hex of an arbitrary byte buffer — used by the
    /// UI for the short-fallback filename when an attachment
    /// ref carries no `name`. Mirror of
    /// `StorageClient.blobIdHex`.
    public static func hex(_ bytes: Data) -> String {
        var out = String()
        out.reserveCapacity(bytes.count * 2)
        for b in bytes {
            let hi = Int(b >> 4) & 0x0F
            let lo = Int(b) & 0x0F
            out.append(hexAlphabet[hi])
            out.append(hexAlphabet[lo])
        }
        return out
    }

    private static let hexAlphabet: [Character] = Array("0123456789abcdef")
}

/// ADR-0047 §1 — the optional media metadata an attachment carries. Every
/// field is bounded by `lakko-attachments`, which drops an out-of-bound value
/// rather than failing the envelope; this client never produces one.
public struct MediaAttachmentMeta: Equatable, Sendable {
    /// `voice`, `video_note` or `gif`; `nil` for a plain photo or video,
    /// whose bubble is decided by its MIME type alone (UX spec section 10).
    public var kind: String?
    public var durationMs: UInt32?
    /// At most 128 amplitude bytes.
    public var waveform: Data?
    public var width: UInt32?
    public var height: UInt32?
    public var thumb: ThumbRef?

    public init(
        kind: String?,
        durationMs: UInt32? = nil,
        waveform: Data? = nil,
        width: UInt32? = nil,
        height: UInt32? = nil,
        thumb: ThumbRef? = nil
    ) {
        self.kind = kind
        self.durationMs = durationMs
        self.waveform = waveform
        self.width = width
        self.height = height
        self.thumb = thumb
    }
}

/// Coarse-grained pipeline stage reported through
/// [``AttachmentBridge/encryptAndUpload(bytes:mime:name:storage:onStage:)``]'s
/// `onStage` callback (Polish 1.x). The callback fires once per
/// stage, before the corresponding work begins, so the UI can
/// flip its caption to match the about-to-happen work.
public enum UploadStage: Sendable {
    /// About to run `encryptAttachment` over the plaintext bytes.
    case encrypting
    /// About to PUT the ciphertext to `services/storage`.
    case uploading
}

// MARK: - Errors

public enum AttachmentBridgeError: Error {
    /// Plaintext above `maxAttachmentBytes` — fail fast before
    /// any encryption / upload.
    case tooLarge(got: Int, max: Int)
    /// `encryptAttachment` from the UniFFI surface failed.
    case encryption(Error)
    /// `decryptAttachment` from the UniFFI surface failed —
    /// wrong key, wrong nonce, AEAD tag mismatch.
    case decryption(Error)
    /// `StorageClient.putBlob` failed (4xx / 5xx / network).
    case upload(Error)
    /// `StorageClient.getBlob` failed with a non-404 error.
    case download(Error)
    /// Storage returned 404 — the blob is no longer there.
    case missing(blobId: Data)
}

extension AttachmentBridgeError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .tooLarge(let got, let max):
            return "attachment \(got) bytes exceeds the \(max)-byte ceiling"
        case .encryption(let err):
            return "attachment encryption failed: \(err)"
        case .decryption(let err):
            return "attachment decryption failed: \(err)"
        case .upload(let err):
            return "attachment upload failed: \(err)"
        case .download(let err):
            return "attachment download failed: \(err)"
        case .missing:
            return "the attachment is no longer available on the server"
        }
    }
}
