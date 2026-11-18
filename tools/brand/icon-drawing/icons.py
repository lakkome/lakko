# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
"""Lakko in-app icon set: drawing definitions.

Every icon is drawn here on a 24x24 grid with absolute coordinates. Outline
icons are 1.75-wide round-capped strokes; filled variants are one even-odd
path that is filled and stroked with the same 1.75 stroke, so a filled icon
has the same outer size as its outline twin, and its knocked-out details are
drawn 0.875 larger than they appear."""
import math
from geom import (A, C, L, TAU, capsule, circle, circle_ccw, clip_closed_sdf, clip_open_sdf,
                  cross, path, parse, poly, rrect, runs_path, sdf_circle, sdf_rrect,
                  sdf_union, thick_polyline, xform_segs, arrow_outline, seg_start, seg_end,
                  bridge_along, clip_closed_sdf_bridged)

SW = 1.75
H = SW / 2
GAP = 2.75          # outline-to-outline clearance, centreline to centreline
ICONS = {}          # name -> list of (kind, d)


def icon(name):
    def deco(fn):
        ICONS[name] = fn
        return fn
    return deco


def S(*contours):
    return ("s", path(*contours))


def Sd(d):
    return ("s", d)


def F(*contours):
    return ("f", path(*contours))


def FS(*contours):
    return ("fs", path(*contours))


def segs_of(d):
    """Parsed contours of a raw absolute path string."""
    return parse(d)


def grow_circle(c, r):
    return circle_ccw(c, r + H)


def hole_line(p0, p1, half=0.8):
    """A knocked-out line of visible half width `half` (stroke-compensated)."""
    return capsule(p0, p1, half + H)


def hole_polyline(points, half=0.8):
    return thick_polyline(points, half + H)


def hole_dot(c, r=1.0):
    return circle_ccw(c, r + H)


def dot(c, r=1.15):
    return circle(c, r)


# ---------------------------------------------------------------- shared parts

BUBBLE = ("M12 3.5C17 3.5 21 6.95 21 11.15C21 15.35 17 18.8 12 18.8C10.75 18.8 9.55 18.6 8.45 18.2"
          "C7.25 19.35 5.6 20.15 3.6 20.4C4.4 19.4 4.95 18.3 5.15 17.1C3.8 15.6 3 13.45 3 11.15"
          "C3 6.95 7 3.5 12 3.5Z")

HANDSET = ("M8.3 3.75C7.8 3.05 6.85 2.9 6.15 3.4L4.4 4.65C3.35 5.4 2.85 6.7 3.15 7.95"
           "C4.45 13.55 10.45 19.55 16.05 20.85C17.3 21.15 18.6 20.65 19.35 19.6L20.6 17.85"
           "C21.1 17.15 20.95 16.2 20.25 15.7L17.55 13.75C16.95 13.3 16.1 13.4 15.6 13.95"
           "L14.45 15.2C12.25 14.1 9.9 11.75 8.8 9.55L10.05 8.4C10.6 7.9 10.7 7.05 10.25 6.45Z")

HANDSET_DOWN = ("M2.55 13.55C7.55 8.6 16.45 8.6 21.45 13.55C21.95 14.05 21.95 14.85 21.5 15.35"
                "L20.35 16.65C19.9 17.15 19.2 17.3 18.6 17.05L16.25 16.05C15.65 15.8 15.3 15.25 15.3 14.6"
                "L15.3 12.75C13.15 12.15 10.85 12.15 8.7 12.75L8.7 14.6C8.7 15.25 8.35 15.8 7.75 16.05"
                "L5.4 17.05C4.8 17.3 4.1 17.15 3.65 16.65L2.5 15.35C2.05 14.85 2.05 14.05 2.55 13.55Z")


def contour(d):
    return parse(d)[0][0]


def person_parts(cx=12.0, head_cy=7.6, head_r=3.75, body_top=14.0, body_bottom=20.5, half_w=7.0):
    """Head circle and a domed body with a flat base, as segments."""
    head = circle((cx, head_cy), head_r)
    # dome: two cubic quarters from the base corners up to the top
    k = 0.58
    hgt = body_bottom - body_top
    x0, x1 = cx - half_w, cx + half_w
    r = 1.0
    body = [
        C((x0, body_bottom - r), (x0, body_bottom - hgt * k - r * 0.2), (cx - half_w * k, body_top), (cx, body_top)),
        C((cx, body_top), (cx + half_w * k, body_top), (x1, body_bottom - hgt * k - r * 0.2), (x1, body_bottom - r)),
        A((x1 - r, body_bottom - r), r, 0, math.pi / 2),
        L((x1 - r, body_bottom), (x0 + r, body_bottom)),
        A((x0 + r, body_bottom - r), r, math.pi / 2, math.pi),
    ]
    return head, body


def person_sdf(cx=12.0, head_cy=7.6, head_r=3.75, body_top=14.0, body_bottom=20.5, half_w=7.0):
    """Rough signed distance to a person (head + body) for overlap clearing."""
    hgt = body_bottom - body_top
    # body approximated by an ellipse-ish rounded rect whose top is round
    body = sdf_rrect(cx - half_w, body_top, 2 * half_w, hgt + 6, min(half_w, hgt + 3) * 0.95)
    return sdf_union(sdf_circle((cx, head_cy), head_r), body)


# ---------------------------------------------------------------- tab bar

# The web tab-bar shapes (apps/web/src/ui/icons.tsx), chosen by the maintainer on
# 2026-11-12 over the SF-style bubble, newspaper and person-in-a-circle.
WEB_CHAT = ("M4 6A2.5 2.5 0 0 1 6.5 3.5L17.5 3.5A2.5 2.5 0 0 1 20 6L20 14"
            "A2.5 2.5 0 0 1 17.5 16.5L9.2 16.5L4.8 20L4.8 16.4A2.5 2.5 0 0 1 4 14Z")


@icon("chat")
def _():
    return [Sd(WEB_CHAT)]


@icon("chat-fill")
def _():
    return [("fs", WEB_CHAT)]


FEED_CARDS = (rrect(4, 4, 16, 6.5, 1.8), rrect(4, 13.5, 16, 6.5, 1.8))


@icon("feed")
def _():
    return [S(*FEED_CARDS)]


@icon("feed-fill")
def _():
    return [FS(*FEED_CARDS)]


# A person cropped by a rounded square (not a circle). The shoulders end on the
# flat part of the bottom edge, inside the corner arcs.
PSQ = (3.25, 3.25, 17.5, 17.5, 3.75)
PSQ_HEAD = ((12, 9.35), 3.3)


@icon("person-square")
def _():
    return [S(rrect(*PSQ)), S(circle(*PSQ_HEAD)),
            S(([C((7.15, 20.75), (7.75, 17.35), (9.65, 15.35), (12, 15.35)),
                C((12, 15.35), (14.35, 15.35), (16.25, 17.35), (16.85, 20.75))], False))]


@icon("person-square-fill")
def _():
    (hc, hr) = PSQ_HEAD
    head = circle_ccw(hc, hr + H)
    body = [C((7.25, 20.7), (7.8, 16.6), (9.75, 14.25), (12, 14.25)),
            C((12, 14.25), (14.25, 14.25), (16.2, 16.6), (16.75, 20.7)),
            L((16.75, 20.7), (7.25, 20.7))]
    return [FS(rrect(*PSQ), head, body)]


@icon("phone")
def _():
    return [S(*parse(HANDSET))]


@icon("phone-fill")
def _():
    return [FS(*parse(HANDSET))]


def gear(r_out=9.6, r_in=7.55, teeth=8, half_top=0.155, half_base=0.27):
    segs = []
    pts = []
    for k in range(teeth):
        th = -math.pi / 2 + k * TAU / teeth
        pts.append((th - half_base, r_in))
        pts.append((th - half_top, r_out))
        pts.append((th + half_top, r_out))
        pts.append((th + half_base, r_in))
    c = (12, 12)
    P = [(c[0] + r * math.cos(a), c[1] + r * math.sin(a)) for a, r in pts]
    n = len(P)
    for i in range(n):
        a0, r0 = pts[i]
        a1, r1 = pts[(i + 1) % n]
        if r0 == r_in and r1 == r_in and i % 4 == 3:
            a1u = a1 if a1 > a0 else a1 + TAU
            segs.append(A(c, r_in, a0, a1u))
        elif r0 == r_out and r1 == r_out:
            segs.append(A(c, r_out, a0, a1))
        else:
            segs.append(L(P[i], P[(i + 1) % n]))
    return segs


@icon("gear")
def _():
    return [S(gear()), S(circle((12, 12), 3.1))]


@icon("gear-fill")
def _():
    return [FS(gear(), circle_ccw((12, 12), 3.1 + H))]


# ---------------------------------------------------------------- chevrons, arrows

def open_line(*pts):
    return (poly(list(pts), closed=False), False)


@icon("chevron-back")
def _():
    return [S(open_line((14.75, 4.75), (7.75, 12), (14.75, 19.25)))]


@icon("chevron-forward")
def _():
    return [S(open_line((9.25, 4.75), (16.25, 12), (9.25, 19.25)))]


@icon("chevron-up")
def _():
    return [S(open_line((4.75, 15.75), (12, 8.25), (19.25, 15.75)))]


@icon("chevron-down")
def _():
    return [S(open_line((4.75, 8.25), (12, 15.75), (19.25, 8.25)))]


@icon("chevron-up-down")
def _():
    return [S(open_line((7.75, 9.25), (12, 5), (16.25, 9.25)), open_line((7.75, 14.75), (12, 19), (16.25, 14.75)))]


@icon("arrow-down")
def _():
    return [S(open_line((12, 3.75), (12, 20.25)), open_line((5.5, 13.75), (12, 20.25), (18.5, 13.75)))]


