#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
"""Generate docs/ux/screen-matrix.svg - a Figma-importable wireframe matrix
of every major Lakko screen on web / iOS / Android, with real UI copy read
out of the three clients' sources."""
import html

# ---------------------------------------------------------------- tokens
BG        = "#f6f7fb"
ELEV      = "#ffffff"
SUNKEN    = "#eef0f6"
BORDER    = "#e4e7ef"
FG        = "#1a1d2b"
MUTED     = "#6b7280"
FAINT     = "#9aa1b1"
ACCENT    = "#3730a3"
ACCENT_SOFT = "#e0e7ff"
DANGER    = "#dc2626"
SUCCESS   = "#22c55e"
WARN      = "#b45309"
PAGE      = "#eceef5"
FONT      = "Inter, -apple-system, system-ui, 'Segoe UI', Roboto, sans-serif"

WEB_W, WEB_H = 900, 596
PH_W,  PH_H  = 300, 596
NOTES_W      = 348
GAP          = 28
MARGIN       = 40
ROW_LABEL_H  = 46
ROW_H        = ROW_LABEL_H + max(WEB_H, PH_H) + 44
HEADER_H     = 188

X_WEB = MARGIN
X_IOS = X_WEB + WEB_W + GAP
X_AND = X_IOS + PH_W + GAP
X_NOT = X_AND + PH_W + GAP + 8
TOTAL_W = X_NOT + NOTES_W + MARGIN

out = []
def e(s): return html.escape(str(s), quote=True)
def add(s): out.append(s)

def T(x, y, s, size=13, color=FG, weight=400, anchor="start", opacity=None, italic=False):
    op = f' opacity="{opacity}"' if opacity else ""
    it = ' font-style="italic"' if italic else ""
    add(f'<text x="{x:.1f}" y="{y:.1f}" font-family="{FONT}" font-size="{size}" '
        f'font-weight="{weight}" fill="{color}" text-anchor="{anchor}"{op}{it}>{e(s)}</text>')

def R(x, y, w, h, fill=ELEV, r=0, stroke=None, sw=1, opacity=None, dash=None):
    st = f' stroke="{stroke}" stroke-width="{sw}"' if stroke else ""
    op = f' opacity="{opacity}"' if opacity else ""
    da = f' stroke-dasharray="{dash}"' if dash else ""
    add(f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="{r}" '
        f'fill="{fill}"{st}{op}{da}/>')

