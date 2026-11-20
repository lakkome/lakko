// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.Strings
import me.lakko.app.ui.components.openInSystemBrowser

/**
 * The Terms of Service and Privacy Policy pages (`docs/ux/legal-pages.md`):
 * fixed paths on the web origin of the deployment this build talks to
 * ([ApiEndpoints.webOrigin]), so a test-stack build links the test stack.
 */
public object LegalPages {
    /** Path of the Terms of Service page on the web origin. */
    public const val TERMS_PATH: String = "/terms"

    /** Path of the Privacy Policy page on the web origin. */
    public const val PRIVACY_PATH: String = "/privacy"

    /** `<origin>/terms`; [origin] is normalised (no trailing slash). */
    public fun termsUrl(origin: String = ApiEndpoints.webOrigin): String = join(origin, TERMS_PATH)

    /** `<origin>/privacy`; [origin] is normalised (no trailing slash). */
    public fun privacyUrl(origin: String = ApiEndpoints.webOrigin): String = join(origin, PRIVACY_PATH)

    private fun join(origin: String, path: String): String {
        val base = ApiEndpoints.normalizedBase(origin) ?: ApiEndpoints.PRODUCTION_WEB_ORIGIN
        return base + path
    }
}

/** The sign-up notice as text plus the character ranges of its two links. */
public data class LegalNoticeText(
    val text: String,
    /** Range of the Terms of Service link in [text], or `null` if the
     *  translation dropped the placeholder. */
    val terms: IntRange?,
    /** Range of the Privacy Policy link in [text], or `null`. */
    val privacy: IntRange?,
)

private const val TERMS_MARK = ""
private const val PRIVACY_MARK = ""

/**
 * Assemble `legal.signup_notice` with its `{terms}` / `{privacy}` arguments
 * filled by `legal.terms` / `legal.privacy`, and find where each link word
 * sits. The message is formatted with private-use markers as the arguments
 * (so the formatter's own isolates and word order stay intact), then each
 * marker is replaced by its link word and its range recorded.
 */
public fun legalNoticeText(strings: Strings): LegalNoticeText = assembleLegalNotice(
    format = { texts -> strings.format("legal.signup_notice", texts) },
    termsWord = strings["legal.terms"],
    privacyWord = strings["legal.privacy"],
)

/** The assembly alone, testable with any formatter. */
public fun assembleLegalNotice(
    format: (Map<String, String>) -> String,
    termsWord: String,
    privacyWord: String,
): LegalNoticeText {
    val template = format(mapOf("terms" to TERMS_MARK, "privacy" to PRIVACY_MARK))
    val out = StringBuilder()
    var terms: IntRange? = null
    var privacy: IntRange? = null
    for (ch in template) {
        when (ch.toString()) {
            TERMS_MARK -> {
                val start = out.length
                out.append(termsWord)
                if (terms == null) terms = start until out.length
            }
            PRIVACY_MARK -> {
                val start = out.length
                out.append(privacyWord)
                if (privacy == null) privacy = start until out.length
            }
            else -> out.append(ch)
        }
    }
    return LegalNoticeText(out.toString(), terms, privacy)
}

/** [notice] as an annotated string whose two link words open [onOpen] with their URL. */
internal fun legalNoticeAnnotated(
    notice: LegalNoticeText,
    termsUrl: String,
    privacyUrl: String,
    linkColor: Color,
    onOpen: (String) -> Unit,
): AnnotatedString {
    val styles = TextLinkStyles(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline))
    val links = listOfNotNull(
        notice.terms?.let { it to termsUrl },
        notice.privacy?.let { it to privacyUrl },
    ).sortedBy { it.first.first }
    return buildAnnotatedString {
        var at = 0
        for ((range, url) in links) {
            if (range.first < at) continue
            append(notice.text.substring(at, range.first))
            withLink(LinkAnnotation.Url(url, styles) { onOpen(url) }) {
                append(notice.text.substring(range.first, range.last + 1))
            }
            at = range.last + 1
        }
        append(notice.text.substring(at))
    }
}

/**
 * The muted line under a Create account button (`docs/ux/legal-pages.md`
 * section 2 items 1 and 2). Each link word is a [LinkAnnotation.Url], so
 * TalkBack lists it as a link; a tap opens the page in the system browser.
 */
@Composable
internal fun LegalSignupNotice(modifier: Modifier = Modifier) {
    val strings = LocalStrings.current
    val context = LocalContext.current
    val linkColor = MaterialTheme.colorScheme.primary
    val termsUrl = LegalPages.termsUrl()
    val privacyUrl = LegalPages.privacyUrl()
    val annotated = remember(strings, context, linkColor, termsUrl, privacyUrl) {
        legalNoticeAnnotated(legalNoticeText(strings), termsUrl, privacyUrl, linkColor) { url ->
            openInSystemBrowser(context, url)
        }
    }
    Text(
        text = annotated,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.fillMaxWidth(),
    )
}