@icon("arrow-up-right")
def _():
    return [S(open_line((5.5, 18.5), (18.25, 5.75)), open_line((8.5, 5.75), (18.25, 5.75), (18.25, 15.5)))]


@icon("arrow-up-down")
def _():
    return [S(open_line((7.5, 19.5), (7.5, 4.5)), open_line((3.75, 8.25), (7.5, 4.5), (11.25, 8.25)),
              open_line((16.5, 4.5), (16.5, 19.5)), open_line((12.75, 15.75), (16.5, 19.5), (20.25, 15.75)))]


@icon("arrow-clockwise")
def _():
    c, r = (12, 12.75), 7.5
    a0 = math.radians(-50)
    arc = [A(c, r, a0, math.radians(270))]  # from the gap at upper right, round to the top
    tip = (c[0], c[1] - r)
    return [S((arc, False)), S(open_line((tip[0] - 2.6, tip[1] - 2.6), tip, (tip[0] - 2.6, tip[1] + 2.6)))]


def circled_arrow(up=True):
    disc = circle((12, 12), 9.25)
    if up:
        hole = arrow_outline((12, 17.0), (12, 7.25), (7.6, 11.65), (16.4, 11.65), 0.95 + H)
    else:
        hole = arrow_outline((12, 7.0), (12, 16.75), (7.6, 12.35), (16.4, 12.35), 0.95 + H)
    return [FS(disc, hole)]


@icon("arrow-up-circle-fill")
def _():
    return circled_arrow(True)


@icon("arrow-down-circle-fill")
def _():
    return circled_arrow(False)


@icon("arrow-down-square")
def _():
    # an app tile with a downward arrow: "get the newer app"
    return [S(rrect(4.25, 4.25, 15.5, 15.5, 4.0)),
            S(open_line((12, 7.75), (12, 15.75)), open_line((8.5, 12.25), (12, 15.75), (15.5, 12.25)))]


# ---------------------------------------------------------------- marks

@icon("xmark")
def _():
    return [S(open_line((6, 6), (18, 18)), open_line((18, 6), (6, 18)))]


@icon("checkmark")
def _():
    return [S(open_line((4.75, 12.75), (9.5, 17.5), (19.25, 6.5)))]


# ---- checkmark-double: two equal checks; the second one's short arm stops short of the first

@icon("checkmark-double")
def _():
    a, k = 4.0, 1.25            # short-arm run, long-arm slope
    xp, yp, run = 6.4, 17.0, 7.9
    first = [(xp - a, yp - a), (xp, yp), (xp + run, yp - run * k)]
    d = 7.0
    second = [(x + d, y) for x, y in first]
    arm = sdf_capsule_region(first[1], first[2], 0.0)
    short2 = keep_outside([open_line(second[0], second[1])], arm, GAP)
    return [S(open_line(*first)), S(*short2, open_line(second[1], second[2]))]


@icon("circle")
def _():
    return [S(circle((12, 12), 9.25))]


@icon("checkmark-circle")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((7.75, 12.4), (10.75, 15.4), (16.4, 9.0)))]


@icon("checkmark-circle-fill")
def _():
    return [FS(circle((12, 12), 9.25), hole_polyline([(7.75, 12.4), (10.75, 15.4), (16.4, 9.0)], 0.95))]


@icon("xmark-circle-fill")
def _():
    return [FS(circle((12, 12), 9.25), cross((12, 12), 4.1, 0.95 + H, math.pi / 4))]


@icon("minus-circle")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((7.5, 12), (16.5, 12)))]


@icon("minus-circle-fill")
def _():
    return [FS(circle((12, 12), 9.25), hole_line((7.5, 12), (16.5, 12), 0.95))]


@icon("radio-on")
def _():
    # the chosen option in a single-choice list
    return [S(circle((12, 12), 9.25)), F(circle((12, 12), 5.0))]


@icon("bullet")
def _():
    return [F(circle((12, 12), 3.25))]


@icon("circle-slash")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((5.5, 5.5), (18.5, 18.5)))]


@icon("plus")
def _():
    return [S(open_line((12, 4.5), (12, 19.5)), open_line((4.5, 12), (19.5, 12)))]


@icon("ellipsis")
def _():
    return [F(dot((5.5, 12), 1.6), dot((12, 12), 1.6), dot((18.5, 12), 1.6))]


@icon("ellipsis-circle")
def _():
    return [S(circle((12, 12), 9.25)), F(dot((7.75, 12), 1.25), dot((12, 12), 1.25), dot((16.25, 12), 1.25))]


@icon("info-circle")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((12, 11.0), (12, 16.75))), F(dot((12, 7.6), 1.2))]


@icon("exclamation-circle")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((12, 7.0), (12, 12.75))), F(dot((12, 16.4), 1.2))]


@icon("exclamation-circle-fill")
def _():
    return [FS(circle((12, 12), 9.25), hole_line((12, 7.0), (12, 12.75), 0.95), hole_dot((12, 16.4), 1.25))]


TRIANGLE = ("M10.25 4.6C11.05 3.25 12.95 3.25 13.75 4.6L21.05 17.3C21.85 18.65 20.9 20.25 19.35 20.25"
            "L4.65 20.25C3.1 20.25 2.15 18.65 2.95 17.3Z")


@icon("warning")
def _():
    return [S(*parse(TRIANGLE)), S(open_line((12, 9.0), (12, 13.75))), F(dot((12, 17.0), 1.15))]


@icon("warning-fill")
def _():
    tri = parse(TRIANGLE)[0][0]
    return [FS(tri, hole_line((12, 9.25), (12, 13.75), 0.9), hole_dot((12, 16.95), 1.2))]


# ---------------------------------------------------------------- helpers for overlaps, badges, slashes

def sdf_capsule_region(p0, p1, h):
    def f(p):
        dx, dy = p1[0] - p0[0], p1[1] - p0[1]
        t = max(0, min(1, ((p[0] - p0[0]) * dx + (p[1] - p0[1]) * dy) / (dx * dx + dy * dy)))
        return math.hypot(p[0] - p0[0] - dx * t, p[1] - p0[1] - dy * t) - h
    return f


def outline_sdf(f):
    """Distance to the outline of a region (for clearing strokes near a stroke)."""
    return lambda p: abs(f(p))


def keep_outside(contours, f, clear):
    """Stroke runs of the given contours with everything within `clear` of
    region f removed."""
    out = []
    for item in contours:
        segs, closed = item if isinstance(item, tuple) else (item, True)
        out += [(r, False) for r in clip_open_sdf(segs, lambda p: f(p) - clear, closed)]
    return out


BADGE_C = (17.6, 17.6)
BADGE_R = 3.9


def badge_clear(f_main_contours, c=BADGE_C, r=BADGE_R):
    return keep_outside(f_main_contours, sdf_circle(c, r), GAP)


def badge(kind, c=BADGE_C, r=BADGE_R, filled=False):
    """A small round badge at lower right: plus, minus, xmark, check."""
    cx, cy = c
    a = r * 0.5
    marks = {
        "plus": [open_line((cx - a, cy), (cx + a, cy)), open_line((cx, cy - a), (cx, cy + a))],
        "minus": [open_line((cx - a, cy), (cx + a, cy))],
        "xmark": [open_line((cx - a * 0.75, cy - a * 0.75), (cx + a * 0.75, cy + a * 0.75)),
                  open_line((cx + a * 0.75, cy - a * 0.75), (cx - a * 0.75, cy + a * 0.75))],
    }
    if not filled:
        return [S(circle(c, r)), S(*marks[kind])]
    rr = r - 0.1
    holes = {
        "plus": [cross(c, a, 0.75 + H, 0)],
        "minus": [hole_line((cx - a, cy), (cx + a, cy), 0.75)],
        "xmark": [cross(c, a * 1.05, 0.75 + H, math.pi / 4)],
    }[kind]
    return [FS(circle(c, rr), *holes)]


SLASH = ((3.75, 3.75), (20.25, 20.25))


def _slash_n():
    (x0, y0), (x1, y1) = SLASH
    m = math.hypot(x1 - x0, y1 - y0)
    return (-(y1 - y0) / m, (x1 - x0) / m)


def slash_dist(p):
    n = _slash_n()
    return (p[0] - SLASH[0][0]) * n[0] + (p[1] - SLASH[0][1]) * n[1]


def use_slash(shift=0.0):
    """Move the slash along its normal: positive moves it up and to the right."""
    global SLASH
    SLASH = ((3.75 + shift, 3.75 - shift), (20.25 + shift, 20.25 - shift))


def slashed_outline(contours):
    """Stroke runs with a clearing around the slash, plus the slash itself."""
    runs = keep_outside(contours, lambda p: abs(slash_dist(p)), GAP)
    return [S(*runs), S(open_line(*SLASH))]


def slashed_fill(contours, holes=()):
    """Filled shape cut in two along the slash, plus the slash itself."""
    g = 1.0 / 2 + 0.875 + 0.5  # visible clearance about 1 on each side of the slash
    g = H + 1.0 + H
    parts = []
    for side in (1, -1):
        for segs in contours:
            piece = clip_closed_sdf(segs, lambda p, s=side: s * slash_dist(p) - g)
            if piece:
                parts.append(piece)
    hole_parts = []
    for side in (1, -1):
        for segs in holes:
            piece = clip_closed_sdf(segs, lambda p, s=side: s * slash_dist(p) - g + 2 * H)
            if piece:
                hole_parts.append(piece)
    return [FS(*parts, *hole_parts), S(open_line(*SLASH))]


