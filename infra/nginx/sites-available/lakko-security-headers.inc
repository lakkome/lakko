# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Security headers of lakko.me, included by lakko.conf at server level AND
# inside every location that sets an add_header of its own: nginx inherits
# add_header from the server level only into locations that set none, so
# without the include such a location (index.html, the link preview pages,
# /assets/, ...) went out with none of these headers.
#
# Install: /etc/nginx/sites-available/lakko-security-headers.inc (next to
# lakko-proxy-headers.inc; see the install comment at the top of lakko.conf).
#
# The lines keep the indentation they had inside the server block: the
# multi-line header values below carry it, and moving the text unchanged
# keeps every value byte-identical.

    # ---- Security headers (Phase 1.17a/c) ----------------------------------
    # Source of truth for the directives a `<meta>` tag cannot
    # carry (`frame-ancestors`, HSTS, COOP/CORP, Permissions-
    # Policy). Keep `script-src` / `style-src` / `connect-src` /
    # `worker-src` etc. in lock-step with
    # `apps/web/scripts/post-build.mjs::CSP`.
    #
    # `connect-src` names the cross-origin endpoints the production bundle
    # dials. `wss://sfu.lakko.me` MUST equal the origin of the
    # `VITE_LAKKO_SFU_URL` the production web build was made with (the SFU
    # vhost is `sfu.conf`); without it every group call fails at the
    # WebSocket open. The post-build script derives the meta copy from that
    # env var and warns when an origin it derived is missing here. When the
    # build also sets `VITE_LAKKO_TRANSPARENCY_URL`, add that origin too.
    #
    # `img-src` / `media-src` carry `blob:` for decrypted attachment previews
    # (`URL.createObjectURL`), which `'self'` does not match.
    add_header Content-Security-Policy
        "default-src 'self'; \
         script-src 'self' 'wasm-unsafe-eval'; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; \
         media-src 'self' blob:; \
         font-src 'self'; \
         connect-src 'self' wss://sfu.lakko.me; \
         worker-src 'self'; \
         manifest-src 'self'; \
         frame-ancestors 'none'; \
         base-uri 'self'; \
         form-action 'self'; \
         object-src 'none'"
        always;

    # 1 year HSTS — bump to 2y + preload after a quarter of clean
    # renewal runs, then submit hstspreload.org.
    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;

    # Belt-and-suspenders for middleboxes that strip CSP:
    add_header X-Frame-Options              "DENY"        always;
    add_header X-Content-Type-Options       "nosniff"     always;
    add_header Referrer-Policy              "no-referrer" always;
    add_header Cross-Origin-Opener-Policy   "same-origin" always;
    # $lakko_corp is a map in lakko.conf: "same-origin" everywhere except the
    # link preview picture, which other apps' pages may load.
    add_header Cross-Origin-Resource-Policy $lakko_corp always;
    # COEP `unsafe-none` until 1.18+ tells us we want SAB / SIMD —
    # `require-corp` would force every cross-origin sub-resource
    # to opt in via CORP, which we don't have today.
    add_header Cross-Origin-Embedder-Policy "unsafe-none" always;

    # Lock down sensors / DOM features we don't use. Calls DO use the
    # microphone, the camera and media autoplay (remote call audio / video
    # plays without a click), so those three are allowed for this origin
    # only: `=()` there blocked `getUserMedia` outright on the deployed site.
    add_header Permissions-Policy
        "accelerometer=(), \
         ambient-light-sensor=(), \
         autoplay=(self), \
         battery=(), \
         camera=(self), \
         display-capture=(self), \
         fullscreen=(), \
         geolocation=(), \
         gyroscope=(), \
         magnetometer=(), \
         microphone=(self), \
         midi=(), \
         payment=(), \
         picture-in-picture=(), \
         publickey-credentials-get=(self), \
         screen-wake-lock=(), \
         usb=(), \
         xr-spatial-tracking=()" always;