def C(cx, cy, rad, fill=ACCENT_SOFT, stroke=None):
    st = f' stroke="{stroke}" stroke-width="1"' if stroke else ""
    add(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="{rad:.1f}" fill="{fill}"{st}/>')

def L(x1, y1, x2, y2, color=BORDER, sw=1):
    add(f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" stroke="{color}" stroke-width="{sw}"/>')

_depth = []
_clips = []
def G(gid, depth=1):
    add(f'<g id="{e(gid)}">'); _depth.append(depth)
def Gend():
    add('</g>' * (_depth.pop() if _depth else 1))
def clip(x, y, w, h, r=14):
    cid = f"clip{len(_clips)}"
    _clips.append(f'<clipPath id="{cid}"><rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" '
                  f'height="{h:.1f}" rx="{r}"/></clipPath>')
    add(f'<g clip-path="url(#{cid})">')
    return cid

def wrap(text, width_chars):
    words, lines, cur = str(text).split(), [], ""
    for w in words:
        if len(cur) + len(w) + 1 <= width_chars:
            cur = (cur + " " + w).strip()
        else:
            lines.append(cur); cur = w
    if cur: lines.append(cur)
    return lines

def paragraph(x, y, text, width_chars, size=12, color=MUTED, lh=16, weight=400):
    for i, ln in enumerate(wrap(text, width_chars)):
        T(x, y + i * lh, ln, size, color, weight)
    return y + max(1, len(wrap(text, width_chars))) * lh

# ------------------------------------------------------------- widgets
def avatar(cx, cy, rad, initials, tint=ACCENT_SOFT, ink=ACCENT):
    C(cx, cy, rad, tint)
    T(cx, cy + rad * 0.36, initials, rad * 0.9, ink, 600, "middle")

def pill(x, y, label, fill=ACCENT, ink="#ffffff", h=22, pad=10, size=11, weight=500, r=11):
    w = len(label) * size * 0.58 + pad * 2
    R(x, y, w, h, fill, r)
    T(x + w / 2, y + h * 0.68, label, size, ink, weight, "middle")
    return w

def button(x, y, w, label, variant="primary", h=34):
    if variant == "primary":
        R(x, y, w, h, ACCENT, 8); T(x + w / 2, y + h * 0.63, label, 13, "#ffffff", 500, "middle")
    elif variant == "secondary":
        R(x, y, w, h, ELEV, 8, BORDER); T(x + w / 2, y + h * 0.63, label, 13, FG, 500, "middle")
    elif variant == "danger":
        R(x, y, w, h, ELEV, 8, DANGER); T(x + w / 2, y + h * 0.63, label, 13, DANGER, 500, "middle")
    else:
        T(x + w / 2, y + h * 0.63, label, 13, ACCENT, 500, "middle")
    return y + h

def field(x, y, w, label, placeholder, h=34):
    T(x, y, label, 11, MUTED, 500)
    R(x, y + 6, w, h, ELEV, 8, BORDER)
    T(x + 10, y + 6 + h * 0.63, placeholder, 12, FAINT)
    return y + 6 + h

def search(x, y, w, placeholder, h=30):
    R(x, y, w, h, SUNKEN, 15)
    C(x + 15, y + h / 2, 4.5, "none", FAINT)
    L(x + 18, y + h / 2 + 3, x + 22, y + h / 2 + 7, FAINT, 1.4)
    T(x + 28, y + h * 0.65, placeholder, 12, FAINT)
    return y + h

def listrow(x, y, w, title, sub=None, meta=None, badge=None, initials="A",
            h=56, selected=False, tint=ACCENT_SOFT, ink=ACCENT, show_avatar=True):
    if selected: R(x, y, w, h, ACCENT_SOFT, 8, opacity="0.55")
    if show_avatar:
        avatar(x + 26, y + h / 2, 16, initials, tint, ink); tx = x + 52
    else:
        tx = x + 12
    T(tx, y + (h / 2 - 3 if sub else h / 2 + 4), title, 13.5, FG, 500)
    if sub: T(tx, y + h / 2 + 13, sub, 11.5, MUTED)
    if meta: T(x + w - 12, y + h / 2 - 3, meta, 10.5, FAINT, 400, "end")
    if badge: pill(x + w - 34, y + h / 2 + 3, badge, ACCENT, "#ffffff", 17, 6, 10, 600, 9)
    return y + h

def settingsrow(x, y, w, title, value=None, chevron=True, h=42, danger=False):
    T(x + 12, y + h * 0.62, title, 13, DANGER if danger else FG, 500)
    if value: T(x + w - (24 if chevron else 12), y + h * 0.62, value, 11.5, MUTED, 400, "end")
    if chevron:
        L(x + w - 17, y + h / 2 - 4, x + w - 13, y + h / 2, FAINT, 1.4)
        L(x + w - 17, y + h / 2 + 4, x + w - 13, y + h / 2, FAINT, 1.4)
    return y + h

def sectionlabel(x, y, text, color=MUTED):
    T(x + 12, y + 12, text.upper(), 9.5, color, 600)
    return y + 22

def bubble(x, y, w, text, side="in", meta=None, chars=34):
    lines = wrap(text, chars)
    bw = min(w * 0.68, max(len(l) for l in lines) * 6.6 + 26)
    bh = len(lines) * 16 + 18
    bx = x if side == "in" else x + w - bw
    R(bx, y, bw, bh, SUNKEN if side == "in" else ACCENT, 14)
    for i, ln in enumerate(lines):
        T(bx + 12, y + 17 + i * 16, ln, 12.5, FG if side == "in" else "#ffffff")
    if meta:
        T(bx + bw - 10, y + bh - 6, meta, 9, FAINT if side == "in" else "#dfe3ff", 400, "end")
    return y + bh + 8

def composer(x, y, w, h=44, placeholder="Message", mic=True):
    R(x, y, w, h, ELEV); L(x, y, x + w, y, BORDER)
    C(x + 20, y + h / 2, 9, SUNKEN); T(x + 20, y + h / 2 + 4, "+", 13, MUTED, 600, "middle")
    R(x + 36, y + 9, w - 96, h - 18, SUNKEN, 13)
    T(x + 46, y + h / 2 + 4, placeholder, 12, FAINT)
    C(x + w - 24, y + h / 2, 13, ACCENT)
    T(x + w - 24, y + h / 2 + 4, "▶" if not mic else "▶", 10, "#ffffff", 400, "middle")
    return y + h

RAIL = [("Chats", "chat"), ("Groups", "groups"), ("Channels", "channels"),
        ("Contacts", "contacts"), ("Settings", "settings")]

def glyph(cx, cy, kind, color):
    if kind == "chat":
        R(cx - 7, cy - 6, 14, 11, "none", 3, color, 1.6)
        add(f'<path d="M{cx-3:.1f} {cy+5:.1f} L{cx-1:.1f} {cy+9:.1f} L{cx+2:.1f} {cy+5:.1f}" fill="none" stroke="{color}" stroke-width="1.6"/>')
    elif kind == "groups":
        C(cx - 4, cy - 3, 3.2, "none", color); C(cx + 4, cy - 3, 3.2, "none", color)
        add(f'<path d="M{cx-9:.1f} {cy+6:.1f} a5 5 0 0 1 10 0" fill="none" stroke="{color}" stroke-width="1.5"/>')
        add(f'<path d="M{cx-1:.1f} {cy+6:.1f} a5 5 0 0 1 10 0" fill="none" stroke="{color}" stroke-width="1.5"/>')
    elif kind == "channels":
        add(f'<path d="M{cx-7:.1f} {cy+2:.1f} L{cx+6:.1f} {cy-6:.1f} L{cx+6:.1f} {cy+7:.1f} Z" fill="none" stroke="{color}" stroke-width="1.6"/>')
    elif kind == "contacts":
        C(cx, cy - 4, 3.4, "none", color)
        add(f'<path d="M{cx-6:.1f} {cy+7:.1f} a6 6 0 0 1 12 0" fill="none" stroke="{color}" stroke-width="1.6"/>')
    elif kind == "settings":
        C(cx, cy, 4.2, "none", color); C(cx, cy, 7.5, "none", color)
    elif kind == "call":
        add(f'<path d="M{cx-6:.1f} {cy-6:.1f} q10 12 12 12 l2-3 4 3 q-8 8 -18 -2 q-10 -10 -2 -18 l3 4 z" fill="{color}"/>')
    elif kind == "video":
        R(cx - 7, cy - 5, 10, 10, "none", 2, color, 1.6)
        add(f'<path d="M{cx+4:.1f} {cy:.1f} l5-3 v6 z" fill="{color}"/>')
    elif kind == "back":
        L(cx + 3, cy - 5, cx - 3, cy, color, 1.7); L(cx + 3, cy + 5, cx - 3, cy, color, 1.7)
    elif kind == "more":
        for d in (-5, 0, 5): C(cx, cy + d, 1.5, color)
    elif kind == "plus":
        L(cx - 5, cy, cx + 5, cy, color, 1.7); L(cx, cy - 5, cx, cy + 5, color, 1.7)
    elif kind == "link":
        R(cx - 7, cy - 3, 8, 6, "none", 3, color, 1.5); R(cx - 1, cy - 3, 8, 6, "none", 3, color, 1.5)

def navrail(x, y, h, active):
    R(x, y, 72, h, ELEV); L(x + 72, y, x + 72, y + h, BORDER)
    R(x + 16, y + 16, 40, 40, ACCENT, 10)
    T(x + 36, y + 43, "L", 22, "#ffffff", 700, "middle")
    cy = y + 84
    for label, ic in RAIL:
        on = label == active
        if on: R(x + 12, cy - 4, 48, 42, ACCENT_SOFT, 10)
        glyph(x + 36, cy + 8, ic, ACCENT if on else MUTED)
        T(x + 36, cy + 30, label, 8.5, ACCENT if on else MUTED, 600 if on else 400, "middle")
        cy += 52
    avatar(x + 36, y + h - 30, 14, "A")

def tabbar(x, y, w, active):
    R(x, y, w, 52, ELEV); L(x, y, x + w, y, BORDER)
    step = w / 5
    for i, (label, ic) in enumerate(RAIL):
        cx = x + step * (i + 0.5); on = label == active
        glyph(cx, y + 18, ic, ACCENT if on else FAINT)
        T(cx, y + 40, label, 8, ACCENT if on else FAINT, 600 if on else 400, "middle")
    return y + 52

def statusbar(x, y, w):
    T(x + 14, y + 13, "9:41", 9.5, FG, 600)
    for i, bw in enumerate((3, 4, 5, 6)):
        R(x + w - 52 + i * 5, y + 11 - bw, 3, bw, FG, 1)
    R(x + w - 26, y + 4, 16, 8, "none", 2, FG, 1)
    return y + 18

def topbar(x, y, w, title, left=None, right=(), h=44, big=False):
    R(x, y, w, h, ELEV); L(x, y + h, x + w, y + h, BORDER)
    if left: glyph(x + 18, y + h / 2, left, ACCENT)
    T(x + (34 if left else 16) if big else x + w / 2, y + h * 0.62,
      title, 15 if big else 14, FG, 600, "start" if big else "middle")
    rx = x + w - 20
    for ic in reversed(right):
        glyph(rx, y + h / 2, ic, ACCENT); rx -= 30
    return y + h

def phone_frame(x, y, name, chrome_title=None, left=None, right=(), tab=None, big_title=False, bg=BG):
    G(name, 2)
    clip(x, y, PH_W, PH_H, 18)
    R(x, y, PH_W, PH_H, bg, 18, BORDER)
    cy = statusbar(x, y, PH_W)
    if chrome_title is not None:
        cy = topbar(x, cy, PH_W, chrome_title, left, right, big=big_title)
    bottom = y + PH_H - (52 if tab else 0)
    return cy, bottom

def web_frame(x, y, name, active=None):
    G(name, 2)
    clip(x, y, WEB_W, WEB_H, 14)
    R(x, y, WEB_W, WEB_H, BG, 14, BORDER)
    R(x, y, WEB_W, 26, SUNKEN, 14)
    R(x, y + 14, WEB_W, 12, SUNKEN)
    for i, col in enumerate(("#f87171", "#fbbf24", "#34d399")):
        C(x + 16 + i * 13, y + 13, 4.2, col)
    R(x + 60, y + 6, 240, 14, ELEV, 7)
    T(x + 70, y + 16, "lakko.me", 9, FAINT)
    if active: navrail(x, y + 26, WEB_H - 26, active)
    return (x + (72 if active else 0), y + 26, WEB_W - (72 if active else 0), WEB_H - 26)

def auth_split(x, y, w, h, headline, lede):
    """Gradient brand panel + card, the shared auth shell."""
    bw = int(w * 0.42)
    add(f'<rect x="{x:.1f}" y="{y:.1f}" width="{bw}" height="{h}" fill="url(#brandGrad)"/>')
    R(x + bw, y, w - bw, h, "#eef2ff")
    R(x + 40, y + 40, 56, 56, "#ffffff", 14, opacity="0.18")
    T(x + 68, y + 79, "L", 30, "#ffffff", 700, "middle")
    yy = y + h / 2 - 10
    for i, ln in enumerate(wrap(headline, 22)):
        T(x + 40, yy + i * 26, ln, 21, "#ffffff", 700)
    paragraph(x + 40, yy + len(wrap(headline, 22)) * 26 + 14, lede, 34, 12, "#dfe3ff")
    cw = min(360, w - bw - 60)
    cx = x + bw + (w - bw - cw) / 2
    return cx, cw

# --------------------------------------------------------------- rows
ROWS = []
def row(title, note):
    ROWS.append({"title": title, "note": note})

def render_all():
    y = HEADER_H
    for i, spec in enumerate(ROWS):
        ry = y + i * ROW_H
        R(MARGIN - 16, ry - 10, TOTAL_W - 2 * MARGIN + 32, ROW_H - 18, ELEV, 14, opacity="0.5")
        T(MARGIN, ry + 18, f"{i + 1}.  {spec['title']}", 17, FG, 600)
        L(MARGIN, ry + 30, TOTAL_W - MARGIN, ry + 30, BORDER)
        py = ry + ROW_LABEL_H
        spec["draw"](py)
        # notes
        ny = py + 8
        T(X_NOT, ny, "PARITY", 9.5, FAINT, 600); ny += 18
        for block in spec["note"]:
            if block.startswith("+"):
                col, txt = SUCCESS, block[1:].strip()
            elif block.startswith("!"):
                col, txt = DANGER, block[1:].strip()
            elif block.startswith("~"):
                col, txt = WARN, block[1:].strip()
            else:
                col, txt = MUTED, block
            C(X_NOT + 4, ny - 4, 3, col)
            ny = paragraph(X_NOT + 14, ny, txt, 44, 11.5, FG if col != MUTED else MUTED, 15) + 8

# ============================================================ ROW 1 welcome
def r_welcome(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__no-welcome")
    R(cx, cy, cw, ch, BG)
    T(cx + cw / 2, cy + ch / 2 - 40, "no Welcome screen on web", 15, FAINT, 600, "middle", italic=True)
    paragraph(cx + cw / 2 - 150, cy + ch / 2 - 16,
              "An unauthenticated visit to / redirects straight to Log in. "
              "The brand headline the phones show on first run has nowhere to live.",
              48, 12, MUTED, 16)
    R(cx + cw / 2 - 110, cy + ch / 2 + 44, 220, 34, ELEV, 8, BORDER, dash="4 3")
    T(cx + cw / 2, cy + ch / 2 + 66, "/login", 12, FAINT, 400, "middle")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__welcome", bg="#ffffff")
        add(f'<rect x="{x:.1f}" y="{top:.1f}" width="{PH_W}" height="{bot - top}" fill="url(#brandGrad)" opacity="0.10"/>')
        R(x + PH_W / 2 - 34, top + 74, 68, 68, ACCENT, 16)
        T(x + PH_W / 2, top + 122, "L", 36, "#ffffff", 700, "middle")
        T(x + PH_W / 2, top + 176, "Lakko", 26, FG, 700, "middle")
        paragraph(x + 44, top + 206, "Messages that stay between you.", 26, 13, MUTED, 18)
        button(x + 28, bot - 112, PH_W - 56, "Create account", "primary", 40)
        button(x + 28, bot - 62, PH_W - 56, "Log in", "secondary", 40)
        Gend()

row("First run / Welcome", [
    "+ iOS and Android are identical: logo, 'Lakko', 'Messages that stay between you.', Create account / Log in.",
    "! Web has no Welcome screen at all - / goes to Log in. The product's one-line promise is never shown on web.",
    "~ Fix is cheap: reuse AuthLayout's gradient panel for a public landing at /.",
])
ROWS[-1]["draw"] = r_welcome

# ============================================================ ROW 2 login
def r_login(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__login")
    bx, bw = auth_split(cx, cy, cw, ch, "Welcome back", "Log in to continue.")
    R(bx, cy + 70, bw, ch - 140, ELEV, 12, BORDER)
    yy = cy + 96
    T(bx + 24, yy, "Welcome back", 19, FG, 600); yy += 20
    T(bx + 24, yy, "Log in to continue.", 12, MUTED); yy += 28
    yy = field(bx + 24, yy, bw - 48, "Username", "") + 16
    yy = field(bx + 24, yy, bw - 48, "Password", "") + 8
    T(bx + bw - 24, yy + 10, "Forgot password?", 11.5, ACCENT, 500, "end"); yy += 24
    yy = button(bx + 24, yy, bw - 48, "Log in") + 10
    yy = button(bx + 24, yy, bw - 48, "Use passkey", "secondary") + 20
    R(bx + 24, yy - 8, bw - 48, 30, SUNKEN, 8)
    T(bx + 36, yy + 12, "How Lakko protects your password", 11.5, ACCENT, 500); yy += 38
    T(bx + bw / 2, yy, "No account? Create account", 11.5, MUTED, 400, "middle")
    Gend()

    for x, plat, extra in ((X_IOS, "ios", True), (X_AND, "android", True)):
        top, bot = phone_frame(x, y, f"{plat}__login", "Log in" if plat == "android" else None, bg=ELEV)
        yy = top + (36 if plat == "ios" else 24)
        if plat == "ios":
            R(x + PH_W / 2 - 26, yy, 52, 52, ACCENT, 12)
            T(x + PH_W / 2, yy + 37, "L", 26, "#ffffff", 700, "middle")
            yy += 72
            T(x + 24, yy, "Welcome back", 20, FG, 700); yy += 30
        yy = field(x + 24, yy, PH_W - 48, "Username", "") + 16
        yy = field(x + 24, yy, PH_W - 48, "Password", "") + 8
        T(x + PH_W - 24, yy + 12, "Forgot password?", 11.5, ACCENT, 500, "end"); yy += 28
        yy = button(x + 24, yy, PH_W - 48, "Log in", "primary", 40) + 10
        yy = button(x + 24, yy, PH_W - 48, "Use passkey", "secondary", 40) + 20
        R(x + 20, yy, PH_W - 40, 44, SUNKEN, 10)
        T(x + 32, yy + 27, "How Lakko protects your password", 11.5, ACCENT, 500)
        if plat == "ios":
            T(x + PH_W / 2, bot - 22, "New to Lakko?  Create account", 11.5, MUTED, 400, "middle")
        Gend()

row("Log in", [
    "+ Same four controls everywhere: Username, Password, Log in, Use passkey, plus 'Forgot password?'.",
    "+ The 'How Lakko protects your password' explainer is on all three (web carries it on both Log in and Create account).",
    "~ Web shows 'Create account' in the footer, iOS shows 'New to Lakko?', Android shows nothing - three answers to one question.",
])
ROWS[-1]["draw"] = r_login

# ============================================================ ROW 3 signup
# Copy below is the English catalog (l10n/catalog/en.json) as of 2026-10-02.
SIGNUP_LEDE = "Your password never leaves your device. You will get a recovery phrase to write down next."
USERNAME_HINT = "Latin letters, digits and _. Others find you by this name."
NAME_HINT = "Shown to your contacts. Leave it empty to show your username."
PASSWORD_HINT = "At least 10 characters."

def r_signup(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__signup")
    bx, bw = auth_split(cx, cy, cw, ch, "Create your account", SIGNUP_LEDE)
    R(bx, cy + 54, bw, ch - 108, ELEV, 12, BORDER)
    yy = cy + 80
    T(bx + 24, yy, "Create your account", 18, FG, 600); yy += 18
    yy = paragraph(bx + 24, yy, SIGNUP_LEDE, 46, 11.5, MUTED, 15) + 12
    yy = field(bx + 24, yy, bw - 48, "Username", "") + 6
    T(bx + 24, yy + 12, "@alice", 11, ACCENT); yy += 30
    yy = field(bx + 24, yy, bw - 48, "Name (optional)", "alice") + 16
    yy = field(bx + 24, yy, bw - 48, "Password", "") + 16
    yy = field(bx + 24, yy, bw - 48, "Confirm password", "") + 18
    yy = button(bx + 24, yy, bw - 48, "Create account") + 12
    R(bx + 24, yy, bw - 48, 28, SUNKEN, 8)
    T(bx + 36, yy + 18, "How Lakko protects your password", 11, ACCENT, 500); yy += 40
    T(bx + bw / 2, yy, "Already have an account? Log in", 11.5, MUTED, 400, "middle")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        if plat == "android":
            top, bot = phone_frame(x, y, f"{plat}__signup", "Create your account", "back", bg=ELEV)
            yy = top + 20
        else:
            top, bot = phone_frame(x, y, f"{plat}__signup", "", "back", bg=ELEV)
            yy = top + 28
            T(x + 20, yy, "Create your account", 19, FG, 700); yy += 18
            yy = paragraph(x + 20, yy, SIGNUP_LEDE, 46, 10.5, MUTED, 14) + 10
        yy = field(x + 20, yy, PH_W - 40, "Username", "") + 6
        yy = paragraph(x + 20, yy + 12, USERNAME_HINT, 46, 10, FAINT, 13) + 10
        yy = field(x + 20, yy, PH_W - 40, "Name (optional)", "Optional" if plat == "android" else "") + 6
        yy = paragraph(x + 20, yy + 12, NAME_HINT, 46, 10, FAINT, 13) + 10
        yy = field(x + 20, yy, PH_W - 40, "Password", "") + 6
        T(x + 20, yy + 12, PASSWORD_HINT, 10, FAINT); yy += 28
        yy = field(x + 20, yy, PH_W - 40, "Confirm password", "") + 18
        yy = button(x + 20, yy, PH_W - 40, "Create account", "primary", 40) + 16
        if plat == "ios":
            T(x + PH_W / 2, yy + 4, "Already have an account?  Log in", 11, MUTED, 400, "middle")
        else:
            paragraph(x + 20, yy + 4, SIGNUP_LEDE, 46, 10.5, MUTED, 14)
        Gend()

row("Create account", [
    "+ Field order, labels and hints are the same on all three and come from one catalog: Username, 'Name (optional)', Password, Confirm password, Create account.",
    "~ Web shows no username rule up front: it echoes the handle live and speaks only when the name is rejected. The phones show the rule sentence under the field from the start.",
    "~ The sentence that announces the recovery phrase sits under the title on web and iOS, and below the button on Android.",
    "~ 'How Lakko protects your password' is on the web form only; 'Already have an account? Log in' is on web and iOS, Android has the back arrow instead.",
    "+ Nothing about recovery is chosen on this form any more: the phrase, its check and the passkey offer follow as steps 1 to 3.",
])
ROWS[-1]["draw"] = r_signup

# ============================================================ recovery widgets
PHRASE_WORDS = ["absorb", "canyon", "detail", "engine", "forest", "gravel",
                "harbor", "island", "jacket", "kernel", "lumber", "magnet"]

def phrase_grid(x, gy, w, cols, cell_h=26, size=10.5):
    """The 12 numbered words on the sunken surface, numbered row by row:
    3 columns x 4 rows at phone width, 4 x 3 on a wide screen."""
    rows_ = (len(PHRASE_WORDS) + cols - 1) // cols
    h = rows_ * cell_h + 14
    R(x, gy, w, h, SUNKEN, 10)
    cw = (w - 16) / cols
    for i, wd in enumerate(PHRASE_WORDS):
        r_, c_ = divmod(i, cols)
        gx, gyy = x + 8 + c_ * cw, gy + 7 + r_ * cell_h
        T(gx + 16, gyy + cell_h * 0.64, str(i + 1), size - 1.5, FAINT, 600, "end")
        T(gx + 22, gyy + cell_h * 0.64, wd, size, FG, 500)
    return gy + h

def step_label(x, y, current, total):
    T(x, y, f"Step {current} of {total}", 10.5, MUTED, 600)
    return y + 16

def group_label(x, y, text):
    T(x, y + 10, text, 10.5, MUTED, 600)
    return y + 18

def methodrow(x, y, w, title, sub, chars):
    """One row of a method list: icon, title, wrapped subtitle, chevron."""
    lines = wrap(sub, chars)
    h = 26 + len(lines) * 12 + 8
    R(x, y, w, h, ELEV, 10, BORDER)
    C(x + 20, y + h / 2, 11, ACCENT_SOFT)
    C(x + 20, y + h / 2, 3.5, ACCENT)
    T(x + 40, y + 19, title, 12.5, FG, 600)
    for i, ln in enumerate(lines):
        T(x + 40, y + 33 + i * 12, ln, 9.8, MUTED)
    L(x + w - 16, y + h / 2 - 4, x + w - 12, y + h / 2, FAINT, 1.4)
    L(x + w - 16, y + h / 2 + 4, x + w - 12, y + h / 2, FAINT, 1.4)
    return y + h + 6

def then_box(x, y, w, caption, lines_, chars):
    """A dashed 'what comes next / other state' box under a form."""
    body = []
    for ln in lines_:
        body.extend(wrap(ln, chars))
    h = 22 + len(body) * 13 + 6
    R(x, y, w, h, "none", 8, FAINT, dash="4 3")
    T(x + 10, y + 15, caption.upper(), 8.5, FAINT, 600)
    for i, ln in enumerate(body):
        T(x + 10, y + 30 + i * 13, ln, 10, MUTED)
    return y + h

# ============================================================ sign-up step 1: the phrase
PHRASE_TITLE = "Save your recovery phrase"
PHRASE_LEDE = ("These 12 words bring your account back on a new device if you lose your password "
               "and your devices. Write them down in order and keep them somewhere safe.")
PHRASE_ANYONE = "Anyone who has these words can read your messages and act as you. Do not share them."
PHRASE_NO_SHOT = "Do not keep them as a screenshot or in a plain text file. Lakko cannot show them again."

def r_phrase(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__signup-phrase")
    bx, bw = auth_split(cx, cy, cw, ch, PHRASE_TITLE, PHRASE_LEDE)
    R(bx, cy + 44, bw, ch - 88, ELEV, 12, BORDER)
    yy = cy + 74
    T(bx + 20, yy, PHRASE_TITLE, 17, FG, 600); yy += 20
    yy = step_label(bx + 20, yy, 1, 3) + 4
    yy = paragraph(bx + 20, yy, PHRASE_LEDE, 56, 11, MUTED, 14) + 6
    yy = phrase_grid(bx + 20, yy, bw - 40, 4, 28) + 10
    button(bx + 20, yy, 70, "Copy", "secondary", 26); yy += 44
    yy = paragraph(bx + 20, yy, PHRASE_NO_SHOT, 56, 11, MUTED, 14) + 8
    button(bx + 20, yy, bw - 40, "I have written them down")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        if plat == "android":
            top, bot = phone_frame(x, y, f"{plat}__signup-phrase", PHRASE_TITLE, bg=ELEV)
            yy = top + 22
            yy = step_label(x + 18, yy, 1, 3) + 2
        else:
            top, bot = phone_frame(x, y, f"{plat}__signup-phrase", bg=ELEV)
            yy = top + 30
            yy = step_label(x + 18, yy, 1, 3) + 8
            T(x + 18, yy, PHRASE_TITLE, 18, FG, 700); yy += 20
        yy = paragraph(x + 18, yy, PHRASE_LEDE, 48, 10.5, MUTED, 14) + 4
        yy = phrase_grid(x + 18, yy, PH_W - 36, 3, 27) + 10
        yy = button(x + 18, yy, PH_W - 36, "Copy", "secondary", 32) + 18
        if plat == "android":
            yy = paragraph(x + 18, yy, PHRASE_ANYONE, 48, 10.5, MUTED, 14) + 6
            paragraph(x + 18, yy, PHRASE_NO_SHOT, 48, 10.5, MUTED, 14)
        else:
            R(x + 18, yy - 14, PH_W - 36, 62, ELEV, 10, BORDER)
            paragraph(x + 30, yy + 4, PHRASE_NO_SHOT, 44, 10.5, MUTED, 14)
        button(x + 18, bot - 58, PH_W - 36,
               "Continue" if plat == "android" else "I have written them down", "primary", 40)
        Gend()

row("Sign-up, step 1: the recovery phrase", [
    "+ 12 numbered words on all three, numbered row by row: 3 columns x 4 rows at phone width, 4 x 3 on a wide screen (the web card shown here). Display, the check and typing the phrase back all use this one order.",
    "+ Same title, 'Step 1 of 3', lede and Copy on all three. No shares, no 'More options': the phrase is the only thing to save.",
    "~ The button that leaves this step reads 'I have written them down' on web and iOS and 'Continue' on Android.",
    "~ Android shows two warning sentences ('Anyone who has these words...' and the screenshot one); web and iOS show the screenshot sentence only.",
    "~ Step 2 (not drawn) asks for three words at random positions. Web adds an 'I understand that losing these words...' checkbox before Continue; the phones do not have it.",
])
ROWS[-1]["draw"] = r_phrase

# ============================================================ sign-up step 3: the passkey offer
PASSKEY_LEDE = "Add a passkey so you can sign in with your face, fingerprint or screen lock."
KEYS_CHOICE = ["Let this passkey restore your keys?", "Turn on  /  Sign-in only"]

def r_signup_passkey(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__signup-passkey")
    bx, bw = auth_split(cx, cy, cw, ch, "Add a passkey", PASSKEY_LEDE)
    R(bx, cy + 110, bw, ch - 220, ELEV, 12, BORDER)
    yy = cy + 142
    T(bx + 24, yy, "Add a passkey", 18, FG, 600); yy += 20
    yy = step_label(bx + 24, yy, 3, 3) + 4
    yy = paragraph(bx + 24, yy, PASSKEY_LEDE, 52, 11.5, MUTED, 15) + 8
    yy = button(bx + 24, yy, bw - 48, "Add a passkey") + 6
    yy = button(bx + 24, yy, bw - 48, "Not now", "ghost") + 14
    then_box(bx + 24, yy, bw - 48, "After the passkey is added", KEYS_CHOICE, 50)
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        if plat == "android":
            top, bot = phone_frame(x, y, f"{plat}__signup-passkey", "Add a passkey", bg=ELEV)
            yy = top + 22
            yy = step_label(x + 20, yy, 3, 3) + 4
        else:
            top, bot = phone_frame(x, y, f"{plat}__signup-passkey", bg=ELEV)
            yy = top + 30
            yy = step_label(x + 20, yy, 3, 3) + 8
            T(x + 20, yy, "Add a passkey", 19, FG, 700); yy += 22
        yy = paragraph(x + 20, yy, PASSKEY_LEDE, 44, 11, MUTED, 15) + 10
        yy = button(x + 20, yy, PH_W - 40, "Add a passkey", "primary", 40) + 8
        yy = button(x + 20, yy, PH_W - 40, "Not now", "secondary", 40) + 22
        then_box(x + 20, yy, PH_W - 40, "After the passkey is added", KEYS_CHOICE, 40)
        Gend()

row("Sign-up, step 3: add a passkey", [
    "+ Same step on all three: 'Add a passkey' or 'Not now'. It never blocks sign-up, and an account without a username is not offered it (the flow is then two steps).",
    "+ Adding a passkey stores nothing by itself. Where the platform can protect keys with it, the opt-in question 'Let this passkey restore your keys?' follows; otherwise a line says the passkey signs in only.",
    "~ Web offers the step only in a browser that supports passkeys; the phones offer it to every account with a username.",
    "~ 'Not now' is a text button on web and a full-width secondary button on the phones.",
])
ROWS[-1]["draw"] = r_signup_passkey

# ============================================================ recovery: the list of ways back
RECOVER_KEYS = [("Another device", "If you have one that is still signed in."),
                ("Passkey", "Signs you in. Brings back your chats too if you turned that on for this passkey."),
                ("Recovery phrase", "Type your 12 words and choose a new password.")]
RECOVER_SIGNIN = [("Three words from your phrase", "Type three words of your recovery phrase. This device checks them."),
                  ("Authenticator code", "A code from your authenticator app.")]
RECOVER_NOTE = ("Three words or an authenticator code sign you in. On a device that has never held "
                "this account you need another device, a passkey or your recovery phrase.")

def r_recover(y):
    def groups(x, yy, w, chars):
        yy = group_label(x, yy, "Brings back your chats")
        for t_, s_ in RECOVER_KEYS:
            yy = methodrow(x, yy, w, t_, s_, chars)
        yy = group_label(x, yy + 4, "Signs you in on this device")
        for t_, s_ in RECOVER_SIGNIN:
            yy = methodrow(x, yy, w, t_, s_, chars)
        return yy

    cx, cy, cw, ch = web_frame(X_WEB, y, "web__recover")
    bx, bw = auth_split(cx, cy, cw, ch, "Get back in", "Choose how.")
    R(bx, cy + 14, bw, ch - 28, ELEV, 12, BORDER)
    yy = cy + 42
    T(bx + 20, yy, "Get back in", 18, FG, 600); yy += 18
    T(bx + 20, yy, "Choose how.", 11.5, MUTED); yy += 12
    yy = groups(bx + 20, yy, bw - 40, 52) + 10
    yy = paragraph(bx + 20, yy, RECOVER_NOTE, 58, 10.5, MUTED, 13) + 8
    T(bx + bw / 2, yy, "Remembered your password? Log in", 11, MUTED, 400, "middle")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        if plat == "android":
            top, bot = phone_frame(x, y, f"{plat}__recover", "Get back in", "back", bg=ELEV)
            yy = top + 22
            T(x + 16, yy, "Choose how to get back into @anna.", 11, MUTED); yy += 12
        else:
            top, bot = phone_frame(x, y, f"{plat}__recover", "", "back", bg=ELEV)
            yy = top + 28
            T(x + 16, yy, "Get back in", 19, FG, 700); yy += 18
            T(x + 16, yy, "Choose how.", 11, MUTED); yy += 10
        yy = groups(x + 16, yy, PH_W - 32, 42) + 8
        yy = paragraph(x + 16, yy, RECOVER_NOTE, 50, 10, MUTED, 13) + 6
        if plat == "ios":
            T(x + PH_W / 2, yy, "Remembered your password?  Log in", 10.5, MUTED, 400, "middle")
            T(x + PH_W / 2, yy + 16, "New to Lakko?  Create account", 10.5, MUTED, 400, "middle")
        Gend()

row("Recovery: the ways back in", [
    "+ Same two groups, same rows, same order and same sentences on all three. 'Brings back your chats': Another device (first), Passkey, Recovery phrase. 'Signs you in on this device': Three words from your phrase, Authenticator code.",
    "+ No row is hidden or badged from server state. The one conditional row is local: 'Three words from your phrase' is listed only on a device that holds the phrase and the keys of its cached account, and the note under the list names three words only when the row is there.",
    "~ The line under the title is 'Choose how.' on web and iOS; Android names the cached account ('Choose how to get back into @anna.').",
    "~ Footer: web has 'Remembered your password? Log in'; iOS adds 'New to Lakko? Create account'; Android has neither, only the back arrow.",
    "! 'Another device' is not the same act everywhere: web asks for the username and a six-digit code typed from the signed-in device; iOS and Android show a QR code for the other device to scan.",
])
ROWS[-1]["draw"] = r_recover

# ============================================================ recovery: three words
WORDS_POSITIONS = (7, 3, 12)
WORDS_PROMPT = "Type words 7, 3 and 12 of your recovery phrase."
WORDS_SIGNIN_ONLY = "This device checks the words itself and signs you in. You then choose a new password."
WORDS_NO_MATCH = "Those words did not match."
WORDS_TRIES = "4 tries left before a 15-minute pause."
WORDS_PAUSED = ["Too many wrong tries. Try again in 14:59."]

def r_three_words(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__recover-three-words")
    bx, bw = auth_split(cx, cy, cw, ch, "Three words from your phrase",
                        "Type three words of your recovery phrase. This device checks them.")
    R(bx, cy + 50, bw, ch - 100, ELEV, 12, BORDER)
    yy = cy + 82
    glyph(bx + 28, yy - 5, "back", ACCENT)
    T(bx + 46, yy, "Three words from your phrase", 15, FG, 600); yy += 26
    T(bx + 20, yy, "@anna on this device", 11.5, MUTED); yy += 22
    T(bx + 20, yy, WORDS_PROMPT, 12, FG); yy += 12
    sw = (bw - 40 - 20) / 3
    for i, pos in enumerate(WORDS_POSITIONS):
        sx = bx + 20 + i * (sw + 10)
        R(sx, yy, sw, 46, ELEV, 8, BORDER)
        T(sx + 9, yy + 15, str(pos), 10, ACCENT, 600)
        T(sx + 9, yy + 34, f"word {pos}", 11.5, FAINT)
    yy += 62
    yy = paragraph(bx + 20, yy, WORDS_SIGNIN_ONLY, 56, 11, MUTED, 14) + 6
    T(bx + 20, yy, WORDS_NO_MATCH, 11.5, DANGER, 500); yy += 18
    T(bx + 20, yy, WORDS_TRIES, 11, MUTED); yy += 14
    yy = button(bx + 20, yy, bw - 40, "Sign in") + 14
    yy = then_box(bx + 20, yy, bw - 40, "After the fifth miss", WORDS_PAUSED, 54) + 22
    T(bx + bw / 2, yy, "Remembered your password? Log in", 11, MUTED, 400, "middle")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        if plat == "android":
            top, bot = phone_frame(x, y, f"{plat}__recover-three-words", "Three words from your phrase", "back", bg=ELEV)
            yy = top + 24
        else:
            top, bot = phone_frame(x, y, f"{plat}__recover-three-words", "", "back", bg=ELEV)
            yy = top + 28
            T(x + 20, yy, "Three words from your phrase", 16.5, FG, 700); yy += 16
            yy = paragraph(x + 20, yy, "Type three words of your recovery phrase. This device checks them.", 46, 10.5, MUTED, 14) + 6
        T(x + 20, yy, "@anna on this device", 11.5, FG if plat == "ios" else MUTED, 500); yy += 20
        T(x + 20, yy, WORDS_PROMPT, 11, FG); yy += 22
        for pos in WORDS_POSITIONS:
            yy = field(x + 20, yy, PH_W - 40, f"Word {pos}", "", 32) + 16
        yy = paragraph(x + 20, yy - 2, WORDS_SIGNIN_ONLY, 48, 10.5, MUTED, 14) + 4
        T(x + 20, yy, WORDS_NO_MATCH, 11, DANGER, 500); yy += 16
        T(x + 20, yy, WORDS_TRIES, 10.5, MUTED); yy += 12
        yy = button(x + 20, yy, PH_W - 40, "Sign in", "primary", 38) + 12
        then_box(x + 20, yy, PH_W - 40, "After the fifth miss", WORDS_PAUSED, 44)
        Gend()

row("Recovery: three words", [
    "+ Same form on all three: no username field (the cached account is named), three numbered fields at positions drawn at random and asked in the drawn order, 'Sign in'. The device checks the words itself; nothing is sent until they match.",
    "+ Same budget on all three: a miss draws new positions, clears the fields and shows the tries left; the fifth miss starts a 15-minute pause with a countdown, and the count survives a restart.",
    "+ It signs in only, and says so on the form. 'Choose a new password' follows on all three.",
    "~ Web lays the three fields out in one row, each with its number inside and the placeholder 'word 7'; the phones stack three labelled fields ('Word 7').",
    "~ iOS repeats the row's subtitle under the title; web and Android show the title alone.",
])
ROWS[-1]["draw"] = r_three_words

# ============================================================ ROW 5 chats
def r_chats(y):
    chats = [("Anna Weber", "You: see you at 7", "12:04", "2", "AW"),
             ("Ivan Petrov", "Anna: sent a photo", "11:20", None, "IP"),
             ("Maria", "Typing...", "Tue", None, "M"),
             ("Weekend plans", "Ivan: I'm in", "Mon", "5", "WP"),
             ("Dmitry", "No messages yet", "Sun", None, "D")]
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__chats", "Chats")
    lw = 300
    R(cx, cy, lw, ch, ELEV); L(cx + lw, cy, cx + lw, cy + ch, BORDER)
    yy = cy + 14
    T(cx + 16, yy + 10, "Chats", 17, FG, 600)
    glyph(cx + lw - 26, yy + 6, "plus", ACCENT); yy += 26
    yy = search(cx + 16, yy, lw - 32, "Search") + 10
    T(cx + 16, yy + 10, "PINNED", 9, FAINT, 600); yy += 18
    for i, (t, s, m, b, ini) in enumerate(chats):
        yy = listrow(cx + 8, yy, lw - 16, t, s, m, b, ini, selected=(i == 0))
        if i == 1:
            T(cx + 16, yy + 12, "ALL CHATS", 9, FAINT, 600); yy += 18
    R(cx + lw, cy, cw - lw, ch, BG)
    R(cx + lw + (cw - lw) / 2 - 110, cy + ch / 2 - 16, 220, 32, SUNKEN, 16)
    T(cx + lw + (cw - lw) / 2, cy + ch / 2 + 4, "Select a chat to start messaging", 11.5, MUTED, 400, "middle")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__chats", "Chats", None, ("plus",), tab="Chats", big_title=True, bg=ELEV)
        yy = top + 10
        yy = search(x + 12, yy, PH_W - 24, "Search" if plat == "ios" else "Search chats") + 8
        T(x + 16, yy + 10, "PINNED", 9, FAINT, 600); yy += 18
        for i, (t, s, m, b, ini) in enumerate(chats[:4]):
            yy = listrow(x + 4, yy, PH_W - 8, t, s, m, b, ini)
            if i == 1:
                T(x + 16, yy + 12, "ALL CHATS", 9, FAINT, 600); yy += 18
        if plat == "android":
            C(x + PH_W - 40, bot - 40, 22, ACCENT); glyph(x + PH_W - 40, bot - 40, "plus", "#ffffff")
        tabbar(x, bot, PH_W, "Chats")
        Gend()

row("Chats list", [
    "+ The strongest parity in the product: same title, same search, pinned / all grouping, same swipe-or-menu actions (Pin, Mute, Archive), same empty state 'No chats yet' + 'Start a chat'.",
    "+ Same five destinations on all three: Chats, Groups, Channels, Contacts, Settings.",
    "~ Android places a floating action button for New chat; iOS and web use a header +.",
    "! The design system's IA lists a top-level 'Calls' destination. No client has one - web /calls redirects to Contacts and call links live inside Contacts. Either the doc or the three clients are wrong.",
])
ROWS[-1]["draw"] = r_chats

# ============================================================ ROW 6 conversation
def r_chat(y):
    msgs = [("in", "Are we still on for 7?", "11:58", None),
            ("out", "Yes. I booked the table already.", "12:01", "Read"),
            ("in", "Perfect. I'll bring the photos.", "12:03", None),
            ("out", "see you at 7", "12:04", "Sent")]
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__conversation", "Chats")
    lw = 260
    R(cx, cy, lw, ch, ELEV); L(cx + lw, cy, cx + lw, cy + ch, BORDER)
    yy = search(cx + 14, cy + 16, lw - 28, "Search") + 8
    for i, (t, s, m, b, ini) in enumerate([("Anna Weber", "You: see you at 7", "12:04", None, "AW"),
                                           ("Ivan Petrov", "Anna: sent a photo", "11:20", None, "IP"),
                                           ("Maria", "Typing...", "Tue", None, "M")]):
        yy = listrow(cx + 6, yy, lw - 12, t, s, m, b, ini, selected=(i == 0))
    mx = cx + lw; mw = cw - lw
    R(mx, cy, mw, ch, BG)
    R(mx, cy, mw, 52, ELEV); L(mx, cy + 52, mx + mw, cy + 52, BORDER)
    avatar(mx + 30, cy + 26, 15, "AW")
    T(mx + 54, cy + 22, "Anna Weber", 14, FG, 600)
    T(mx + 54, cy + 37, "@anna", 10.5, MUTED)
    for i, ic in enumerate(("more", "video", "call")):
        glyph(mx + mw - 24 - i * 32, cy + 26, ic, ACCENT)
    yy = cy + 70
    for side, txt, tm, st in msgs:
        yy = bubble(mx + 20, yy, mw - 40, txt, side, f"{tm}  {st}" if st else tm)
    T(mx + 24, yy + 14, "Anna is typing...", 10.5, FAINT, 400, italic=True)
    composer(mx, cy + ch - 48, mw)
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__conversation", "", "back",
                               ("more", "video", "call"), bg=BG)
        R(x, top - 44, PH_W, 44, ELEV)
        glyph(x + 18, top - 22, "back", ACCENT)
        avatar(x + 48, top - 22, 13, "AW")
        T(x + 68, top - 25, "Anna Weber", 12.5, FG, 600)
        T(x + 68, top - 12, "@anna", 9.5, MUTED)
        for i, ic in enumerate(("more", "video", "call")):
            glyph(x + PH_W - 20 - i * 28, top - 22, ic, ACCENT)
        yy = top + 12
        for side, txt, tm, st in msgs:
            yy = bubble(x + 14, yy, PH_W - 28, txt, side, f"{tm}  {st}" if st else tm, 26)
        T(x + 18, yy + 14, "Anna is typing...", 10, FAINT, 400, italic=True)
        composer(x, bot - 46, PH_W, 46)
        Gend()

row("Conversation", [
    "+ Same header (avatar, name, @username, call / video / more), same bubble shape, same 'Sent' / 'Read' state, same typing line, same composer with attach + emoji + send.",
    "+ Edit, Delete, React, reaction chips and the '(edited)' / 'message deleted' markers exist on all three; hover menu on web, long-press on the phones - the documented divergence.",
    "~ Group info opens a right-side members panel on web, a sheet labelled 'Group info' on iOS, and a screen labelled 'Details' on Android. Three names for one surface.",
])
ROWS[-1]["draw"] = r_chat

# ============================================================ ROW 7 new group
def r_newgroup(y):
    people = [("Anna Weber", "@anna", "AW", True), ("Ivan Petrov", "@ivan", "IP", True),
              ("Maria Lopez", "@maria", "ML", False), ("Dmitry K.", "@dmitry", "DK", False)]
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__new-group", "Groups")
    R(cx, cy, cw, ch, BG)
    px = cx + cw / 2 - 230
    R(px, cy + 40, 460, ch - 120, ELEV, 12, BORDER)
    yy = cy + 70
    T(px + 24, yy, "New group", 19, FG, 600); yy += 20
    yy = paragraph(px + 24, yy, "You will be the group's owner. You can add more people later.", 58, 11.5, MUTED, 15) + 14
    yy = field(px + 24, yy, 412, "Group name", "Weekend plans") + 20
    T(px + 24, yy, "MEMBERS", 9, FAINT, 600); yy += 12
    for n, u, ini, on in people:
        R(px + 24, yy, 412, 44, SUNKEN if on else ELEV, 8)
        avatar(px + 48, yy + 22, 14, ini)
        T(px + 70, yy + 20, n, 12.5, FG, 500); T(px + 70, yy + 33, u, 10, MUTED)
        C(px + 420, yy + 22, 8, ACCENT if on else "none", BORDER if not on else None)
        if on: T(px + 420, yy + 26, "✓", 9, "#ffffff", 700, "middle")
        yy += 48
    button(px + 24, cy + ch - 70, 412, "Create group")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__new-group", "New group",
                               "back" if plat == "android" else None, bg=ELEV)
        yy = top + 16
        yy = field(x + 18, yy, PH_W - 36, "Group name (optional)" if plat == "ios" else "Group name", "Weekend plans") + 6
        if plat == "ios":
            yy = paragraph(x + 18, yy + 10, "Everyone in the group sees the name. Leave it blank to title the group by its members.", 44, 10, FAINT, 13) + 10
        else:
            yy += 14
        T(x + 18, yy, "MEMBERS", 9, FAINT, 600); yy += 12
        for n, u, ini, on in people:
            R(x + 18, yy, PH_W - 36, 44, SUNKEN if on else ELEV, 8)
            avatar(x + 42, yy + 22, 14, ini)
            T(x + 64, yy + 20, n, 12, FG, 500); T(x + 64, yy + 33, u, 9.5, MUTED)
            C(x + PH_W - 36, yy + 22, 8, ACCENT if on else "none", BORDER if not on else None)
            if on: T(x + PH_W - 36, yy + 26, "✓", 9, "#ffffff", 700, "middle")
            yy += 48
        button(x + 18, bot - 56, PH_W - 36, "Create group", "primary", 40)
        Gend()

row("New group", [
    "+ Same shape: name field, contact picker with checkmarks, one Create group button, same refusal copy 'Accept this contact again to add them to a group.'",
    "~ Only iOS explains what an empty name does ('Leave it blank to title the group by its members'). Web and Android leave the user guessing.",
    "~ The no-contacts empty state differs: web 'Add contacts first; a group starts from people you already know', iOS points at 'the Contacts tab', Android shows a 'Go to Contacts' button.",
])
ROWS[-1]["draw"] = r_newgroup

# ============================================================ ROW 8 contacts
def r_contacts(y):
    reqs = [("@sergey", "Hi, this is Sergey from the meetup.", "S")]
    cons = [("Anna Weber", "@anna", "AW"), ("Ivan Petrov", "@ivan", "IP"),
            ("Maria Lopez", "@maria", "ML")]
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__contacts", "Contacts")
    R(cx, cy, cw, ch, BG)
    colw = (cw - 60) / 2
    # left column
    lx = cx + 20
    R(lx, cy + 20, colw, 150, ELEV, 12, BORDER)
    T(lx + 20, cy + 46, "Add a contact", 15, FG, 600)
    yy = field(lx + 20, cy + 60, colw - 40, "Add by username", "@alice") + 12
    yy = field(lx + 20, yy, colw - 40, "", "Hi, this is Alice from the conference.") + 8
    button(lx + 20, yy, 90, "Send", "primary", 30)
    R(lx, cy + 186, colw, 120, ELEV, 12, BORDER)
    T(lx + 20, cy + 212, "Invite links", 15, FG, 600)
    paragraph(lx + 20, cy + 232, "Share a link with someone you want to hear from.", 40, 11, MUTED, 14)
    button(lx + 20, cy + 258, 130, "Create link", "secondary", 30)
    R(lx, cy + 322, colw, 120, ELEV, 12, BORDER)
    T(lx + 20, cy + 348, "Call link", 15, FG, 600)
    paragraph(lx + 20, cy + 368, "A call anyone with the link can ask to join. You host it.", 40, 11, MUTED, 14)
    button(lx + 20, cy + 396, 140, "Create call link", "secondary", 30)
    # right column
    rx = lx + colw + 20
    R(rx, cy + 20, colw, ch - 60, ELEV, 12, BORDER)
    yy = cy + 46
    T(rx + 16, yy, "Requests", 14, FG, 600); yy += 12
    for u, msg, ini in reqs:
        R(rx + 12, yy, colw - 24, 62, SUNKEN, 8)
        avatar(rx + 36, yy + 22, 14, ini)
        T(rx + 58, yy + 20, u, 12.5, FG, 600)
        T(rx + 58, yy + 34, msg, 10, MUTED)
        button(rx + 58, yy + 40, 66, "Accept", "primary", 20)
        button(rx + 130, yy + 40, 66, "Decline", "secondary", 20)
        yy += 74
    T(rx + 16, yy, "Connected", 14, FG, 600); yy += 12
    for n, u, ini in cons:
        yy = listrow(rx + 8, yy, colw - 16, n, u, None, None, ini, 52)
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__contacts", "Contacts", None, ("plus",), tab="Contacts", big_title=True, bg=ELEV)
        yy = top + 10
        yy = search(x + 12, yy, PH_W - 24, "Search contacts" if plat == "ios" else "Search") + 10
        T(x + 16, yy + 8, "REQUESTS", 9, FAINT, 600); yy += 16
        for u, msg, ini in reqs:
            R(x + 10, yy, PH_W - 20, 62, SUNKEN, 8)
            avatar(x + 34, yy + 22, 14, ini)
            T(x + 56, yy + 20, u, 12, FG, 600)
            T(x + 56, yy + 34, "Hi, this is Sergey...", 10, MUTED)
            button(x + 56, yy + 40, 62, "Accept", "primary", 20)
            button(x + 124, yy + 40, 62, "Decline", "secondary", 20)
            yy += 72
        T(x + 16, yy + 8, "CONTACTS", 9, FAINT, 600); yy += 16
        for n, u, ini in cons:
            yy = listrow(x + 4, yy, PH_W - 8, n, u, None, None, ini, 50)
        yy += 10
        R(x + 12, yy, PH_W - 24, 34, SUNKEN, 8)
        T(x + 24, yy + 22, "Share invite link" if plat == "ios" else "Invite links", 11.5, ACCENT, 500)
        yy += 42
        R(x + 12, yy, PH_W - 24, 34, SUNKEN, 8)
        T(x + 24, yy + 22, "Create call link", 11.5, ACCENT, 500)
        tabbar(x, bot, PH_W, "Contacts")
        Gend()

row("Contacts", [
    "+ Same three jobs on all three: add by username with an intro line, invite links, call links. Requests carry Accept / Decline.",
    "~ Web lays them out as three cards side by side; the phones stack them under the contact list, so 'Create call link' is below the fold on a phone.",
    "! Invite links carry a label, an expiry preset and a use limit on web and Android. iOS offers only Create link / Copy / Share / Revoke, so a link made on an iPhone silently takes whatever the defaults are.",
    "! This screen is where Calls actually lives. Nothing in the navigation says so.",
])
ROWS[-1]["draw"] = r_contacts

# ============================================================ ROW 9 channels
def r_channels(y):
    chans = [("Lakko news", "@lakko-news", "LN", "Subscribed"),
             ("Design notes", "@design", "DN", "Subscribed"),
             ("Release log", "@releases", "RL", None)]
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__channels", "Channels")
    lw = 280
    R(cx, cy, lw, ch, ELEV); L(cx + lw, cy, cx + lw, cy + ch, BORDER)
    yy = cy + 14
    T(cx + 16, yy + 10, "Channels", 17, FG, 600)
    glyph(cx + lw - 26, yy + 6, "plus", ACCENT); yy += 26
    yy = search(cx + 16, yy, lw - 32, "@my-channel") + 10
    for n, h, ini, st in chans:
        yy = listrow(cx + 8, yy, lw - 16, n, h, None, None, ini, 54, selected=(n == "Lakko news"))
    yy += 8
    T(cx + 16, yy + 10, "New channel", 11.5, ACCENT, 500)
    T(cx + 16, yy + 30, "Join as co-publisher", 11.5, ACCENT, 500)
    mx = cx + lw; mw = cw - lw
    R(mx, cy, mw, ch, BG)
    R(mx, cy, mw, 52, ELEV); L(mx, cy + 52, mx + mw, cy + 52, BORDER)
    avatar(mx + 30, cy + 26, 15, "LN")
    T(mx + 54, cy + 22, "Lakko news", 14, FG, 600)
    T(mx + 54, cy + 37, "@lakko-news  ·  Subscribed", 10.5, MUTED)
    T(mx + mw - 20, cy + 30, "Manage", 11.5, ACCENT, 500, "end")
    yy = cy + 70
    for txt in ("Version 1.4 is out. Group names are end-to-end encrypted now.",
                "We published the security notes for this release.",
                "Calls now show the safety number on every platform."):
        lines = wrap(txt, 52)
        bh = len(lines) * 16 + 26
        R(mx + 20, yy, mw - 40, bh, ELEV, 12, BORDER)
        for i, ln in enumerate(lines):
            T(mx + 34, yy + 22 + i * 16, ln, 12, FG)
        T(mx + mw - 34, yy + bh - 8, "12:04", 9, FAINT, 400, "end")
        yy += bh + 10
    composer(mx, cy + ch - 48, mw, placeholder="Write a post")
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__channels", "Channels", None, ("plus",), tab="Channels", big_title=True, bg=ELEV)
        yy = top + 10
        yy = search(x + 12, yy, PH_W - 24,
                    "Search or enter a @handle" if plat == "ios" else "Find a channel") + 10
        for n, h, ini, st in chans:
            yy = listrow(x + 4, yy, PH_W - 8, n, h, st, None, ini, 54)
        yy += 12
        R(x + 12, yy, PH_W - 24, 34, SUNKEN, 8)
        T(x + 24, yy + 22, "Create a channel", 11.5, ACCENT, 500); yy += 42
        R(x + 12, yy, PH_W - 24, 34, SUNKEN, 8)
        T(x + 24, yy + 22, "Join as co-publisher", 11.5, ACCENT, 500); yy += 50
        paragraph(x + 16, yy, "Channels you create or subscribe to appear here.", 40, 10.5, FAINT, 14)
        tabbar(x, bot, PH_W, "Channels")
        Gend()

row("Channels", [
    "+ Same model on all three: subscribed list, find by @handle, create, join as co-publisher, reader with posts, 'Waiting to be let in...' while the publisher shares access.",
    "~ Search placeholder: '@my-channel' (web), 'Search or enter a @handle' (iOS), 'Find a channel' (Android).",
    "+ Name, handle and an optional description on all three create flows; subscribers see the name and the description.",
])
ROWS[-1]["draw"] = r_channels

# ============================================================ ROW 10 in-call
def r_call(y):
    def tiles(x, ty, w, h, cols, rows_n, names):
        tw = (w - (cols - 1) * 8) / cols
        th = (h - (rows_n - 1) * 8) / rows_n
        k = 0
        for r_ in range(rows_n):
            for c_ in range(cols):
                if k >= len(names): return
                nm, mic = names[k]; k += 1
                tx, tyy = x + c_ * (tw + 8), ty + r_ * (th + 8)
                R(tx, tyy, tw, th, "#20243a", 12)
                avatar(tx + tw / 2, tyy + th / 2 - 6, min(22, th / 4), nm[:2].upper(), "#2f3554", "#c7cdf5")
                T(tx + 10, tyy + th - 10, nm, 10, "#e6e9f4", 500)
                if not mic:
                    C(tx + tw - 16, tyy + th - 14, 7, "#3b3f5c")
                    L(tx + tw - 20, tyy + th - 18, tx + tw - 12, tyy + th - 10, "#f87171", 1.4)
        return

    def menu(x, my, w, items):
        h = len(items) * 22 + 16
        R(x, my, w, h, "#232841", 10, "#39406a")
        for i, (lab, fake) in enumerate(items):
            T(x + 12, my + 25 + i * 22, lab, 10.5, "#f87171" if fake else "#e6e9f4", 500 if fake else 400)
        return my + h

    def controls(x, cyy, w, labels, dark=True):
        n = len(labels); step = w / n
        for i, (lab, kind) in enumerate(labels):
            ccx = x + step * (i + 0.5)
            C(ccx, cyy, 19, DANGER if kind == "end" else "#2a2f4a")
            T(ccx, cyy + 4, lab[:1].upper() if kind != "end" else "✕", 12,
              "#ffffff", 600, "middle")
            T(ccx, cyy + 34, lab, 8.5, "#aab1cc", 400, "middle")

    cx, cy, cw, ch = web_frame(X_WEB, y, "web__in-call")
    R(cx, cy, cw, ch, "#12152a")
    T(cx + 20, cy + 30, "Weekend plans", 15, "#ffffff", 600)
    T(cx + 20, cy + 47, "04:12", 11, "#aab1cc")
    pill(cx + 220, cy + 22, "Encrypted  ·  4821 9930 1174 6650", ACCENT_SOFT, ACCENT, 22, 12, 10.5, 600)
    T(cx + cw - 20, cy + 37, "Connection details", 11, "#aab1cc", 400, "end")
    tiles(cx + 20, cy + 64, cw - 40 - 200, ch - 190, 2, 2,
          [("Anna Weber", True), ("Ivan Petrov", False), ("Maria", True), ("You", True)])
    R(cx + cw - 190, cy + 64, 170, ch - 190, "#1a1e34", 12)
    T(cx + cw - 176, cy + 88, "Participants (4)", 11.5, "#e6e9f4", 600)
    for i, nm in enumerate(("Anna Weber", "Ivan Petrov", "Maria", "You")):
        avatar(cx + cw - 164, cy + 112 + i * 34, 11, nm[:2].upper(), "#2f3554", "#c7cdf5")
        T(cx + cw - 146, cy + 116 + i * 34, nm, 10.5, "#e6e9f4")
    controls(cx + cw / 2 - 190, cy + ch - 74, 380,
             [("Speaker", "s"), ("Mic", "m"), ("Camera", "c"),
              ("Participants", "p"), ("Chat", "ch"), ("End", "end")])
    T(cx + 20, cy + ch - 16, "Flip camera lives on the self tile, not in the bar", 9.5, "#8a92b2", 400, "start", italic=True)
    Gend()

    bar = [("Speaker", "s"), ("Mic", "m"), ("Camera", "c"), ("More", "o"), ("End", "end")]
    menus = {"ios": [("Participants", False), ("Chat", False), ("Settings", False),
                     ("Settings > Low-data mode", True)],
             "android": [("Participants", False), ("Chat", False), ("Raise hand", True),
                         ("Share screen", True), ("Settings", False),
                         ("Settings > Low-data mode", True)]}
    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__in-call", bg="#12152a")
        R(x, top - 18, PH_W, PH_H - 18, "#12152a", 18)
        T(x + 16, top + 14, "Weekend plans", 13.5, "#ffffff", 600)
        T(x + 16, top + 30, "04:12", 10, "#aab1cc")
        pill(x + 16, top + 42, "Encrypted", ACCENT_SOFT, ACCENT, 20, 10, 10, 600)
        T(x + 96, top + 56, "4821 9930 1174 6650", 9.5, "#aab1cc")
        T(x + PH_W - 16, top + 22, "Participants (4)", 10, "#aab1cc", 400, "end")
        tiles(x + 12, top + 70, PH_W - 24, 250, 2, 2,
              [("Anna Weber", True), ("Ivan Petrov", False), ("Maria", True), ("You", True)])
        my = menu(x + PH_W - 186, top + 334, 170, menus[plat])
        T(x + PH_W - 186, my + 14, "\u2191 the More menu, opened", 9, "#8a92b2", 400, "start", italic=True)
        controls(x + 12, bot - 62, PH_W - 24, bar)
        T(x + PH_W / 2, bot - 10, "red = local flag only, no peer sees it", 9, "#f87171", 400, "middle", italic=True)
        Gend()

row("In-call", [
    "+ Same shell everywhere: title, timer, roster tiles with a muted marker, participants list, in-call chat, and the E2EE pill that only turns on when the media is really sealed.",
    "+ The safety number and the connection detail (round trip, direct / relay) are on all three.",
    "+ The control bar itself matches: Speaker, Mic, Camera, then group-only Participants / Chat (one More button on the phones), then End. Off is the loud state on all three.",
    "! The drift is inside the More menu. Android still lists Raise hand and Share screen; both write a flag only that device can see, so the tile badge 'Sharing' is shown to nobody. Web deleted them for exactly that reason and the spec puts screen sharing out of scope; iOS kept the fields and dropped the controls.",
    "! 'Low-data mode' on both phones promises 'Caps video at 360p'. Nothing reads the flag - no media path on either platform mentions it. Web removed the same toggle in the same pass.",
    "~ Same control, two names: 'Flip camera' on the phones, 'Switch to the other camera' on web.",
])
ROWS[-1]["draw"] = r_call

# ============================================================ ROW 11 devices
def r_devices(y):
    devs = [("iPhone 15", "This device", True), ("MacBook Pro", "Added 12 Sep", False),
            ("Pixel 8", "Added 3 Sep", False)]
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__devices", "Settings")
    R(cx, cy, cw, ch, BG)
    px = cx + cw / 2 - 250
    yy = cy + 34
    T(px, yy, "Your devices", 19, FG, 600); yy += 18
    paragraph(px, yy, "See, add and remove the devices on this account.", 60, 11.5, MUTED, 15); yy += 28
    R(px, yy, 500, 180, ELEV, 12, BORDER)
    iy = yy + 16
    for n, s, me in devs:
        T(px + 20, iy + 20, n, 13, FG, 500)
        T(px + 20, iy + 35, "The device you are using now." if me else s, 10.5, MUTED)
        if not me:
            button(px + 410, iy + 12, 70, "Remove", "danger", 26)
        else:
            pill(px + 420, iy + 14, "This device", ACCENT_SOFT, ACCENT, 20, 8, 10, 600)
        iy += 54
        if n != devs[-1][0]: L(px + 16, iy - 8, px + 484, iy - 8, BORDER)
    yy += 200
    R(px, yy, 500, 130, ELEV, 12, BORDER)
    T(px + 20, yy + 28, "Add a device", 14, FG, 600)
    paragraph(px + 20, yy + 44, "The new device shows a code; scan it with this one.", 58, 11, MUTED, 14)
    button(px + 20, yy + 76, 130, "Scan a code", "primary", 32)
    button(px + 162, yy + 76, 130, "Show a code", "secondary", 32)
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        title = "Devices"
        top, bot = phone_frame(x, y, f"{plat}__devices", title, "back", bg=BG)
        yy = top + 14
        R(x + 12, yy, PH_W - 24, 158, ELEV, 12, BORDER)
        iy = yy + 10
        for n, s, me in devs:
            T(x + 26, iy + 20, n, 12.5, FG, 500)
            T(x + 26, iy + 34, "This device" if me else s, 10, MUTED)
            if not me: T(x + PH_W - 26, iy + 26, "Remove", 11, DANGER, 500, "end")
            iy += 50
        yy += 172
        R(x + 12, yy, PH_W - 24, 112, ELEV, 12, BORDER)
        T(x + 26, yy + 26, "Add a device" if plat == "ios" else "Link device", 13, FG, 600)
        paragraph(x + 26, yy + 42, "Point the camera at the code your new device is showing.", 40, 10, MUTED, 13)
        button(x + 26, yy + 72, PH_W - 76, "Open camera" if plat == "android" else "Scan a code", "primary", 30)
        yy += 126
        Gend()

row("Devices", [
    "+ Same list shape (this device marked, Remove on the others) and the same pairing idea (one device shows a code, the other scans it).",
    "! Vocabulary split: web and iOS say 'Add a device', Android says 'Link device' / 'Link this device?'. The user reads one word on the phone and another on the laptop for the same act.",
    "+ The recovery phrase, passkeys and the authenticator sit behind Settings > Security > Recovery on all three, not on this screen; that page links back here through its 'Other devices' row.",
    "~ Only web offers both directions ('Scan a code' and 'Show a code') in one place; the phones scan only.",
])
ROWS[-1]["draw"] = r_devices

# ============================================================ ROW 12 settings
def r_settings(y):
    cx, cy, cw, ch = web_frame(X_WEB, y, "web__settings", "Settings")
    R(cx, cy, cw, ch, BG)
    lw = 250
    R(cx, cy, lw, ch, ELEV); L(cx + lw, cy, cx + lw, cy + ch, BORDER)
    T(cx + 16, cy + 30, "Settings", 17, FG, 600)
    yy = cy + 48
    for lab in ("Profile", "Security", "Appearance", "Media", "About", "Account"):
        if lab == "Security": R(cx + 8, yy, lw - 16, 42, ACCENT_SOFT, 8)
        T(cx + 20, yy + 26, lab, 13, ACCENT if lab == "Security" else FG, 500)
        yy += 46
    mx = cx + lw; mw = cw - lw
    yy = cy + 34
    T(mx + 24, yy, "Security", 19, FG, 600); yy += 18
    paragraph(mx + 24, yy, "How you sign in, which devices can read your messages, and how you get back in.", 56, 11.5, MUTED, 15)
    yy += 34
    for title, sub in (("Passkeys", "Sign in without a password."),
                       ("Devices", "See, add and remove the devices on this account."),
                       ("Recovery", "Your recovery phrase and the other ways back into your account."),
                       ("Safety numbers", "Compare codes with a contact.")):
        R(mx + 24, yy, mw - 68, 58, ELEV, 12, BORDER)
        T(mx + 44, yy + 24, title, 13, FG, 600)
        T(mx + 44, yy + 41, sub, 10.5, MUTED)
        L(mx + mw - 54, yy + 25, mx + mw - 50, yy + 29, FAINT, 1.4)
        L(mx + mw - 54, yy + 33, mx + mw - 50, yy + 29, FAINT, 1.4)
        yy += 66
    Gend()

    native_rows = [("Account", [("Profile", None)]),
                   ("Security", [("Passkeys", None), ("Devices", None), ("Recovery", None)]),
                   ("Appearance", [("Theme", "System")]),
                   ("Media", [("Autoplay video notes", "Off")]),
                   ("Network", [("Anti-censorship", "Off")]),
                   ("About", [("Version", "1.4.0")])]
    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__settings", "Settings", None, (), tab="Settings", big_title=True, bg=BG)
        yy = top + 6
        R(x + 10, yy, PH_W - 20, 52, ELEV, 12, BORDER)
        avatar(x + 38, yy + 26, 17, "AW")
        T(x + 66, yy + 23, "Anna Weber", 13, FG, 600)
        T(x + 66, yy + 38, "@anna", 10, MUTED)
        yy += 60
        for sec, rows_ in native_rows:
            T(x + 22, yy + 11, sec.upper(), 9, MUTED, 600); yy += 17
            R(x + 10, yy, PH_W - 20, len(rows_) * 28 + 4, ELEV, 10, BORDER)
            ry = yy + 2
            for t_, v_ in rows_:
                settingsrow(x + 12, ry, PH_W - 24, t_, v_, chevron=(v_ is None), h=28)
                ry += 28
            yy += len(rows_) * 28 + 10
        T(x + 22, yy + 11, "DANGER ZONE", 9, DANGER, 600); yy += 17
        dz = ["Log out", "Forget this device"] + (["Delete account"] if plat == "ios" else [])
        R(x + 10, yy, PH_W - 20, len(dz) * 28 + 4, ELEV, 10, BORDER)
        ry = yy + 2
        for t_ in dz:
            settingsrow(x + 12, ry, PH_W - 24, t_, None, False, 28, danger=True)
            ry += 28
        if plat == "android":
            T(x + 22, ry + 16, "(no Delete account here - it lives on Passkeys)", 9, DANGER, 400, italic=True)
        tabbar(x, bot, PH_W, "Settings")
        Gend()

row("Settings", [
    "+ Same sections and same order on all three: Profile, Security (Passkeys / Devices / Recovery), Appearance, Media, About, then a separated danger zone.",
    "~ Web is master / detail with a section list; the phones are one scrolling list with section headers. Fine for the form factor, but web nests Profile as a peer section while the phones nest it under 'Account'.",
    "! Web has no Network section - anti-censorship cannot work in a browser (ADR-0026). The gap is real and correct; the UI never says so, it just is not there.",
    "! Android's 'Delete account' is not in the danger zone at all - it sits on the Passkeys screen. Web and iOS put it in Settings > Account. A destructive action in a place nobody would look for it.",
])
ROWS[-1]["draw"] = r_settings

# ============================================================ settings > security > recovery
REC_KEYS_GROUP = "Brings back your chats on a new device"
REC_SIGNIN_GROUP = "Signs you in when you forget your password"
REC_PHRASE_SUB = "The 12 words you wrote down at signup restore your keys and contacts on a new device."
REC_PASSKEYS_SUB = "2 passkeys. 1 restores keys."
REC_DEVICES_SUB = "See, add and remove the devices on this account."
REC_WORDS_SUB = ("If you forget your password on this device, type three words of your recovery "
                 "phrase to sign in. There is nothing to set up.")
REC_TOTP_LEDE = "A six-digit code from an authenticator app lets you sign in again if you lose your password."
REC_LEDE = ("Your recovery phrase restores everything. These get you back into the account when you "
            "have lost your password; your messages then come from the phrase or from another of your devices.")

def r_recovery_settings(y):
    def inforow(x, yy, w, title, subs, chars, action=None, chevron=False, strong=None):
        lines = wrap(subs, chars)
        T(x + 14, yy + 18, title, 12.5, FG, 600)
        for i, ln in enumerate(lines):
            T(x + 14, yy + 32 + i * 12, ln, 9.8, MUTED)
        h = 26 + len(lines) * 12
        if strong:
            T(x + 14, yy + 32 + len(lines) * 12, strong, 9.8, FG, 600); h += 12
        if action:
            T(x + w - 14, yy + 18, action, 11, ACCENT, 500, "end")
        if chevron:
            L(x + w - 18, yy + 14, x + w - 14, yy + 18, FAINT, 1.4)
            L(x + w - 18, yy + 22, x + w - 14, yy + 18, FAINT, 1.4)
        return yy + h + 8

    cx, cy, cw, ch = web_frame(X_WEB, y, "web__settings-recovery", "Settings")
    R(cx, cy, cw, ch, BG)
    px, pw = cx + cw / 2 - 270, 540
    yy = cy + 30
    T(px, yy, "< Security", 11.5, ACCENT, 500); yy += 28
    T(px, yy, "Recovery", 19, FG, 600); yy += 16
    R(px, yy, pw, 198, ELEV, 12, BORDER)
    T(px + 14, yy + 24, REC_KEYS_GROUP, 13.5, FG, 600)
    iy = yy + 32
    iy = inforow(px, iy, pw, "Recovery phrase", REC_PHRASE_SUB, 70, action="Show recovery phrase", strong="Saved on this device.")
    L(px + 14, iy - 3, px + pw - 14, iy - 3, BORDER)
    iy = inforow(px, iy, pw, "Passkeys", REC_PASSKEYS_SUB, 70, action="Manage")
    L(px + 14, iy - 3, px + pw - 14, iy - 3, BORDER)
    inforow(px, iy, pw, "Other devices", REC_DEVICES_SUB, 70, action="Manage")
    yy += 210
    R(px, yy, pw, 100, ELEV, 12, BORDER)
    T(px + 14, yy + 24, REC_SIGNIN_GROUP, 13.5, FG, 600)
    inforow(px, yy + 32, pw, "Three words from your phrase", REC_WORDS_SUB, 92)
    yy += 112
    R(px, yy, pw, 104, ELEV, 12, BORDER)
    T(px + 14, yy + 24, "Authenticator app", 13.5, FG, 600)
    paragraph(px + 14, yy + 42, REC_TOTP_LEDE, 96, 10.5, MUTED, 13)
    button(px + 14, yy + 62, 170, "Set up authenticator", "secondary", 30)
    Gend()

    for x, plat in ((X_IOS, "ios"), (X_AND, "android")):
        top, bot = phone_frame(x, y, f"{plat}__settings-recovery", "Recovery", "back", bg=BG)
        w = PH_W - 24
        yy = top + 12
        if plat == "android":
            yy = paragraph(x + 14, yy + 8, REC_LEDE, 52, 9.8, MUTED, 12) + 2
        T(x + 14, yy + 10, REC_KEYS_GROUP, 10.5, MUTED, 600); yy += 18
        card_top = yy
        R(x + 12, card_top, w, 214, ELEV, 12, BORDER)
        iy = inforow(x + 12, yy, w, "Recovery phrase", REC_PHRASE_SUB, 48, strong="Saved on this device.")
        button(x + 26, iy - 4, w - 28, "Show recovery phrase", "secondary", 28); iy += 34
        L(x + 26, iy - 3, x + PH_W - 26, iy - 3, BORDER)
        iy = inforow(x + 12, iy, w, "Passkeys", REC_PASSKEYS_SUB, 44, chevron=True)
        L(x + 26, iy - 3, x + PH_W - 26, iy - 3, BORDER)
        inforow(x + 12, iy, w, "Other devices", REC_DEVICES_SUB, 44, chevron=True)
        yy = card_top + 222
        T(x + 14, yy + 10, REC_SIGNIN_GROUP, 10.5, MUTED, 600); yy += 18
        R(x + 12, yy, w, 76, ELEV, 12, BORDER)
        inforow(x + 12, yy, w, "Three words from your phrase", REC_WORDS_SUB, 50)
        yy += 86
        if plat == "ios":
            R(x + 12, yy, w, 98, ELEV, 12, BORDER)
            T(x + 26, yy + 22, "Authenticator app", 12.5, FG, 600)
            paragraph(x + 26, yy + 38, REC_TOTP_LEDE, 48, 9.8, MUTED, 12)
            button(x + 26, yy + 62, w - 28, "Set up authenticator", "secondary", 28)
        else:
            R(x + 12, yy, w, 46, ELEV, 12, BORDER)
            T(x + 26, yy + 20, "Authenticator app", 12.5, FG, 600)
            T(x + 26, yy + 34, "A code from your authenticator app.", 9.8, MUTED)
            L(x + PH_W - 34, yy + 20, x + PH_W - 29, yy + 25, FAINT, 1.4)
            L(x + PH_W - 24, yy + 20, x + PH_W - 29, yy + 25, FAINT, 1.4)
        Gend()

row("Settings > Security > Recovery", [
    "+ Same content and order on all three: the phrase row (what it is for, 'Saved on this device.', 'Show recovery phrase'), Passkeys with the count and how many restore keys, Other devices, an informational three-word row with nothing to switch, then the authenticator.",
    "! 'Show recovery phrase' proves it is you in three ways. Web asks for the account password. iOS asks the device (Face ID, Touch ID or the passcode) and refuses when no passcode is set. Android asks the device (fingerprint, face or screen lock) and falls back to the account password when the device has no screen lock.",
    "~ Web puts 'Show recovery phrase' and 'Manage' at the end of each row; the phones use a button under the phrase row and whole-row navigation for Passkeys and Other devices.",
    "~ Android opens the page with a paragraph explaining what the phrase and the other ways do; web and iOS go straight to the cards.",
    "~ The authenticator is an open card with its set-up button on web and iOS, and a collapsed 'Authenticator app' row that expands on Android.",
])
ROWS[-1]["draw"] = r_recovery_settings

# ---------------------------------------------------------------- assemble
def header():
    R(0, 0, TOTAL_W, HEADER_H, ELEV)
    L(0, HEADER_H - 1, TOTAL_W, HEADER_H - 1, BORDER)
    R(MARGIN, 34, 52, 52, ACCENT, 12)
    T(MARGIN + 26, 70, "L", 26, "#ffffff", 700, "middle")
    T(MARGIN + 70, 56, "Lakko - cross-platform screen matrix", 26, FG, 700)
    T(MARGIN + 70, 78, "Every major screen on web, iOS and Android side by side, with the copy each client actually ships.", 13, MUTED)
    T(MARGIN + 70, 96, "Drawn from the three clients' sources on 2026-09-21; sign-up and recovery rows redrawn 2026-10-02. Schematic, not pixel-accurate: read it for structure, copy and parity, not for spacing.", 12, FAINT)
    yy = 128
    for lab, x, w in (("WEB  -  desktop, SolidJS", X_WEB, WEB_W),
                      ("iOS  -  SwiftUI", X_IOS, PH_W),
                      ("ANDROID  -  Compose", X_AND, PH_W),
                      ("PARITY NOTES", X_NOT, NOTES_W)):
        R(x, yy, w, 30, SUNKEN, 8)
        T(x + 12, yy + 20, lab, 12, FG, 600)
    # legend
    lx = MARGIN + 70 + 640
    for col, lab in ((SUCCESS, "matches"), (WARN, "wording / layout drift"), (DANGER, "real gap")):
        C(lx, 52, 4, col); T(lx + 12, 56, lab, 11.5, MUTED)
        lx += len(lab) * 6.4 + 34

def build():
    total_h = HEADER_H + len(ROWS) * ROW_H + 30
    add(f'<?xml version="1.0" encoding="UTF-8"?>')
    add(f'<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->')
    add(f'<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->')
    add(f'<svg xmlns="http://www.w3.org/2000/svg" width="{TOTAL_W}" height="{total_h}" '
        f'viewBox="0 0 {TOTAL_W} {total_h}">')
    defs_at = len(out)
    add("<!--DEFS-->")
    R(0, 0, TOTAL_W, total_h, PAGE)
    header()
    render_all()
    add('</svg>')
    out[defs_at] = ('<defs><linearGradient id="brandGrad" x1="0" y1="0" x2="1" y2="1">'
                    '<stop offset="0" stop-color="#312e81"/><stop offset="1" stop-color="#818cf8"/>'
                    '</linearGradient>' + "".join(_clips) + '</defs>')
    return "\n".join(out)

if __name__ == "__main__":
    import sys
    path = sys.argv[1] if len(sys.argv) > 1 else "docs/ux/screen-matrix.svg"
    svg = build()
    with open(path, "w") as f:
        f.write(svg)
    print(f"wrote {path}: {len(svg)} bytes, {len(ROWS)} rows, {TOTAL_W}x{HEADER_H + len(ROWS) * ROW_H + 30}")