def ellipse(c, rx, ry):
    k = 0.5523
    cx, cy = c
    return [C((cx, cy - ry), (cx + rx * k, cy - ry), (cx + rx, cy - ry * k), (cx + rx, cy)),
            C((cx + rx, cy), (cx + rx, cy + ry * k), (cx + rx * k, cy + ry), (cx, cy + ry)),
            C((cx, cy + ry), (cx - rx * k, cy + ry), (cx - rx, cy + ry * k), (cx - rx, cy)),
            C((cx - rx, cy), (cx - rx, cy - ry * k), (cx - rx * k, cy - ry), (cx, cy - ry))]


def arc_arrow(c, r, a0, a1, head=2.0):
    """An arc from a0 to a1 with an arrowhead at a1."""
    arc = [A(c, r, a0, a1)]
    tip = (c[0] + r * math.cos(a1), c[1] + r * math.sin(a1))
    sgn = 1 if a1 > a0 else -1
    tx, ty = -math.sin(a1) * sgn, math.cos(a1) * sgn   # direction of travel at the tip
    def rot(x, y, d):
        cd, sd = math.cos(d), math.sin(d)
        return (x * cd - y * sd, x * sd + y * cd)
    b1 = rot(-tx, -ty, math.radians(42))
    b2 = rot(-tx, -ty, math.radians(-42))
    return [(arc, False), open_line((tip[0] + b1[0] * head, tip[1] + b1[1] * head), tip,
                                    (tip[0] + b2[0] * head, tip[1] + b2[1] * head))]


# ---------------------------------------------------------------- people

PERSON = dict(cx=12.0, head_cy=7.6, head_r=3.75, body_top=14.25, body_bottom=20.5, half_w=7.25)


def person_at(**kw):
    p = dict(PERSON)
    p.update(kw)
    return person_parts(**p), person_sdf(**p)


@icon("person")
def _():
    (head, body), _ = person_at()
    return [S(head, body)]


@icon("person-fill")
def _():
    (head, body), _ = person_at()
    return [FS(head), FS(body)]


TWO_FRONT = dict(cx=9.25, head_cy=8.5, head_r=3.4, body_top=14.6, body_bottom=20.5, half_w=6.5)
TWO_BACK = dict(cx=16.5, head_cy=7.25, head_r=3.0, body_top=12.75, body_bottom=18.75, half_w=5.25)


def two_people():
    (fh, fb), fs = person_at(**TWO_FRONT)
    (bh, bb), _ = person_at(**TWO_BACK)
    return (fh, fb), (bh, bb), fs


@icon("person-2")
def _():
    (fh, fb), (bh, bb), fs = two_people()
    back = keep_outside([bh, bb], fs, GAP)
    return [S(fh, fb), S(*back)]


@icon("person-2-fill")
def _():
    (fh, fb), (bh, bb), fs = two_people()
    g = H + 1.0 + H
    # the back person is cut along the front person's outline grown by the clearance:
    # its head along the front head, its body along the front body
    f = TWO_FRONT
    head_out = circle((f["cx"], f["head_cy"]), f["head_r"] + g)
    hgt = f["body_bottom"] - f["body_top"]
    r = min(f["half_w"], hgt + 3) * 0.95
    body_out = rrect(f["cx"] - f["half_w"] - g, f["body_top"] - g, 2 * f["half_w"] + 2 * g, hgt + 6 + 2 * g, r + g)
    back_f = person_sdf(**TWO_BACK)
    pieces = [clip_closed_sdf_bridged(bh, lambda q: fs(q) - g, bridge_along(head_out, back_f)),
              clip_closed_sdf_bridged(bb, lambda q: fs(q) - g, bridge_along(body_out, back_f))]
    return [FS(fh), FS(fb), FS(*[p for p in pieces if p])]


# ---- person-3: three heads that do not touch; only the side bodies go behind the middle one

MID3 = dict(cx=12.0, head_cy=6.6, head_r=2.9, body_top=12.9, body_bottom=20.5, half_w=5.75)
SIDE3 = dict(head_cy=9.6, head_r=2.2, body_top=14.6, body_bottom=19.0, half_w=2.9)


def three_people_v2():
    left = dict(SIDE3, cx=4.9)
    right = dict(SIDE3, cx=19.1)
    (mh, mb), _ = person_at(**MID3)
    (lh, lb), _ = person_at(**left)
    (rh, rb), _ = person_at(**right)
    m = MID3
    box = (m["cx"] - m["half_w"], m["body_top"], 2 * m["half_w"], m["body_bottom"] - m["body_top"] + 6,
           m["half_w"] * 0.95)
    return (mh, mb), (lh, lb), (rh, rb), sdf_rrect(*box), box


@icon("person-3")
def _():
    (mh, mb), (lh, lb), (rh, rb), mbody, _ = three_people_v2()
    return [S(mh, mb), S(lh, rh), S(*keep_outside([lb, rb], mbody, GAP))]


@icon("person-3-fill")
def _():
    (mh, mb), (lh, lb), (rh, rb), mbody, (x, y, w, h, r) = three_people_v2()
    g = H + 1.0 + H
    grown = rrect(x - g, y - g, w + 2 * g, h + 2 * g, r + g)
    parts = [clip_closed_sdf_bridged(s, lambda q: mbody(q) - g, bridge_along(grown, person_sdf(**dict(SIDE3, cx=cx))))
             for s, cx in ((lb, 4.9), (rb, 19.1))]
    return [FS(mh), FS(mb), FS(lh, rh), FS(*[p for p in parts if p])]


def person_with_badge(kind, filled=False):
    (head, body), _ = person_at(cx=10.25, head_cy=7.6, head_r=3.6, body_top=14.25, half_w=6.75)
    if filled:
        g = H + 1.0 + H
        f = sdf_circle(BADGE_C, BADGE_R)
        parts = [p for p in (clip_closed_sdf(s, lambda q: f(q) - g) for s in (head, body)) if p]
        return [FS(*parts)] + badge(kind, filled=True)
    return [S(*badge_clear([head, body]))] + badge(kind)


@icon("person-plus")
def _():
    return person_with_badge("plus")


@icon("person-minus")
def _():
    return person_with_badge("minus")


def person_circle_parts():
    return [circle((12, 12), 9.25), circle((12, 9.6), 3.4),
            ([C((6.15, 18.85), (7.45, 16.6), (9.55, 15.35), (12, 15.35)),
              C((12, 15.35), (14.45, 15.35), (16.55, 16.6), (17.85, 18.85))], False)]


@icon("person-circle-xmark")
def _():
    return [S(*badge_clear(person_circle_parts()))] + badge("xmark")


@icon("person-circle-plus")
def _():
    return [S(*badge_clear(person_circle_parts()))] + badge("plus")


# ---- passkey: a person and a key with a clear bit (two teeth), so it never reads as a pin

def badge_key(cx=18.25, bow_cy=10.4, bow_r=2.45, bottom=20.75):
    y0 = bow_cy + bow_r
    shaft = open_line((cx, y0), (cx, bottom))
    teeth = [open_line((cx, bottom - 3.0), (cx + 2.1, bottom - 3.0)), open_line((cx, bottom), (cx + 2.1, bottom))]
    region = sdf_union(sdf_circle((cx, bow_cy), bow_r), sdf_capsule_region((cx, y0), (cx, bottom), 0.0),
                       sdf_capsule_region((cx, bottom - 3.0), (cx + 2.1, bottom - 3.0), 0.0))
    return [circle((cx, bow_cy), bow_r), shaft] + teeth, region


@icon("passkey")
def _():
    (head, body), _ = person_at(cx=9.25, head_cy=7.6, head_r=3.6, body_top=14.25, half_w=6.75)
    key, keyf = badge_key()
    return [S(*keep_outside([head, body], keyf, GAP)), S(*key)]


def gear_small(c, r_out, r_in, teeth=8):
    segs = []
    pts = []
    half_top, half_base = 0.17, 0.3
    for k in range(teeth):
        th = -math.pi / 2 + k * TAU / teeth
        pts += [(th - half_base, r_in), (th - half_top, r_out), (th + half_top, r_out), (th + half_base, r_in)]
    P = [(c[0] + r * math.cos(a), c[1] + r * math.sin(a)) for a, r in pts]
    return poly(P)


@icon("person-2-gear")
def _():
    (fh, fb), (bh, bb), fs = two_people()
    shift = lambda segs: xform_segs(segs, 1.0, -0.75, 0)
    fh, fb, bh, bb = shift(fh), shift(fb), shift(bh), shift(bb)
    fs2 = lambda p: fs((p[0] + 0.75, p[1]))
    back = keep_outside([bh, bb], fs2, GAP)
    gc = (18.0, 17.75)
    gf = sdf_circle(gc, 4.0)
    front = keep_outside([fh, fb], gf, GAP)
    back = keep_outside(back, gf, GAP)
    return [S(*front), S(*back), FS(gear_small(gc, 3.6, 2.7), circle_ccw(gc, 0.9 + H))]


@icon("person-question")
def _():
    (head, body), _ = person_at(cx=10.0, head_cy=7.75, head_r=3.6, body_top=14.25, half_w=6.75)
    q = ("M16.6 5.4C16.75 4.15 17.75 3.25 19.05 3.25C20.4 3.25 21.4 4.2 21.4 5.45C21.4 6.45 20.85 7.0 20.05 7.5"
         "C19.4 7.9 19.05 8.35 19.05 9.1")
    return [FS(head), FS(body), S(*parse(q)), F(dot((19.05, 11.6), 1.1))]


# ---------------------------------------------------------------- calls

@icon("phone-down")
def _():
    return [S(*parse(HANDSET_DOWN))]


@icon("phone-down-fill")
def _():
    return [FS(*parse(HANDSET_DOWN))]


def small_handset():
    return xform_segs(contour(HANDSET), 0.8, -0.2, 3.8)


@icon("phone-outgoing")
def _():
    return [S(small_handset()), S(open_line((14.25, 9.75), (20.25, 3.75)), open_line((15.25, 3.75), (20.25, 3.75), (20.25, 8.75)))]


