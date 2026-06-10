// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Host-JVM tests for [isPlausibleContact] (ADR-0018 factor (d)) —
 *  the permissive client-side mirror of the server's
 *  `normalize_contact` gate (`services/auth/src/recovery_contact.rs`)
 *  and of the web `apps/web/src/auth/contact-shape.ts`. */
public class ContactShapeTest {

    @Test
    public fun acceptsPlausibleEmails() {
        assertTrue(isPlausibleContact(ContactKind.EMAIL, "alice@example.com"))
        assertTrue(isPlausibleContact(ContactKind.EMAIL, "bob+tag@mail.co"))
        // Trimmed before the check.
        assertTrue(isPlausibleContact(ContactKind.EMAIL, "  alice@example.com  "))
    }

    @Test
    public fun rejectsMalformedEmails() {
        assertFalse(isPlausibleContact(ContactKind.EMAIL, "no-at-sign"))
        assertFalse(isPlausibleContact(ContactKind.EMAIL, "@no-local.com"))
        assertFalse(isPlausibleContact(ContactKind.EMAIL, "no-domain@"))
        assertFalse(isPlausibleContact(ContactKind.EMAIL, "two@@at.com"))
        assertFalse(isPlausibleContact(ContactKind.EMAIL, "has space@x.com"))
        assertFalse(isPlausibleContact(ContactKind.EMAIL, ""))
        assertFalse(isPlausibleContact(ContactKind.EMAIL, "   "))
    }

    @Test
    public fun acceptsPlausiblePhones() {
        assertTrue(isPlausibleContact(ContactKind.PHONE, "+15551234567"))
        // The common human separators are stripped before the check.
        assertTrue(isPlausibleContact(ContactKind.PHONE, "+1 (555) 123-4567"))
        assertTrue(isPlausibleContact(ContactKind.PHONE, "+44-20-7946-0958"))
        assertTrue(isPlausibleContact(ContactKind.PHONE, "  +15551234567  "))
        // 15 digits is the E.164 ceiling — accepted.
        assertTrue(isPlausibleContact(ContactKind.PHONE, "+123456789012345"))
    }

    @Test
    public fun rejectsMalformedPhones() {
        assertFalse(isPlausibleContact(ContactKind.PHONE, "5551234567")) // no leading +
        assertFalse(isPlausibleContact(ContactKind.PHONE, "+")) // no digits
        assertFalse(isPlausibleContact(ContactKind.PHONE, "+abc1234")) // non-digit
        assertFalse(isPlausibleContact(ContactKind.PHONE, "+1234567890123456")) // 16 digits
        assertFalse(isPlausibleContact(ContactKind.PHONE, ""))
    }

    @Test
    public fun rejectsUnknownKind() {
        // Fail-closed: an unrecognised kind discriminator is never
        // plausible (defends against a future renumber drift).
        assertFalse(isPlausibleContact(0, "alice@example.com"))
        assertFalse(isPlausibleContact(9, "+15551234567"))
    }
}
