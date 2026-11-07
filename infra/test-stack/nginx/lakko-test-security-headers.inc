# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Security headers of test.lakko.me, included by nginx/lakko-test.conf at
# server level AND inside every location that sets an add_header of its own:
# nginx inherits add_header from the server level only into locations that
# set none, so without the include such a location (index.html, the link
# preview pages, /assets/, ...) went out with none of these headers.
#
# Install: /etc/nginx/snippets/lakko-test-security-headers.inc
#
# Cross-Origin-Resource-Policy comes from $lakko_test_corp (a map in
# lakko-test.conf): "same-origin" everywhere except the link preview picture,
# which other apps' pages may load.
add_header Content-Security-Policy
    "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; connect-src 'self' wss://test.lakko.me; worker-src 'self'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'; object-src 'none'"
    always;
add_header X-Frame-Options              "DENY"        always;
add_header X-Content-Type-Options       "nosniff"     always;
add_header Referrer-Policy              "no-referrer" always;
add_header Cross-Origin-Opener-Policy   "same-origin" always;
add_header Cross-Origin-Resource-Policy $lakko_test_corp always;
add_header Permissions-Policy
    "accelerometer=(), ambient-light-sensor=(), autoplay=(self), battery=(), camera=(self), display-capture=(self), fullscreen=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(self), midi=(), payment=(), picture-in-picture=(), publickey-credentials-get=(self), screen-wake-lock=(), usb=(), xr-spatial-tracking=()"
    always;