@icon("phone-incoming")
def _():
    return [S(small_handset()), S(open_line((20.25, 3.75), (14.25, 9.75)), open_line((14.25, 4.75), (14.25, 9.75), (19.25, 9.75)))]


@icon("phone-connection-fill")
def _():
    waves = [(arc_c, r) for arc_c, r in (((13.0, 11.0), 4.25), ((13.0, 11.0), 7.75))]
    hs = small_handset()
    return [FS(hs), S(*[([A(c, r, math.radians(-80), math.radians(-10))], False) for c, r in waves])]


LENS = "M17.4 10.2L19.75 8.6C20.4 8.15 21.25 8.6 21.25 9.4V14.6C21.25 15.4 20.4 15.85 19.75 15.4L17.4 13.8Z"


@icon("video")
def _():
    return [S(rrect(2.75, 6.25, 12.25, 11.5, 2.75)), S(*parse(LENS))]


@icon("video-fill")
def _():
    return [FS(rrect(2.75, 6.25, 12.25, 11.5, 2.75)), FS(*parse(LENS))]


@icon("video-slash")
def _():
    use_slash(0.0)
    return slashed_outline([rrect(2.75, 6.25, 12.25, 11.5, 2.75), parse(LENS)[0][0]])


@icon("video-slash-fill")
def _():
    use_slash(0.0)
    return slashed_fill([rrect(2.75, 6.25, 12.25, 11.5, 2.75), parse(LENS)[0][0]])


MIC_BODY = rrect(8.75, 2.75, 6.5, 11.75, 3.25)
MIC_STAND = parse("M5.75 11.0C5.75 14.65 8.55 17.5 12 17.5C15.45 17.5 18.25 14.65 18.25 11.0")[0]
MIC_STEM = ([L((12, 17.5), (12, 21.0))], False)


@icon("mic")
def _():
    return [S(MIC_BODY), S(MIC_STAND, MIC_STEM)]


@icon("mic-fill")
def _():
    return [FS(MIC_BODY), S(MIC_STAND, MIC_STEM)]


@icon("mic-slash")
def _():
    use_slash(1.0)
    runs = drop_short_runs(keep_outside([MIC_BODY, MIC_STAND, MIC_STEM], lambda p: abs(slash_dist(p)), GAP))
    return [S(*runs), S(open_line(*SLASH))]


@icon("display")
def _():
    return [S(rrect(2.75, 3.75, 18.5, 12.75, 2.25)), S(open_line((12, 16.5), (12, 20.0)), open_line((7.75, 20.25), (16.25, 20.25)))]


CAMERA_BODY = ("M3.25 9.4C3.25 8.15 4.25 7.15 5.5 7.15H7.55L8.85 5.15C9.25 4.55 9.9 4.25 10.6 4.25H13.4"
               "C14.1 4.25 14.75 4.55 15.15 5.15L16.45 7.15H18.5C19.75 7.15 20.75 8.15 20.75 9.4V17.75"
               "C20.75 19.0 19.75 20.0 18.5 20.0H5.5C4.25 20.0 3.25 19.0 3.25 17.75Z")


@icon("camera-flip")
def _():
    return [S(*parse(CAMERA_BODY)), S(*arc_arrow((12, 13.0), 3.75, math.radians(195), math.radians(335), 1.7)),
            S(*arc_arrow((12, 13.0), 3.75, math.radians(15), math.radians(155), 1.7))]


@icon("camera")
def _():
    return [S(*parse(CAMERA_BODY)), S(circle((12, 13.1), 3.6))]


SPEAKER = ("M3.75 9.75C3.75 9.2 4.2 8.75 4.75 8.75H7.75L12.4 4.85C13.05 4.3 14.0 4.75 14.0 5.6V18.4"
           "C14.0 19.25 13.05 19.7 12.4 19.15L7.75 15.25H4.75C4.2 15.25 3.75 14.8 3.75 14.25Z")


@icon("speaker")
def _():
    return [S(*parse(SPEAKER)),
            S(([A((13.75, 12), 4.0, math.radians(-42), math.radians(42))], False),
              ([A((13.75, 12), 7.25, math.radians(-45), math.radians(45))], False))]


@icon("speaker-slash-fill")
def _():
    use_slash(0.0)
    return slashed_fill([parse(SPEAKER)[0][0]])


@icon("layout-grid")
def _():
    return [S(rrect(3.75, 3.75, 7.0, 7.0, 1.75), rrect(13.25, 3.75, 7.0, 7.0, 1.75),
              rrect(3.75, 13.25, 7.0, 7.0, 1.75), rrect(13.25, 13.25, 7.0, 7.0, 1.75))]


@icon("layout-focus")
def _():
    return [S(rrect(3.75, 3.5, 4.5, 3.75, 1.0), rrect(9.75, 3.5, 4.5, 3.75, 1.0), rrect(15.75, 3.5, 4.5, 3.75, 1.0),
              rrect(3.75, 10.0, 16.5, 10.5, 2.0))]


# ---- hand-raised: four fingers side by side, one line between neighbours, thumb out to the left

HAND = dict(fw=3.1, x_left=7.1, tops=(4.7, 2.75, 3.4, 5.7), valley=12.25,
            thumb_tip=(4.95, 13.6), thumb_base=(8.9, 18.0), right_down=15.25, bottom=(13.4, 21.25))


def hand_outline(fw, x_left, tops, valley, thumb_tip, thumb_base, right_down, bottom):
    r = fw / 2
    segs = []
    # thumb: lower edge up to the tip, round the tip, upper edge back to the index finger
    dx, dy = thumb_tip[0] - thumb_base[0], thumb_tip[1] - thumb_base[1]
    m = math.hypot(dx, dy)
    ux, uy = dx / m, dy / m
    lo = (-uy * r, ux * r) if (-uy) < 0 else (uy * r, -ux * r)     # offset towards lower left
    up = (-lo[0], -lo[1])
    lo_b = (thumb_base[0] + lo[0], thumb_base[1] + lo[1])
    lo_t = (thumb_tip[0] + lo[0], thumb_tip[1] + lo[1])
    up_t = (thumb_tip[0] + up[0], thumb_tip[1] + up[1])
    # where the thumb's upper edge meets the index finger's left side
    t = (x_left - up_t[0]) / (thumb_base[0] - thumb_tip[0])
    join = (x_left, up_t[1] + t * (thumb_base[1] - thumb_tip[1]))
    a_lo = math.atan2(lo[1], lo[0])
    a_up = math.atan2(up[1], up[0])
    if a_up < a_lo:
        a_up += TAU
    segs.append(L(lo_b, lo_t))
    segs.append(A(thumb_tip, r, a_lo, a_up))
    segs.append(L(up_t, join))
    # fingers
    x = x_left
    for i, top in enumerate(tops):
        cy = top + r
        y_from = join[1] if i == 0 else valley
        segs.append(L((x, y_from), (x, cy)))
        segs.append(A((x + r, cy), r, math.pi, TAU))
        x += fw
        if i < len(tops) - 1:
            segs.append(L((x, cy), (x, valley)))
    # right side and palm
    segs.append(L((x, top + r), (x, right_down)))
    bx, by = bottom
    segs.append(C((x, right_down), (x, by - 2.3), (bx + 3.2, by), (bx, by)))
    segs.append(C((bx, by), (bx - 2.1, by), (lo_b[0] + 1.2, lo_b[1] + 1.55), lo_b))
    return segs




# ---------------------------------------------------------------- editing and message actions

def pencil_outline(tip, end, w=1.9, cone=3.3):
    ux, uy = end[0] - tip[0], end[1] - tip[1]
    m = math.hypot(ux, uy)
    ux, uy = ux / m, uy / m
    nx, ny = -uy, ux
    s = (tip[0] + ux * cone, tip[1] + uy * cone)
    a = math.atan2(ny, nx)
    body = [L(tip, (s[0] + nx * w, s[1] + ny * w)), L((s[0] + nx * w, s[1] + ny * w), (end[0] + nx * w, end[1] + ny * w)),
            A(end, w, a, a - math.pi),
            L((end[0] - nx * w, end[1] - ny * w), (s[0] - nx * w, s[1] - ny * w)), L((s[0] - nx * w, s[1] - ny * w), tip)]
    e2 = (end[0] - ux * 2.6, end[1] - uy * 2.6)
    ferrule = open_line((e2[0] + nx * w, e2[1] + ny * w), (e2[0] - nx * w, e2[1] - ny * w))
    return body, ferrule


@icon("pencil")
def _():
    body, ferrule = pencil_outline((4.25, 19.75), (17.75, 6.25))
    return [S(body), S(ferrule)]


@icon("compose")
def _():
    frame = parse("M11.25 4.25H7.0C5.5 4.25 4.25 5.5 4.25 7.0V17.0C4.25 18.5 5.5 19.75 7.0 19.75H17.0"
                  "C18.5 19.75 19.75 18.5 19.75 17.0V12.75")[0]
    body, ferrule = pencil_outline((10.25, 13.75), (19.0, 5.0), 1.6, 2.6)
    return [S(frame), S(body)]


@icon("trash")
def _():
    return [S(open_line((3.75, 6.25), (20.25, 6.25))),
            S(*parse("M9.0 6.25V4.75C9.0 4.05 9.55 3.5 10.25 3.5H13.75C14.45 3.5 15.0 4.05 15.0 4.75V6.25")),
            S(*parse("M5.75 6.25L6.8 18.55C6.9 19.65 7.8 20.5 8.9 20.5H15.1C16.2 20.5 17.1 19.65 17.2 18.55L18.25 6.25")),
            S(open_line((10.0, 10.0), (10.25, 16.75)), open_line((14.0, 10.0), (13.75, 16.75)))]


