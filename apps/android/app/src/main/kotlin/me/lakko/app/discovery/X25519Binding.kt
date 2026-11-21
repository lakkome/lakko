// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The account X25519 key a directory serves, taken only when the account's
// signature key signed it.

package me.lakko.app.discovery

/**
 * Checks a signed X25519 binding: the 64-byte Ed25519 signature by an
 * account's signature key over its X25519 key (`verifyX25519Binding`).
 */
public fun interface X25519BindingVerifier {
    /** `true` only when [binding] verifies for the pair. */
    public fun verify(binding: ByteArray, ed25519PublicKey: ByteArray, x25519PublicKey: ByteArray): Boolean
}

/**
 * The verifier every seal site uses. The default is the Rust core; a JVM unit
 * test, which has no native library, replaces it.
 */
public object X25519Bindings {
    /** Length of a binding (`x25519BindingLen()`), checked before the call. */
    public const val BINDING_LEN: Int = 64

    private const val KEY_LEN: Int = 32

    @Volatile
    public var verifier: X25519BindingVerifier = X25519BindingVerifier { binding, ed, x ->
        uniffi.lakko_ffi_uniffi.verifyX25519Binding(binding, ed, x)
    }

    /**
     * The X25519 key served beside [signaturePubkeyB64], or `null` when the
     * binding is absent, malformed or does not verify. A caller that gets
     * `null` seals nothing (fail closed): a directory could otherwise put its
     * own X25519 key beside a person's signature key.
     */
    public fun boundX25519Key(
        signaturePubkeyB64: String,
        x25519PubkeyB64: String,
        bindingB64: String?,
        check: X25519BindingVerifier = verifier,
    ): ByteArray? {
        if (bindingB64.isNullOrEmpty()) return null
        return try {
            val decoder = java.util.Base64.getDecoder()
            val ed = decoder.decode(signaturePubkeyB64)
            val x = decoder.decode(x25519PubkeyB64)
            val binding = decoder.decode(bindingB64)
            if (ed.size != KEY_LEN || x.size != KEY_LEN || binding.size != BINDING_LEN) return null
            if (check.verify(binding, ed, x)) x else null
        } catch (_: Throwable) {
            null
        }
    }
}

/** This bundle's X25519 key when its binding verifies; `null` otherwise. */
public fun IdentityFetchResponse.boundX25519Key(
    check: X25519BindingVerifier = X25519Bindings.verifier,
): ByteArray? = X25519Bindings.boundX25519Key(signature_pubkey, x25519_pubkey, x25519_binding, check)
