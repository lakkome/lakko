// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.TEST_INVITE_LINK_LENGTHS
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CallInviteCapability
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.NoHandle

/**
 * Host-side unit tests for [CallInviteAcceptor]'s slug gate.
 *
 * These sit one level ABOVE `CallInviteUrlTest`, which drives
 * [parseCallInviteSlug] directly. The level that matters for a
 * user is the acceptor: `acceptFromSlug` catches only
 * [CallInviteUrlException] (`CallInviteAcceptor.kt` step 1), so a
 * RAW `IllegalArgumentException` escaping the parser would blow
 * straight past that handler and out of the whole pipeline — on
 * input that is, by construction, a link a stranger handed the
 * user. A parser-only test cannot see that; this one can.
 *
 * ## Why the joiner identity is a fake
 *
 * `Identity(NoHandle)` is UniFFI's sanctioned test constructor for
 * an object with no connected Rust handle — it loads no native
 * library, so these tests run under `:app:testDebugUnitTest` on the
 * host JVM. Touching the fake would throw, which is exactly the
 * property we want: both tests below return before
 * `joinerIdentity` is ever dereferenced (the parse gate and the
 * fetch gate both short-circuit ahead of step 6's
 * `acceptCallInvite`), so a green run also proves the pipeline
 * really did stop where we claim it did.
 */
class CallInviteAcceptorTest {

    /** A well-formed 16-character link secret (12 zero bytes). */
    private val SECRET = base64UrlEncodeNoPad(ByteArray(12))

    @Test
    fun `a link without its fragment is damaged before any request`() = runTest {
        val engine = MockEngine { error("discovery must never be reached for a damaged link") }
        val slug = base64UrlEncodeNoPad(ByteArray(TEST_INVITE_LINK_LENGTHS.blobIdLen) { 3 })
        for (argument in listOf(slug, "$slug#", "$slug#short")) {
            val result = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS })
                .use { discovery ->
                    CallInviteAcceptor(discovery, TEST_INVITE_LINK_LENGTHS).acceptFromSlug(
                        slug = argument,
                        joinerAccountId = joinerAccountId(),
                        joinerIdentity = Identity(NoHandle),
                    )
                }
            assertTrue("expected Damaged for '$argument', got $result", result is CallInviteAcceptor.Result.Damaged)
        }
    }

    /** A 16-byte UUIDv7-shaped account id; never reaches the wire
     *  in either test, both of which return before consume. */
    private fun joinerAccountId(): ByteArray = ByteArray(16) { it.toByte() }

    @Test
    fun `acceptFromSlug returns MalformedSlug for characters outside the base64url alphabet`() =
        runTest {
            // `*` is not in the base64url alphabet. `java.util.Base64`
            // throws `IllegalArgumentException`; `parseCallInviteSlug`
            // must translate it to `CallInviteUrlException` so the
            // acceptor's step-1 handler can catch it.
            val engine = MockEngine {
                error("discovery must never be reached for a malformed slug")
            }
            val result = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS })
                .use { discovery ->
                    CallInviteAcceptor(discovery, TEST_INVITE_LINK_LENGTHS).acceptFromSlug(
                        slug = "not*valid*base64*url#$SECRET",
                        joinerAccountId = joinerAccountId(),
                        joinerIdentity = Identity(NoHandle),
                    )
                }
            assertTrue(
                "expected MalformedSlug, got $result",
                result is CallInviteAcceptor.Result.MalformedSlug,
            )
        }

    @Test
    fun `acceptFromSlug reaches discovery for a well-formed pointer slug`() = runTest {
        // Positive control for the test above: a 6-byte pointer slug
        // clears the parse gate, so the failure must come from the
        // FETCH (404 -> NotFound) rather than the parser. Without
        // this, a MalformedSlug result could equally mean the
        // pipeline never ran at all.
        var requests = 0
        val engine = MockEngine {
            requests += 1
            respond(
                content = "",
                status = HttpStatusCode.NotFound,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        val slug = base64UrlEncodeNoPad(ByteArray(TEST_INVITE_LINK_LENGTHS.blobIdLen) { it.toByte() })
        val result = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS })
            .use { discovery ->
                CallInviteAcceptor(discovery, TEST_INVITE_LINK_LENGTHS).acceptFromSlug(
                    slug = "$slug#$SECRET",
                    joinerAccountId = joinerAccountId(),
                    joinerIdentity = Identity(NoHandle),
                )
            }
        assertTrue(
            "expected NotFound, got $result",
            result is CallInviteAcceptor.Result.NotFound,
        )
        assertTrue("discovery should have been called", requests > 0)
    }

    @Test
    fun `acceptFromSlug returns RateLimited when the ledger fetch answers 429`() = runTest {
        val engine = MockEngine {
            respond(
                content = "",
                status = HttpStatusCode.TooManyRequests,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        val slug = base64UrlEncodeNoPad(ByteArray(TEST_INVITE_LINK_LENGTHS.blobIdLen) { 3 })
        val result = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS })
            .use { discovery ->
                CallInviteAcceptor(discovery, TEST_INVITE_LINK_LENGTHS).acceptFromSlug(
                    slug = "$slug#$SECRET",
                    joinerAccountId = joinerAccountId(),
                    joinerIdentity = Identity(NoHandle),
                )
            }
        assertTrue(
            "expected RateLimited (never NotFound / MalformedSlug), got $result",
            result is CallInviteAcceptor.Result.RateLimited,
        )
    }

    @Test
    fun `acceptFromSlug refuses the pre-amendment 32-byte pointer before any request`() = runTest {
        val engine = MockEngine {
            error("discovery must never be reached for an old-width slug")
        }
        val slug = base64UrlEncodeNoPad(ByteArray(32) { it.toByte() })
        val result = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS })
            .use { discovery ->
                CallInviteAcceptor(discovery, TEST_INVITE_LINK_LENGTHS).acceptFromSlug(
                    slug = "$slug#$SECRET",
                    joinerAccountId = joinerAccountId(),
                    joinerIdentity = Identity(NoHandle),
                )
            }
        assertTrue(
            "expected MalformedSlug, got $result",
            result is CallInviteAcceptor.Result.MalformedSlug,
        )
    }

    @Test
    fun `the link grant check is the core's, and a refusal means a damaged link`() {
        // ADR-0078 phase 1 - the acceptor asks the core for the owner's
        // verified mailbox key; it no longer checks the grant itself. The
        // capability is a handle-less fake: the check must reach it only
        // through the verifier it was given.
        val capability = CallInviteCapability(NoHandle)
        val ownerMailbox = ByteArray(32) { 7 }
        var asked: CallInviteCapability? = null
        assertArrayEquals(
            ownerMailbox,
            callLinkGrantOwnerMailboxOrNull(capability) { asked = it; ownerMailbox },
        )
        assertTrue(asked === capability)
        assertNull(
            callLinkGrantOwnerMailboxOrNull(capability) {
                throw FfiException.Mailbox("binding does not verify")
            },
        )
    }
}