def two_sheets(back, front):
    bf = sdf_rrect(*front)
    return [S(rrect(*front)), S(*keep_outside([rrect(*back)], bf, GAP))]


@icon("copy")
def _():
    return two_sheets((4.0, 3.5, 11.0, 13.5, 2.25), (9.0, 7.0, 11.0, 13.5, 2.25))


@icon("paste")
def _():
    clip = rrect(8.5, 2.75, 7.0, 3.5, 1.25)
    board = keep_outside([rrect(4.75, 4.5, 14.5, 16.25, 2.25)], sdf_rrect(8.5, 2.75, 7.0, 3.5, 1.25), GAP)
    return [S(clip), S(*board), S(open_line((8.5, 11.0), (15.5, 11.0)), open_line((8.5, 14.75), (13.25, 14.75)))]


@icon("search")
def _():
    return [S(circle((10.5, 10.5), 6.75)), S(open_line((15.4, 15.4), (20.25, 20.25)))]


@icon("zoom-in")
def _():
    return [S(circle((10.5, 10.5), 6.75)), S(open_line((15.4, 15.4), (20.25, 20.25)),
            open_line((7.5, 10.5), (13.5, 10.5)), open_line((10.5, 7.5), (10.5, 13.5)))]


@icon("zoom-out")
def _():
    return [S(circle((10.5, 10.5), 6.75)), S(open_line((15.4, 15.4), (20.25, 20.25)), open_line((7.5, 10.5), (13.5, 10.5)))]


# ---- link: two links, interlocked (each passes over the other once)

LINK = dict(w=2.85, dl=0.7, half_gap=1.3, lc=6.4)


def link_pair(w, dl, half_gap, lc, centre=(12.0, 12.0)):
    u = (1 / math.sqrt(2), -1 / math.sqrt(2))   # along the chain, up and to the right
    v = (1 / math.sqrt(2), 1 / math.sqrt(2))    # across it

    def P(x, y):
        return (centre[0] + x * u[0] + y * v[0], centre[1] + x * u[1] + y * v[1])
    a0, a1 = P(-half_gap - lc, -dl), P(-half_gap, -dl)
    b0, b1 = P(half_gap, dl), P(half_gap + lc, dl)
    return (capsule(a0, a1, w), capsule(b0, b1, w),
            sdf_capsule_region(a0, a1, w), sdf_capsule_region(b0, b1, w), P)


def outline_crossings(segs, f, n=2000):
    """Points of a closed outline where region f's boundary crosses it."""
    from geom import _point_at
    pts = [_point_at(s, k / n) for s in segs for k in range(n)]
    out = []
    for i in range(len(pts)):
        if (f(pts[i - 1]) < 0) != (f(pts[i]) < 0):
            out.append(pts[i])
    return out


def interlocked_links(**kw):
    """Stroke runs of the two links, each cut where it passes under the other."""
    a, b, fa, fb, P = link_pair(**kw)
    xs = outline_crossings(a, fb)          # the two places the outlines cross
    over_a, over_b = sorted(xs, key=lambda q: q[0] + q[1])   # lower-right one: B passes over A
    near = lambda q, c: math.hypot(q[0] - c[0], q[1] - c[1]) < 3.2  # noqa: E731
    a_runs = keep_outside([a], lambda q: abs(fb(q)) if near(q, over_b) else 99, GAP)
    b_runs = keep_outside([b], lambda q: abs(fa(q)) if near(q, over_a) else 99, GAP)
    return a_runs, b_runs


@icon("link")
def _():
    a_runs, b_runs = interlocked_links(**LINK)
    return [S(*a_runs), S(*b_runs)]


@icon("link-plus")
def _():
    a_runs, b_runs = interlocked_links(**dict(LINK, lc=4.9, centre=(10.0, 10.0)))
    runs = a_runs + b_runs
    return [S(*keep_outside(runs, sdf_circle(BADGE_C, BADGE_R), GAP))] + badge("plus")


def finder(x, y):
    return [S(rrect(x, y, 6.5, 6.5, 1.75)), F(rrect(x + 2.25, y + 2.25, 2.0, 2.0, 0.5))]


@icon("qrcode")
def _():
    els = finder(3.75, 3.75) + finder(13.75, 3.75) + finder(3.75, 13.75)
    mods = [rrect(x, y, 2.5, 2.5, 0.6) for x, y in ((13.25, 13.25), (18.25, 13.25), (15.75, 15.75), (13.25, 18.25), (18.25, 18.25))]
    return els + [F(*mods)]


@icon("qrcode-scan")
def _():
    r = 2.25
    corners = [
        parse(f"M3.75 8.25V6.0C3.75 4.75 4.75 3.75 6.0 3.75H8.25")[0],
        parse(f"M15.75 3.75H18.0C19.25 3.75 20.25 4.75 20.25 6.0V8.25")[0],
        parse(f"M20.25 15.75V18.0C20.25 19.25 19.25 20.25 18.0 20.25H15.75")[0],
        parse(f"M8.25 20.25H6.0C4.75 20.25 3.75 19.25 3.75 18.0V15.75")[0],
    ]
    mods = [rrect(7.5, 7.5, 3.5, 3.5, 0.75), rrect(13.0, 7.5, 3.5, 3.5, 0.75), rrect(7.5, 13.0, 3.5, 3.5, 0.75),
            rrect(13.25, 13.25, 1.6, 1.6, 0.4), rrect(14.9, 14.9, 1.6, 1.6, 0.4)]
    return [S(*corners), F(*mods)]


BOX_OPEN = parse("M8.75 9.5H7.0C5.9 9.5 5.0 10.4 5.0 11.5V18.25C5.0 19.35 5.9 20.25 7.0 20.25H17.0"
                 "C18.1 20.25 19.0 19.35 19.0 18.25V11.5C19.0 10.4 18.1 9.5 17.0 9.5H15.25")[0]


@icon("share")
def _():
    return [S(BOX_OPEN), S(open_line((12, 3.5), (12, 14.0)), open_line((8.6, 6.9), (12, 3.5), (15.4, 6.9)))]


@icon("download")
def _():
    return [S(BOX_OPEN), S(open_line((12, 3.5), (12, 14.0)), open_line((8.6, 10.6), (12, 14.0), (15.4, 10.6)))]


REPLY = ("M9.9 4.6L3.7 10.15C3.25 10.55 3.25 11.25 3.7 11.65L9.9 17.2C10.3 17.55 10.9 17.3 10.9 16.75"
         "V13.4C15.3 13.4 18.35 14.85 20.4 18.55C20.6 18.9 21.1 18.75 21.05 18.35"
         "C20.35 12.5 16.95 8.75 10.9 8.4V5.05C10.9 4.5 10.3 4.25 9.9 4.6Z")


@icon("reply")
def _():
    return [S(*parse(REPLY))]


@icon("forward")
def _():
    return [S(xform_segs(parse(REPLY)[0][0], mirror_x=12.0))]


PIN = ("M8.5 3.75H15.5C16.15 3.75 16.5 4.5 16.1 5.0L14.75 6.75V10.25L17.55 12.4C18.1 12.8 17.8 13.75 17.1 13.75"
       "H6.9C6.2 13.75 5.9 12.8 6.45 12.4L9.25 10.25V6.75L7.9 5.0C7.5 4.5 7.85 3.75 8.5 3.75Z")
PIN_NEEDLE = ([L((12, 13.75), (12, 20.75))], False)


@icon("pin")
def _():
    return [S(*parse(PIN)), S(PIN_NEEDLE)]


@icon("pin-fill")
def _():
    return [FS(*parse(PIN)), S(PIN_NEEDLE)]


@icon("pin-slash")
def _():
    use_slash(0.0)
    return slashed_outline([parse(PIN)[0][0], PIN_NEEDLE])


BOOKMARK = ("M8.0 3.75H16.0C17.1 3.75 18.0 4.65 18.0 5.75V19.5C18.0 20.15 17.3 20.5 16.8 20.15L12.0 16.6"
            "L7.2 20.15C6.7 20.5 6.0 20.15 6.0 19.5V5.75C6.0 4.65 6.9 3.75 8.0 3.75Z")


@icon("bookmark")
def _():
    return [S(*parse(BOOKMARK))]


@icon("bookmark-fill")
def _():
    return [FS(*parse(BOOKMARK))]


@icon("bookmark-slash")
def _():
    use_slash(0.0)
    return slashed_outline([parse(BOOKMARK)[0][0]])


SQ_BUBBLE = ("M7.0 3.75H17.0C18.8 3.75 20.25 5.2 20.25 7.0V13.25C20.25 15.05 18.8 16.5 17.0 16.5H11.0"
             "L7.15 19.75C6.6 20.2 5.75 19.8 5.75 19.1V16.35C4.6 15.95 3.75 14.85 3.75 13.5V7.0"
             "C3.75 5.2 5.2 3.75 7.0 3.75Z")


@icon("translate")
def _():
    return [S(*parse(SQ_BUBBLE)), S(open_line((8.75, 13.25), (12, 6.75), (15.25, 13.25)), open_line((10.0, 10.9), (14.0, 10.9)))]


@icon("text-bubble")
def _():
    return [S(*parse(SQ_BUBBLE)), S(open_line((8.0, 8.5), (16.0, 8.5)), open_line((8.0, 11.75), (13.5, 11.75)))]


@icon("checkmark-message")
def _():
    return [S(*parse(BUBBLE)), S(open_line((8.4, 11.3), (11.0, 13.9), (15.8, 8.6)))]


@icon("nosign")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((5.6, 5.6), (18.4, 18.4)))]


@icon("archive")
def _():
    return [S(rrect(3.25, 4.0, 17.5, 4.75, 1.4)),
            S(*parse("M4.75 8.75V17.75C4.75 19.1 5.9 20.25 7.25 20.25H16.75C18.1 20.25 19.25 19.1 19.25 17.75V8.75")),
            S(open_line((10.0, 12.5), (14.0, 12.5)))]


