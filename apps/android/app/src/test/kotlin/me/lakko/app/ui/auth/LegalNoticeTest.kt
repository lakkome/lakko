// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.LinkAnnotation
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** `docs/ux/legal-pages.md`: the page URLs and the sign-up notice with its two links. */
class LegalNoticeTest {

    @Test
    fun pagesHangOffTheWebOrigin() {
        assertEquals("https://test.lakko.me/terms", LegalPages.termsUrl("https://test.lakko.me"))
        assertEquals("https://test.lakko.me/privacy", LegalPages.privacyUrl("https://test.lakko.me/"))
        assertEquals("https://lakko.me/terms", LegalPages.termsUrl("  https://lakko.me//  "))
    }

    @Test
    fun anEmptyOriginFallsBackToProduction() {
        assertEquals("${ApiEndpoints.PRODUCTION_WEB_ORIGIN}/privacy", LegalPages.privacyUrl(""))
    }

    @Test
    fun theDefaultIsThisBuildsOrigin() {
        assertEquals("${ApiEndpoints.webOrigin}/terms", LegalPages.termsUrl())
        assertEquals("${ApiEndpoints.webOrigin}/privacy", LegalPages.privacyUrl())
    }

    @Test
    fun englishNoticeReadsAsTheSpecAndMarksBothLinks() {
        val notice = legalNoticeText(EnglishForTests.strings)
        val plain = notice.text.filterNot { it == '⁨' || it == '⁩' }
        assertEquals("By creating an account, you agree to the Terms of Service and the Privacy Policy.", plain)
        val terms = notice.terms!!
        val privacy = notice.privacy!!
        assertEquals("Terms of Service", notice.text.substring(terms.first, terms.last + 1))
        assertEquals("Privacy Policy", notice.text.substring(privacy.first, privacy.last + 1))
    }

    @Test
    fun aTranslationThatReordersTheLinksStillFindsBoth() {
        val notice = assembleLegalNotice(
            format = { a -> "Avec ${a["privacy"]} puis ${a["terms"]}." },
            termsWord = "CGU",
            privacyWord = "Confidentialite",
        )
        assertEquals("Avec Confidentialite puis CGU.", notice.text)
        assertEquals(5..19, notice.privacy)
        assertEquals(26..28, notice.terms)
    }

    @Test
    fun aTranslationThatDropsAPlaceholderLosesOnlyThatLink() {
        val notice = assembleLegalNotice(
            format = { a -> "See ${a["terms"]}." },
            termsWord = "Terms",
            privacyWord = "Privacy",
        )
        assertEquals("See Terms.", notice.text)
        assertEquals(4..8, notice.terms)
        assertNull(notice.privacy)
    }

    @Test
    fun theAnnotatedNoticeLinksEachWordToItsPage() {
        val notice = assembleLegalNotice(
            format = { a -> "Agree to ${a["terms"]} and ${a["privacy"]}." },
            termsWord = "Terms",
            privacyWord = "Privacy",
        )
        val annotated = legalNoticeAnnotated(notice, "https://o/terms", "https://o/privacy", Color.Blue) {}
        assertEquals(notice.text, annotated.text)
        val links = annotated.getLinkAnnotations(0, annotated.length)
            .map { annotated.text.substring(it.start, it.end) to (it.item as LinkAnnotation.Url).url }
        assertEquals(listOf("Terms" to "https://o/terms", "Privacy" to "https://o/privacy"), links)
    }
}
