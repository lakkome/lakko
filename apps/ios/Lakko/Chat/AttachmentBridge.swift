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
        storage: StorageClient = StorageClient()
    ) async throws -> AttachmentRef {
        guard bytes.count <= maxAttachmentBytes else {
            throw AttachmentBridgeError.tooLarge(
                got: bytes.count,
                max: maxAttachmentBytes
            )
        }
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
        do {
            try await storage.putBlob(
                blobId: enc.blobId,
                ciphertext: enc.ciphertext
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
            name: (name?.isEmpty ?? true) ? nil : name
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
    /// how to inline-preview. Currently: the four core web
    /// image formats. SVG is deliberately excluded — an
    /// attacker-supplied SVG can carry JS / runtime markup, and
    /// even though iOS doesn't render SVG as a web view, the
    /// safe path is to deny preview consistently across formats
    /// that can / cannot tile.
    public static func isInlinePreviewable(_ ref: AttachmentRef) -> Bool {
        guard let m = ref.mime else { return false }
        return m == "image/jpeg" || m == "image/png"
            || m == "image/webp" || m == "image/gif"
    }

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