@icon("archive-fill")
def _():
    lid = rrect(3.25, 3.75, 17.5, 4.25, 1.4)
    body = parse("M4.75 10.75H19.25V17.75C19.25 19.1 18.1 20.25 16.75 20.25H7.25C5.9 20.25 4.75 19.1 4.75 17.75Z")[0][0]
    return [FS(lid), FS(body, hole_line((10.0, 13.75), (14.0, 13.75), 0.75))]


@icon("unarchive")
def _():
    tray = parse("M3.75 13.5V18.0C3.75 19.25 4.75 20.25 6.0 20.25H18.0C19.25 20.25 20.25 19.25 20.25 18.0V13.5"
                 "H15.6L14.5 15.6H9.5L8.4 13.5Z")
    return [S(*tray), S(open_line((12, 3.75), (12, 11.75)), open_line((8.6, 7.15), (12, 3.75), (15.4, 7.15)))]


BELL = ("M12.0 3.0C8.65 3.0 6.4 5.55 6.4 8.9V12.5C6.4 13.5 6.0 14.45 5.3 15.15L4.7 15.75C4.05 16.4 4.5 17.5 5.45 17.5"
        "H18.55C19.5 17.5 19.95 16.4 19.3 15.75L18.7 15.15C18.0 14.45 17.6 13.5 17.6 12.5V8.9C17.6 5.55 15.35 3.0 12.0 3.0Z")
CLAPPER = parse("M9.75 19.75C10.25 20.6 11.05 21.1 12.0 21.1C12.95 21.1 13.75 20.6 14.25 19.75")[0]


@icon("bell")
def _():
    return [S(*parse(BELL)), S(CLAPPER)]


@icon("bell-slash")
def _():
    use_slash(0.0)
    return slashed_outline([parse(BELL)[0][0], CLAPPER])


@icon("bell-slash-fill")
def _():
    use_slash(0.0)
    out = slashed_fill([parse(BELL)[0][0]])
    return out + [S(*keep_outside([CLAPPER], lambda p: abs(slash_dist(p)), GAP))]


FOLDER = ("M3.75 7.0V17.75C3.75 19.1 4.9 20.25 6.25 20.25H17.75C19.1 20.25 20.25 19.1 20.25 17.75V9.75"
          "C20.25 8.4 19.1 7.25 17.75 7.25H11.5L10.0 5.35C9.6 4.85 9.05 4.6 8.45 4.6H6.15C4.8 4.6 3.75 5.65 3.75 7.0Z")
FOLDER_LINE = ([L((3.75, 10.25), (20.25, 10.25))], False)


@icon("folder")
def _():
    return [S(*parse(FOLDER)), S(FOLDER_LINE)]


def folder_badge(kind):
    runs = badge_clear([parse(FOLDER)[0][0], FOLDER_LINE])
    return [S(*runs)] + badge(kind)


@icon("folder-plus")
def _():
    return folder_badge("plus")


@icon("folder-minus")
def _():
    return folder_badge("minus")


@icon("list")
def _():
    return [F(dot((5.0, 6.5), 1.25), dot((5.0, 12.0), 1.25), dot((5.0, 17.5), 1.25)),
            S(open_line((9.25, 6.5), (20.25, 6.5)), open_line((9.25, 12.0), (20.25, 12.0)), open_line((9.25, 17.5), (20.25, 17.5)))]


@icon("chart-bar")
def _():
    # ADR-0092: a poll, as horizontal result bars of three lengths.
    return [S(open_line((4.75, 6.5), (13.5, 6.5)), open_line((4.75, 12.0), (19.25, 12.0)),
              open_line((4.75, 17.5), (9.75, 17.5)))]


@icon("checklist")
def _():
    # ADR-0092: a checklist, two ticked rows and an open one.
    return [S(open_line((3.75, 6.75), (5.25, 8.25), (8.25, 5.25)),
              open_line((3.75, 12.75), (5.25, 14.25), (8.25, 11.25)),
              open_line((11.25, 6.75), (20.25, 6.75)), open_line((11.25, 12.75), (20.25, 12.75)),
              open_line((11.25, 18.75), (20.25, 18.75)), open_line((4.5, 18.75), (7.5, 18.75)))]


@icon("drag-handle")
def _():
    return [S(open_line((4.5, 7.5), (19.5, 7.5)), open_line((4.5, 12.0), (19.5, 12.0)), open_line((4.5, 16.5), (19.5, 16.5)))]


@icon("clock")
def _():
    return [S(circle((12, 12), 9.25)), S(open_line((12, 6.75), (12, 12.0), (15.6, 14.1)))]


EYE = ("M2.75 12.0C4.95 7.75 8.3 5.25 12.0 5.25C15.7 5.25 19.05 7.75 21.25 12.0C19.05 16.25 15.7 18.75 12.0 18.75"
       "C8.3 18.75 4.95 16.25 2.75 12.0Z")


@icon("eye")
def _():
    return [S(*parse(EYE)), S(circle((12, 12), 3.25))]


@icon("eye-slash")
def _():
    use_slash(0.0)
    return slashed_outline([parse(EYE)[0][0], circle((12, 12), 3.25)])


# ---------------------------------------------------------------- security

LOCK_BODY = rrect(5.0, 10.25, 14.0, 10.5, 2.5)
SHACKLE = parse("M8.0 10.25V7.5C8.0 5.3 9.8 3.5 12.0 3.5C14.2 3.5 16.0 5.3 16.0 7.5V10.25")[0]


@icon("lock")
def _():
    return [S(LOCK_BODY), S(SHACKLE)]


@icon("lock-fill")
def _():
    return [FS(LOCK_BODY), S(SHACKLE)]


# ---- slashed fills without leftover crumbs

def drop_short_runs(runs, min_len=2.6):
    """Remove stroke runs shorter than min_len (crumbs left beside a cut)."""
    from geom import _point_at
    out = []
    for segs, closed in runs:
        pts = [_point_at(s, k / 20) for s in segs for k in range(21)]
        ln = sum(math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]) for i in range(1, len(pts)))
        if ln >= min_len:
            out.append((segs, closed))
    return out


@icon("lock-slash-fill")
def _():
    use_slash(-1.0)
    out = slashed_fill([LOCK_BODY])
    shackle = drop_short_runs(keep_outside([SHACKLE], lambda p: abs(slash_dist(p)), GAP))
    return out + [S(*shackle)]


@icon("mic-slash-fill")
def _():
    use_slash(1.0)
    out = slashed_fill([MIC_BODY])
    stand = drop_short_runs(keep_outside([MIC_STAND, MIC_STEM], lambda p: abs(slash_dist(p)), GAP))
    return out + [S(*stand)]


def mini_lock(cx, top, w=5.5, h=4.5, r=1.1):
    body = rrect(cx - w / 2, top + 2.9, w, h, r)
    sh = parse(f"M{cx - 1.6} {top + 2.9}V{top + 1.6}C{cx - 1.6} {top + 0.7} {cx - 0.9} {top} {cx} {top}"
               f"C{cx + 0.9} {top} {cx + 1.6} {top + 0.7} {cx + 1.6} {top + 1.6}V{top + 2.9}")[0]
    return body, sh


@icon("lock-rotation")
def _():
    body, sh = mini_lock(12, 8.4)
    return [S(*arc_arrow((12, 12), 8.75, math.radians(-60), math.radians(240), 2.4)), F(body), S(sh)]


SHIELD = ("M11.4 3.2L5.45 5.55C4.75 5.85 4.25 6.55 4.25 7.3V11.6C4.25 16.0 7.4 19.65 11.6 20.95"
          "C11.85 21.05 12.15 21.05 12.4 20.95C16.6 19.65 19.75 16.0 19.75 11.6V7.3C19.75 6.55 19.25 5.85 18.55 5.55"
          "L12.6 3.2C12.2 3.05 11.8 3.05 11.4 3.2Z")


@icon("shield")
def _():
    return [S(*parse(SHIELD))]


@icon("shield-check")
def _():
    return [S(*parse(SHIELD)), S(open_line((8.6, 12.0), (11.0, 14.4), (15.5, 9.4)))]


@icon("shield-exclamation")
def _():
    return [S(*parse(SHIELD)), S(open_line((12, 7.6), (12, 12.4))), F(dot((12, 15.6), 1.15))]


@icon("lock-shield")
def _():
    body, sh = mini_lock(12, 7.25)
    return [S(*parse(SHIELD)), F(body), S(sh)]


# ---- network-shield: the globe with a half-filled shield badge, like the iPhone's

@icon("network-shield")
def _():
    g = globe_parts()
    s, sc = 0.47, (16.9, 16.6)
    shield = xform_segs(parse(SHIELD)[0][0], s, sc[0] - 12 * s, sc[1] - 12 * s)
    clear = sdf_rrect(sc[0] - 3.65, sc[1] - 4.2, 7.3, 8.6, 3.0)
    right_half = clip_closed_sdf(shield, lambda p: p[0] - sc[0])
    return [S(*drop_short_runs(keep_outside(g, clear, GAP))), S(shield), FS(right_half)]


def globe_parts():
    lens = parse("M12.0 2.75C9.45 5.3 8.15 8.55 8.15 12.0C8.15 15.45 9.45 18.7 12.0 21.25"
                 "C14.55 18.7 15.85 15.45 15.85 12.0C15.85 8.55 14.55 5.3 12.0 2.75Z")[0][0]
    return [circle((12, 12), 9.25), lens, ([L((2.75, 12), (21.25, 12))], False),
            ([L((4.4, 7.6), (19.6, 7.6))], False), ([L((4.4, 16.4), (19.6, 16.4))], False)]


@icon("globe")
def _():
    return [S(*globe_parts())]


KEY_D = ("M13.75 10.6V12.75L12.85 13.65L13.75 14.55V15.4L12.85 16.3L13.75 17.2V18.9L12.0 20.65L10.25 18.9V10.6")


@icon("key")
def _():
    c = (12, 6.9)
    r = 4.1
    a = math.atan2(10.6 - 6.9, 1.75)
    head = [A(c, r, a, math.pi - a - TAU)]
    stem = parse(KEY_D)[0][0]
    return [S(head + stem), S(circle((12, 5.4), 1.2))]


# ---- lifepreserver: ring and hole, with four solid straps (no thin double lines)

@icon("lifepreserver")
def _():
    c, ro, ri = (12, 12), 9.25, 4.25
    straps = []
    for k in range(4):
        th = math.radians(45 + 90 * k)
        half_out, half_in = math.radians(6.5), math.radians(11)
        # a strap is narrower in angle at the rim than at the hole, so its sides stay close to parallel
        p = lambda r, a: (c[0] + r * math.cos(a), c[1] + r * math.sin(a))  # noqa: E731
        straps.append([A(c, ro, th - half_out, th + half_out), L(p(ro, th + half_out), p(ri, th + half_in)),
                       A(c, ri, th + half_in, th - half_in), L(p(ri, th - half_in), p(ro, th - half_out))])
    return [S(circle(c, ro)), S(circle(c, ri)), FS(*straps)]


@icon("words")
def _():
    return [S(open_line((3.75, 7.0), (8.25, 7.0)), open_line((11.5, 7.0), (20.25, 7.0)),
              open_line((3.75, 12.0), (12.0, 12.0)), open_line((15.25, 12.0), (20.25, 12.0)),
              open_line((3.75, 17.0), (6.75, 17.0)), open_line((10.0, 17.0), (16.0, 17.0)))]


@icon("envelope")
def _():
    return [S(rrect(3.0, 5.25, 18.0, 13.5, 2.5)),
            S(*parse("M3.6 6.75L10.8 12.2C11.5 12.75 12.5 12.75 13.2 12.2L20.4 6.75"))]


# ---------------------------------------------------------------- devices and places

@icon("smartphone")
def _():
    return [S(rrect(6.75, 2.75, 10.5, 18.5, 2.75)), S(open_line((11.0, 5.5), (13.0, 5.5)))]


@icon("tablet")
def _():
    return [S(rrect(4.25, 3.0, 15.5, 18.0, 2.5)), F(dot((12, 5.4), 0.7))]


LAPTOP_BASE = parse("M2.25 17.5H21.75C21.75 18.4 21.05 19.1 20.15 19.1H3.85C2.95 19.1 2.25 18.4 2.25 17.5Z")[0][0]


@icon("laptop")
def _():
    return [S(rrect(4.5, 5.0, 15.0, 10.0, 1.75)), S(LAPTOP_BASE)]


@icon("devices")
def _():
    phone = rrect(15.25, 8.75, 6.25, 11.75, 1.6)
    pf = sdf_rrect(15.25, 8.75, 6.25, 11.75, 1.6)
    screen = rrect(2.75, 4.5, 13.0, 9.0, 1.6)
    base = parse("M2.25 16.75H16.5")[0]
    return [S(phone), S(*keep_outside([screen, base], pf, GAP))]


@icon("smartphone-waves")
def _():
    waves = []
    for r in (5.6, 8.6):
        waves.append(([A((12, 12), r, math.radians(150), math.radians(210))], False))
        waves.append(([A((12, 12), r, math.radians(-30), math.radians(30))], False))
    return [S(rrect(9.25, 5.0, 5.5, 14.0, 1.6)), S(*waves)]


# ---- smartphone-arrow: an arrow from outside into a phone (the password stays on the device)

@icon("smartphone-arrow")
def _():
    ph = rrect(9.75, 2.75, 10.5, 18.5, 2.75)
    slot = open_line((14.0, 5.5), (16.0, 5.5))
    shaft_f = sdf_capsule_region((2.75, 12.0), (15.25, 12.0), 0.0)
    return [S(*keep_outside([ph], shaft_f, GAP)), S(slot),
            S(open_line((2.75, 12.0), (15.25, 12.0)), open_line((11.75, 8.5), (15.25, 12.0), (11.75, 15.5)))]


@icon("radiowaves")
def _():
    waves = []
    for r in (5.0, 8.75):
        waves.append(([A((12, 12), r, math.radians(140), math.radians(220))], False))
        waves.append(([A((12, 12), r, math.radians(-40), math.radians(40))], False))
    return [F(dot((12, 12), 1.9)), S(*waves)]


# ---- location: the pin's stick passes behind the ellipse's far edge, with a gap

@icon("location")
def _():
    head = circle((12, 7.0), 3.6)
    stick = ([L((12, 10.6), (12, 17.25))], False)
    base = ellipse((12, 17.25), 7.25, 3.0)
    base_runs = keep_outside([base], sdf_capsule_region((12, 10.6), (12, 15.4), 0.0), GAP)
    return [S(head), S(stick), S(*base_runs)]


# ---------------------------------------------------------------- settings rows

@icon("appearance")
def _():
    half = [A((12, 12), 9.25, -math.pi / 2, -math.pi / 2 - math.pi), L((12, 21.25), (12, 2.75))]
    return [S(circle((12, 12), 9.25)), F(half)]


@icon("text-size")
def _():
    return [S(open_line((2.75, 19.0), (6.25, 10.75), (9.75, 19.0)), open_line((4.1, 15.9), (8.4, 15.9)),
              open_line((10.75, 19.0), (15.75, 5.0), (20.75, 19.0)), open_line((12.6, 13.9), (18.9, 13.9)))]


@icon("thumbsup")
def _():
    return [S(*parse("M7.25 10.5H5.0C4.3 10.5 3.75 11.05 3.75 11.75V19.0C3.75 19.7 4.3 20.25 5.0 20.25H7.25Z")),
            S(*parse("M7.25 10.5L10.7 4.65C11.1 3.95 12.0 3.7 12.7 4.1C13.45 4.55 13.75 5.45 13.45 6.25L12.4 9.25"
                     "H18.25C19.5 9.25 20.45 10.4 20.2 11.6L18.85 18.4C18.6 19.5 17.65 20.25 16.55 20.25H7.25Z"))]


@icon("sun")
def _():
    rays = []
    for k in range(8):
        th = k * math.pi / 4
        rays.append(open_line((12 + 6.6 * math.cos(th), 12 + 6.6 * math.sin(th)), (12 + 8.9 * math.cos(th), 12 + 8.9 * math.sin(th))))
    return [S(circle((12, 12), 3.9)), S(*rays)]


@icon("moon")
def _():
    return [S(*parse("M19.9 14.6C18.9 15.0 17.85 15.2 16.75 15.2C12.25 15.2 8.6 11.55 8.6 7.05C8.6 5.65 8.95 4.35 9.6 3.2"
                     "C5.95 4.2 3.25 7.55 3.25 11.5C3.25 16.25 7.1 20.1 11.85 20.1C15.55 20.1 18.7 17.85 19.9 14.6Z"))]


@icon("database")
def _():
    top = ellipse((12, 6.25), 7.25, 2.75)
    side = parse("M4.75 6.25V17.75C4.75 19.25 8.0 20.5 12.0 20.5C16.0 20.5 19.25 19.25 19.25 17.75V6.25")[0]
    mid = parse("M4.75 12.0C4.75 13.5 8.0 14.75 12.0 14.75C16.0 14.75 19.25 13.5 19.25 12.0")[0]
    return [S(top), S(side, mid)]


# ---------------------------------------------------------------- composer and media

PAPERCLIP = ("M16.6 7.9L9.85 14.65C9.15 15.35 9.15 16.45 9.85 17.15C10.55 17.85 11.65 17.85 12.35 17.15L18.6 10.9"
             "C20.0 9.5 20.0 7.25 18.6 5.85C17.2 4.45 14.95 4.45 13.55 5.85L6.95 12.45C4.9 14.5 4.9 17.8 6.95 19.85"
             "C9.0 21.9 12.3 21.9 14.35 19.85L19.25 14.95")


@icon("paperclip")
def _():
    return [S(xform_segs(parse(PAPERCLIP)[0][0], 0.94, 0.55, -0.05))]


PLANE = "M20.75 3.25L3.6 9.95C2.95 10.2 2.95 11.15 3.6 11.4L10.3 13.7L12.6 20.4C12.85 21.05 13.8 21.05 14.05 20.4Z"
FOLD = ((10.3, 13.7), (20.75, 3.25))


@icon("send")
def _():
    return [S(*parse(PLANE)), S(open_line(*FOLD))]


@icon("send-fill")
def _():
    (x0, y0), (x1, y1) = FOLD
    m = math.hypot(x1 - x0, y1 - y0)
    n = (-(y1 - y0) / m, (x1 - x0) / m)
    dist = lambda p: (p[0] - x0) * n[0] + (p[1] - y0) * n[1]
    g = H + 0.55
    plane = parse(PLANE)[0][0]
    a = clip_closed_sdf(plane, lambda p: dist(p) - g)
    b = clip_closed_sdf(plane, lambda p: -dist(p) - g)
    return [FS(a, b)]


@icon("face-smiling")
def _():
    return [S(circle((12, 12), 9.25)), F(dot((9.0, 9.9), 1.2), dot((15.0, 9.9), 1.2)),
            S(*parse("M8.4 14.0C9.25 15.6 10.5 16.4 12.0 16.4C13.5 16.4 14.75 15.6 15.6 14.0"))]


@icon("stop-fill")
def _():
    return [FS(rrect(6.0, 6.0, 12.0, 12.0, 2.25))]


PLAY = "M7.75 5.7C7.75 4.75 8.75 4.15 9.6 4.65L19.2 10.75C19.95 11.25 19.95 12.75 19.2 13.25L9.6 19.35C8.75 19.85 7.75 19.25 7.75 18.3Z"


@icon("play-fill")
def _():
    return [FS(*parse(PLAY))]


@icon("pause-fill")
def _():
    return [FS(rrect(6.25, 4.75, 3.5, 14.5, 1.2)), FS(rrect(14.25, 4.75, 3.5, 14.5, 1.2))]


@icon("play-circle")
def _():
    tri = xform_segs(parse(PLAY)[0][0], 0.5, 6.4, 6.0)
    return [S(circle((12, 12), 9.25)), S(tri)]


DOC = ("M8.0 3.75H13.25L18.75 9.25V18.25C18.75 19.35 17.85 20.25 16.75 20.25H8.0C6.9 20.25 6.0 19.35 6.0 18.25V5.75"
       "C6.0 4.65 6.9 3.75 8.0 3.75Z")
DOC_FOLD = parse("M13.25 3.75V7.5C13.25 8.45 14.05 9.25 15.0 9.25H18.75")[0]


@icon("doc")
def _():
    return [S(*parse(DOC)), S(DOC_FOLD)]


@icon("doc-text")
def _():
    return [S(*parse(DOC)), S(DOC_FOLD), S(open_line((9.0, 13.0), (15.75, 13.0)), open_line((9.0, 16.25), (15.75, 16.25)),
            open_line((9.0, 9.75), (10.5, 9.75)))]


MOUNTAINS = "M3.6 17.0L8.3 12.65C8.8 12.2 9.6 12.2 10.05 12.7L12.85 15.65L15.1 13.5C15.6 13.05 16.35 13.05 16.85 13.55L20.4 17.0"


@icon("photo")
def _():
    return [S(rrect(3.0, 4.75, 18.0, 14.5, 2.5)), S(circle((8.75, 9.4), 1.55)), S(*parse(MOUNTAINS))]


@icon("photo-stack")
def _():
    front = (6.5, 7.75, 14.75, 12.0, 2.25)
    back = rrect(2.75, 4.25, 14.75, 12.0, 2.25)
    m = xform_segs(parse(MOUNTAINS)[0][0], 0.78, 4.4, 6.3)
    return [S(rrect(*front)), S(*keep_outside([back], sdf_rrect(*front), GAP)),
            S(circle((11.0, 11.75), 1.25)), S(m)]


@icon("music-note")
def _():
    return [F(circle((8.6, 17.4), 2.85)), S(open_line((11.4, 17.4), (11.4, 3.75))),
            S(*parse("M11.4 3.75C14.4 4.3 16.9 5.75 17.75 8.6"))]


# ---------------------------------------------------------------- channels and conversations

MEGAPHONE = ("M3.75 9.85C3.75 9.0 4.4 8.35 5.25 8.35H8.5L17.2 4.35C18.15 3.9 19.25 4.6 19.25 5.65V18.35"
             "C19.25 19.4 18.15 20.1 17.2 19.65L8.5 15.65H5.25C4.4 15.65 3.75 15.0 3.75 14.15Z")
MEGA_HANDLE = parse("M7.0 15.65L8.15 19.55C8.35 20.2 9.0 20.55 9.65 20.4C10.3 20.2 10.65 19.5 10.45 18.85L9.55 15.9")[0]


@icon("megaphone")
def _():
    return [S(*parse(MEGAPHONE)), S(open_line((8.5, 8.35), (8.5, 15.65))), S(MEGA_HANDLE)]


@icon("megaphone-fill")
def _():
    return [FS(*parse(MEGAPHONE)), S(MEGA_HANDLE)]


# ---- bubbles: back bubble upper left (tail lower left), front bubble lower right (tail lower right)

def chat_bubble(x0, y0, x1, y1, r, tail):
    """A rounded bubble with a tail hanging under its lower-left corner
    (tail='left') or lower-right corner (tail='right')."""
    def seg_list(x0, y0, x1, y1, r):
        k = 0.5523 * r
        # drawn for a left tail, clockwise from the top-left
        return [
            L((x0 + r, y0), (x1 - r, y0)),
            C((x1 - r, y0), (x1 - r + k, y0), (x1, y0 + r - k), (x1, y0 + r)),
            L((x1, y0 + r), (x1, y1 - r)),
            C((x1, y1 - r), (x1, y1 - r + k), (x1 - r + k, y1), (x1 - r, y1)),
            L((x1 - r, y1), (x0 + 3.6, y1)),
            L((x0 + 3.6, y1), (x0 + 0.35, y1 + 2.0)),
            C((x0 + 0.35, y1 + 2.0), (x0 + 0.15, y1 + 2.1), (x0, y1 + 2.0), (x0, y1 + 1.75)),
            L((x0, y1 + 1.75), (x0, y0 + r)),
            C((x0, y0 + r), (x0, y0 + r - k), (x0 + r - k, y0), (x0 + r, y0)),
        ]
    if tail == "left":
        return seg_list(x0, y0, x1, y1, r)
    return xform_segs(seg_list(24 - x1, y0, 24 - x0, y1, r), mirror_x=12.0)


BACK_B = (2.75, 3.25, 15.0, 12.25, 3.4)
FRONT_B = (10.5, 9.75, 21.25, 18.75, 3.4)


def two_bubbles_v2():
    back = chat_bubble(*BACK_B, "left")
    front = chat_bubble(*FRONT_B, "right")
    x0, y0, x1, y1, r = FRONT_B
    ff = sdf_rrect(x0, y0, x1 - x0, y1 - y0, r)
    return back, front, ff


@icon("bubbles")
def _():
    back, front, ff = two_bubbles_v2()
    return [S(front), S(*keep_outside([back], ff, GAP))]


@icon("bubbles-fill")
def _():
    back, front, ff = two_bubbles_v2()
    x0, y0, x1, y1, r = FRONT_B
    g = H + 1.0 + H
    grown = rrect(x0 - g, y0 - g, x1 - x0 + 2 * g, y1 - y0 + 2 * g, r + g)
    bx0, by0, bx1, by1, br = BACK_B
    piece = clip_closed_sdf_bridged(back, lambda p: ff(p) - g,
                                    bridge_along(grown, sdf_rrect(bx0, by0, bx1 - bx0, by1 - by0, br)))
    return [FS(front), FS(piece)]


# ---------------------------------------------------------------- feed rule icons and emoji groups

def star_points(c=(12, 12.6), ro=9.6, ri=4.35):
    pts = []
    for k in range(10):
        th = -math.pi / 2 + k * math.pi / 5
        r = ro if k % 2 == 0 else ri
        pts.append((c[0] + r * math.cos(th), c[1] + r * math.sin(th)))
    return pts


@icon("star")
def _():
    return [S(poly(star_points()))]


HEART = ("M12.0 20.0C12.0 20.0 3.0 15.25 3.0 9.15C3.0 6.25 5.25 4.0 8.05 4.0C9.75 4.0 11.15 4.9 12.0 6.3"
         "C12.85 4.9 14.25 4.0 15.95 4.0C18.75 4.0 21.0 6.25 21.0 9.15C21.0 15.25 12.0 20.0 12.0 20.0Z")


@icon("heart")
def _():
    return [S(*parse(HEART))]


@icon("heart-fill")
def _():
    return [FS(*parse(HEART))]


BOLT = ("M13.6 2.9L5.3 13.3C5.0 13.7 5.3 14.3 5.8 14.3H11.25L10.4 21.1L18.7 10.7C19.0 10.3 18.7 9.7 18.2 9.7"
        "H12.75Z")


@icon("bolt")
def _():
    return [S(*parse(BOLT))]


@icon("bolt-slash")
def _():
    # ADR-0092: the camera's flash, off.
    use_slash(0.0)
    return slashed_outline([parse(BOLT)[0][0]])


@icon("book")
def _():
    return [S(*parse("M12.0 6.3C10.4 5.05 8.25 4.4 5.3 4.4C4.45 4.4 3.75 5.05 3.75 5.9V17.4C3.75 18.25 4.45 18.9 5.3 18.9"
                     "C8.25 18.9 10.4 19.5 12.0 20.75C13.6 19.5 15.75 18.9 18.7 18.9C19.55 18.9 20.25 18.25 20.25 17.4"
                     "V5.9C20.25 5.05 19.55 4.4 18.7 4.4C15.75 4.4 13.6 5.05 12.0 6.3Z")),
            S(open_line((12, 6.3), (12, 20.75)))]


@icon("code")
def _():
    return [S(open_line((7.75, 7.0), (2.75, 12.0), (7.75, 17.0)), open_line((16.25, 7.0), (21.25, 12.0), (16.25, 17.0)),
              open_line((13.5, 4.75), (10.5, 19.25)))]


@icon("flag")
def _():
    return [S(open_line((5.25, 3.5), (5.25, 21.0))),
            S(*parse("M5.25 4.5C7.6 3.45 9.6 3.6 11.6 4.6C13.6 5.6 15.65 5.75 18.75 4.6V13.1C15.65 14.25 13.6 14.1 11.6 13.1"
                     "C9.6 12.1 7.6 11.95 5.25 13.0"))]








# ---- car: front view, body and roof in one outline, solid wheels, two lamps



# ---- soccerball: a ring, a solid centre patch and five seams; nothing else





@icon("logout")
def _():
    return [S(*parse("M11.75 4.25H7.0C5.75 4.25 4.75 5.25 4.75 6.5V17.5C4.75 18.75 5.75 19.75 7.0 19.75H11.75")),
            S(open_line((10.0, 12.0), (20.25, 12.0)), open_line((16.5, 8.25), (20.25, 12.0), (16.5, 15.75)))]
