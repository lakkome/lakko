# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
"""Tiny geometry kit for the Lakko icon set: shapes as lists of line and
circular-arc segments, half-plane clipping, and SVG path output.
Absolute coordinates only, so the SVGs carry no transforms."""
import math

TAU = 2 * math.pi


def fmt(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    if s in ("-0", ""):
        s = "0"
    return s


def pt(p):
    return f"{fmt(p[0])} {fmt(p[1])}"


# segment: ("L", p0, p1) or ("A", c, r, a0, a1)  (y down, angle grows clockwise on screen)
def L(p0, p1):
    return ("L", tuple(p0), tuple(p1))


def A(c, r, a0, a1):
    return ("A", tuple(c), r, a0, a1)


def seg_start(s):
    if s[0] == "L":
        return s[1]
    c, r, a0 = s[1], s[2], s[3]
    return (c[0] + r * math.cos(a0), c[1] + r * math.sin(a0))


def seg_end(s):
    if s[0] == "L":
        return s[2]
    c, r, a1 = s[1], s[2], s[4]
    return (c[0] + r * math.cos(a1), c[1] + r * math.sin(a1))


def close(a, b, eps=1e-3):
    return abs(a[0] - b[0]) < eps and abs(a[1] - b[1]) < eps


def path(*contours):
    """contours: (segments, closed) tuples, or a list of segments (closed)."""
    out = []
    for item in contours:
        if isinstance(item, tuple) and len(item) == 2 and isinstance(item[1], bool):
            segs, closed = item
        else:
            segs, closed = item, True
        cur = None
        start = None
        for s in segs:
            p0 = seg_start(s)
            if cur is None or not close(cur, p0):
                out.append("M" + pt(p0))
                start = p0
            if s[0] == "L":
                out.append("L" + pt(s[2]))
            elif s[0] == "C":
                out.append("C" + pt(s[2]) + " " + pt(s[3]) + " " + pt(s[4]))
            else:
                c, r, a0, a1 = s[1:]
                d = a1 - a0
                if abs(abs(d) - TAU) < 1e-6:  # full circle: two halves
                    mid = a0 + d / 2
                    for (b0, b1) in ((a0, mid), (mid, a1)):
                        e = (c[0] + r * math.cos(b1), c[1] + r * math.sin(b1))
                        out.append(f"A{fmt(r)} {fmt(r)} 0 0 {1 if d > 0 else 0} {pt(e)}")
                else:
                    e = seg_end(s)
                    large = 1 if abs(d) > math.pi else 0
                    sweep = 1 if d > 0 else 0
                    out.append(f"A{fmt(r)} {fmt(r)} 0 {large} {sweep} {pt(e)}")
            cur = seg_end(s)
        if closed and start is not None:
            out.append("Z")
    return "".join(out)


# ---------- primitive shapes ----------

def circle(c, r):
    return [A(c, r, -math.pi / 2, -math.pi / 2 + TAU)]


def circle_ccw(c, r):
    return [A(c, r, -math.pi / 2, -math.pi / 2 - TAU)]


def rrect(x, y, w, h, r):
    r = min(r, w / 2, h / 2)
    segs = []
    if r <= 0:
        return poly([(x, y), (x + w, y), (x + w, y + h), (x, y + h)])
    segs.append(L((x + r, y), (x + w - r, y)))
    segs.append(A((x + w - r, y + r), r, -math.pi / 2, 0))
    segs.append(L((x + w, y + r), (x + w, y + h - r)))
    segs.append(A((x + w - r, y + h - r), r, 0, math.pi / 2))
    segs.append(L((x + w - r, y + h), (x + r, y + h)))
    segs.append(A((x + r, y + h - r), r, math.pi / 2, math.pi))
    segs.append(L((x, y + h - r), (x, y + r)))
    segs.append(A((x + r, y + r), r, math.pi, 1.5 * math.pi))
    return [s for s in segs if not (s[0] == "L" and close(s[1], s[2]))]


def poly(points, closed=True):
    segs = [L(points[i], points[i + 1]) for i in range(len(points) - 1)]
    if closed:
        segs.append(L(points[-1], points[0]))
    return segs


def capsule(p0, p1, h):
    """Closed outline of a line from p0 to p1 with round caps of radius h."""
    dx, dy = p1[0] - p0[0], p1[1] - p0[1]
    n = math.hypot(dx, dy)
    ux, uy = dx / n, dy / n
    nx, ny = -uy, ux
    a = math.atan2(ny, nx)
    q0 = (p0[0] + nx * h, p0[1] + ny * h)
    q1 = (p1[0] + nx * h, p1[1] + ny * h)
    q2 = (p1[0] - nx * h, p1[1] - ny * h)
    q3 = (p0[0] - nx * h, p0[1] - ny * h)
    return [L(q0, q1), A(p1, h, a, a - math.pi), L(q2, q3), A(p0, h, a + math.pi, a)]


def _isect(p, d, q, e):
    den = d[0] * e[1] - d[1] * e[0]
    if abs(den) < 1e-9:
        return None
    t = ((q[0] - p[0]) * e[1] - (q[1] - p[1]) * e[0]) / den
    return (p[0] + d[0] * t, p[1] + d[1] * t)


def thick_polyline(points, h):
    """Closed outline of an open polyline with half width h, miter joins and
    round caps."""
    n = len(points)
    dirs = []
    for i in range(n - 1):
        dx, dy = points[i + 1][0] - points[i][0], points[i + 1][1] - points[i][1]
        m = math.hypot(dx, dy)
        dirs.append((dx / m, dy / m))
    norms = [(-d[1], d[0]) for d in dirs]

    def side(sign):
        pts = []
        for i in range(n):
            if i == 0:
                nn = norms[0]
                pts.append((points[0][0] + sign * nn[0] * h, points[0][1] + sign * nn[1] * h))
            elif i == n - 1:
                nn = norms[-1]
                pts.append((points[-1][0] + sign * nn[0] * h, points[-1][1] + sign * nn[1] * h))
            else:
                n0, n1 = norms[i - 1], norms[i]
                a0 = (points[i][0] + sign * n0[0] * h, points[i][1] + sign * n0[1] * h)
                a1 = (points[i][0] + sign * n1[0] * h, points[i][1] + sign * n1[1] * h)
                x = _isect(a0, dirs[i - 1], a1, dirs[i])
                pts.append(x if x else a0)
        return pts

    left = side(1)
    right = side(-1)
    segs = poly(left, closed=False)
    a_end = math.atan2(norms[-1][1], norms[-1][0])
    segs.append(A(points[-1], h, a_end, a_end - math.pi))
    rr = list(reversed(right))
    segs += poly(rr, closed=False)
    a_st = math.atan2(norms[0][1], norms[0][0])
    segs.append(A(points[0], h, a_st + math.pi, a_st))
    return segs


def cross(c, arm, h, angle):
    """Closed outline of a plus (angle 0) or an x (angle 45 deg) with round
    arm ends; arm = centre to arm tip (centreline)."""
    cx, cy = c
    segs = []
    k = h  # inner corner offset
    for i in range(4):
        th = angle + i * math.pi / 2
        u = (math.cos(th), math.sin(th))
        nrm = (-u[1], u[0])
        # start at inner corner before this arm, along arm right side, cap, left side to inner corner
        inner_a = (cx - nrm[0] * k + u[0] * k, cy - nrm[1] * k + u[1] * k)
        tip = (cx + u[0] * arm, cy + u[1] * arm)
        ra = (tip[0] - nrm[0] * h, tip[1] - nrm[1] * h)
        la = (tip[0] + nrm[0] * h, tip[1] + nrm[1] * h)
        inner_b = (cx + nrm[0] * k + u[0] * k, cy + nrm[1] * k + u[1] * k)
        a = math.atan2(-nrm[1], -nrm[0])
        segs.append(L(inner_a, ra))
        segs.append(A(tip, h, a, a + math.pi))
        segs.append(L(la, inner_b))
    return segs


def arrow_outline(tail, apex, wing_l, wing_r, h):
    """Union outline of an arrow: shaft tail->apex and wings apex->wing_l,
    apex->wing_r, all of half width h, round ends, mitred apex."""
    def unit(a, b):
        dx, dy = b[0] - a[0], b[1] - a[1]
        m = math.hypot(dx, dy)
        return (dx / m, dy / m)
    us = unit(tail, apex)
    ns = (-us[1], us[0])
    ul = unit(apex, wing_l)
    nl = (-ul[1], ul[0])
    ur = unit(apex, wing_r)
    nr = (-ur[1], ur[0])

    def off(p, n, s):
        return (p[0] + n[0] * h * s, p[1] + n[1] * h * s)
    # determine which shaft side faces which wing
    sl = 1 if (ns[0] * (wing_l[0] - apex[0]) + ns[1] * (wing_l[1] - apex[1])) > 0 else -1
    # shaft side towards left wing meets left wing's inner (shaft-facing) edge
    inner_l_sign = 1 if (nl[0] * (tail[0] - apex[0]) + nl[1] * (tail[1] - apex[1])) > 0 else -1
    inner_r_sign = 1 if (nr[0] * (tail[0] - apex[0]) + nr[1] * (tail[1] - apex[1])) > 0 else -1
    j_l = _isect(off(tail, ns, sl), us, off(apex, nl, inner_l_sign), ul)
    j_r = _isect(off(tail, ns, -sl), us, off(apex, nr, inner_r_sign), ur)
    # outer apex: intersection of the outer edges of the two wings
    o = _isect(off(apex, nl, -inner_l_sign), ul, off(apex, nr, -inner_r_sign), ur)
    # walk: tail cap, shaft side (-sl) up to j_r, right wing inner edge to wing_r cap,
    # outer edge back to apex o, left wing outer edge to wing_l cap, inner edge to j_l, shaft back
    t_r = off(tail, ns, -sl)
    t_l = off(tail, ns, sl)
    segs = []
    segs.append(L(t_r, j_r))
    wr_in = off(wing_r, nr, inner_r_sign)
    wr_out = off(wing_r, nr, -inner_r_sign)
    segs.append(L(j_r, wr_in))
    a = math.atan2(nr[1] * inner_r_sign, nr[0] * inner_r_sign)
    b = math.atan2(nr[1] * -inner_r_sign, nr[0] * -inner_r_sign)
    segs.append(A(wing_r, h, a, a + (math.pi if _ccw_is_out(wing_r, a, ur) else -math.pi)))
    segs.append(L(wr_out, o))
    wl_out = off(wing_l, nl, -inner_l_sign)
    wl_in = off(wing_l, nl, inner_l_sign)
    segs.append(L(o, wl_out))
    a2 = math.atan2(nl[1] * -inner_l_sign, nl[0] * -inner_l_sign)
    segs.append(A(wing_l, h, a2, a2 + (math.pi if _ccw_is_out(wing_l, a2, ul) else -math.pi)))
    segs.append(L(wl_in, j_l))
    segs.append(L(j_l, t_l))
    a3 = math.atan2(ns[1] * sl, ns[0] * sl)
    segs.append(A(tail, h, a3, a3 + (math.pi if _ccw_is_out(tail, a3, (-us[0], -us[1])) else -math.pi)))
    return segs


def _ccw_is_out(c, a, outward):
    # the half circle from angle a should pass through direction `outward`
    mid = a + math.pi / 2
    return math.cos(mid) * outward[0] + math.sin(mid) * outward[1] > 0


# ---------- clipping ----------

def _side(p, p0, n):
    return (p[0] - p0[0]) * n[0] + (p[1] - p0[1]) * n[1]


def _split_points(s, p0, n, off):
    """Parameters (as points + angles) where segment s crosses the line
    (p - p0).n = off, ordered along s."""
    res = []
    if s[0] == "L":
        a, b = s[1], s[2]
        fa, fb = _side(a, p0, n) - off, _side(b, p0, n) - off
        if (fa < 0 < fb) or (fb < 0 < fa):
            t = fa / (fa - fb)
            res.append((t, (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)))
    else:
        c, r, a0, a1 = s[1:]
        # point on circle: c + r(cos t, sin t); side = (c-p0).n + r(cos t n0 + sin t n1) - off
        k = _side(c, p0, n) - off
        R = r * math.hypot(n[0], n[1])
        if R > 1e-12 and abs(k) < R:
            phi = math.atan2(n[1], n[0])
            base = math.acos(-k / R)
            for t in (phi + base, phi - base):
                # map t into arc parameter range
                d = a1 - a0
                rel = (t - a0) % TAU if d > 0 else (a0 - t) % TAU
                if 1e-9 < rel < abs(d) - 1e-9:
                    ang = a0 + rel if d > 0 else a0 - rel
                    res.append((rel / abs(d), ang))
        res.sort()
    return res


def _cut(s, p0, n, off):
    """Split segment s into pieces at its crossings with the line."""
    xs = _split_points(s, p0, n, off)
    if not xs:
        return [s]
    pieces = []
    if s[0] == "L":
        prev = s[1]
        for _, q in xs:
            pieces.append(L(prev, q))
            prev = q
        pieces.append(L(prev, s[2]))
    else:
        c, r, a0, a1 = s[1:]
        prev = a0
        for _, ang in xs:
            pieces.append(A(c, r, prev, ang))
            prev = ang
        pieces.append(A(c, r, prev, a1))
    return pieces


def _mid(s):
    if s[0] == "L":
        return ((s[1][0] + s[2][0]) / 2, (s[1][1] + s[2][1]) / 2)
    c, r, a0, a1 = s[1:]
    m = (a0 + a1) / 2
    return (c[0] + r * math.cos(m), c[1] + r * math.sin(m))


def clip_closed(segs, p0, n, off):
    """Keep the part of a closed contour where (p - p0).n >= off; close
    the gaps with straight lines along the clip line."""
    pieces = []
    for s in segs:
        pieces += _cut(s, p0, n, off)
    kept = [s for s in pieces if _side(_mid(s), p0, n) - off >= 0]
    if not kept:
        return []
    out = []
    for i, s in enumerate(kept):
        if out and not close(seg_end(out[-1]), seg_start(s)):
            out.append(L(seg_end(out[-1]), seg_start(s)))
        out.append(s)
    if not close(seg_end(out[-1]), seg_start(out[0])):
        out.append(L(seg_end(out[-1]), seg_start(out[0])))
    # rotate so a bridge line is not in the middle of a run (cosmetic only)
    return out


def clip_band_open(segs, p0, n, g):
    """For stroked outlines: drop the parts within distance g of the line
    through p0 with unit normal n. Returns a list of open runs."""
    pieces = []
    for s in segs:
        for q in _cut(s, p0, n, g):
            pieces += _cut(q, p0, n, -g)
    runs = []
    cur = []
    for s in pieces:
        keep = abs(_side(_mid(s), p0, n)) >= g
        if keep:
            if cur and not close(seg_end(cur[-1]), seg_start(s)):
                runs.append(cur)
                cur = []
            cur.append(s)
        else:
            if cur:
                runs.append(cur)
                cur = []
    if cur:
        runs.append(cur)
    # merge last run into first when the contour wraps around
    if len(runs) > 1 and close(seg_end(runs[-1][-1]), seg_start(runs[0][0])):
        runs[0] = runs[-1] + runs[0]
        runs.pop()
    return runs


def slash_frame(a=(4.0, 4.0), b=(20.0, 20.0)):
    dx, dy = b[0] - a[0], b[1] - a[1]
    m = math.hypot(dx, dy)
    return a, (-dy / m, dx / m)


# ---------- signed-distance clipping (for overlaps and slashes) ----------

def sdf_circle(c, r):
    return lambda p: math.hypot(p[0] - c[0], p[1] - c[1]) - r


def sdf_rrect(x, y, w, h, r):
    cx, cy = x + w / 2, y + h / 2
    hx, hy = w / 2 - r, h / 2 - r

    def f(p):
        qx = abs(p[0] - cx) - hx
        qy = abs(p[1] - cy) - hy
        out = math.hypot(max(qx, 0), max(qy, 0))
        return out + min(max(qx, qy), 0) - r
    return f


def sdf_halfplane(p0, n):
    """Negative on the side the normal points away from."""
    return lambda p: -((p[0] - p0[0]) * n[0] + (p[1] - p0[1]) * n[1])


def sdf_band(p0, n, half):
    """Negative within `half` of the line through p0 with unit normal n."""
    return lambda p: abs((p[0] - p0[0]) * n[0] + (p[1] - p0[1]) * n[1]) - half


def sdf_union(*fs):
    return lambda p: min(f(p) for f in fs)


def sdf_intersect(*fs):
    return lambda p: max(f(p) for f in fs)


def _point_at(s, t):
    if s[0] == "L":
        a, b = s[1], s[2]
        return (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
    c, r, a0, a1 = s[1:]
    ang = a0 + (a1 - a0) * t
    return (c[0] + r * math.cos(ang), c[1] + r * math.sin(ang))


def _sub(s, t0, t1):
    if s[0] == "L":
        return L(_point_at(s, t0), _point_at(s, t1))
    c, r, a0, a1 = s[1:]
    return A(c, r, a0 + (a1 - a0) * t0, a0 + (a1 - a0) * t1)


def _split_sdf(s, f, N=200):
    ts = [i / N for i in range(N + 1)]
    vals = [f(_point_at(s, t)) for t in ts]
    cuts = [0.0]
    for i in range(N):
        if (vals[i] < 0) != (vals[i + 1] < 0):
            lo, hi = ts[i], ts[i + 1]
            flo = vals[i]
            for _ in range(50):
                mid = (lo + hi) / 2
                fm = f(_point_at(s, mid))
                if (fm < 0) == (flo < 0):
                    lo, flo = mid, fm
                else:
                    hi = mid
            cuts.append((lo + hi) / 2)
    cuts.append(1.0)
    return [(_sub(s, cuts[i], cuts[i + 1]), f(_point_at(s, (cuts[i] + cuts[i + 1]) / 2)) >= 0)
            for i in range(len(cuts) - 1) if cuts[i + 1] - cuts[i] > 1e-6]


def clip_open_sdf(segs, f, closed=True):
    """Keep the parts of a stroked outline where f(p) >= 0. Returns runs."""
    pieces = []
    for s in segs:
        pieces += _split_sdf(s, f)
    runs, cur = [], []
    for s, keep in pieces:
        if keep:
            if cur and not close(seg_end(cur[-1]), seg_start(s)):
                runs.append(cur)
                cur = []
            cur.append(s)
        elif cur:
            runs.append(cur)
            cur = []
    if cur:
        runs.append(cur)
    if closed and len(runs) > 1 and close(seg_end(runs[-1][-1]), seg_start(runs[0][0])):
        runs[0] = runs[-1] + runs[0]
        runs.pop()
    if closed and len(runs) == 1 and len(pieces) and all(k for _, k in pieces):
        return [runs[0]]
    return runs


def clip_closed_sdf(segs, f):
    """Keep the part of a closed contour where f(p) >= 0, bridging the cut
    with straight lines. Fine for cuts along straight or gently curved edges."""
    pieces = [s for s, keep in sum((_split_sdf(s, f) for s in segs), []) if keep]
    if not pieces:
        return []
    # start right after a gap so the bridges come out in order
    k = 0
    for i in range(len(pieces)):
        if not close(seg_end(pieces[i - 1]), seg_start(pieces[i])):
            k = i
            break
    pieces = pieces[k:] + pieces[:k]
    out = []
    for s in pieces:
        if out and not close(seg_end(out[-1]), seg_start(s)):
            out.append(L(seg_end(out[-1]), seg_start(s)))
        out.append(s)
    if not close(seg_end(out[-1]), seg_start(out[0])):
        out.append(L(seg_end(out[-1]), seg_start(out[0])))
    return out


def runs_path(runs):
    return path(*[(r, False) for r in runs])


# ---------- cubic segments and a small absolute-path parser ----------

def C(p0, c1, c2, p1):
    return ("C", tuple(p0), tuple(c1), tuple(c2), tuple(p1))


def _bez(s, t):
    p0, c1, c2, p1 = s[1], s[2], s[3], s[4]
    u = 1 - t
    return (u * u * u * p0[0] + 3 * u * u * t * c1[0] + 3 * u * t * t * c2[0] + t * t * t * p1[0],
            u * u * u * p0[1] + 3 * u * u * t * c1[1] + 3 * u * t * t * c2[1] + t * t * t * p1[1])


def _bez_split(s, t):
    p0, c1, c2, p1 = s[1], s[2], s[3], s[4]

    def lerp(a, b):
        return (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
    a, b, c = lerp(p0, c1), lerp(c1, c2), lerp(c2, p1)
    d, e = lerp(a, b), lerp(b, c)
    m = lerp(d, e)
    return C(p0, a, d, m), C(m, e, c, p1)


def _bez_sub(s, t0, t1):
    if t1 < 1:
        s, _ = _bez_split(s, t1)
    if t0 > 0:
        _, s = _bez_split(s, t0 / t1)
    return s


_old_seg_start, _old_seg_end, _old_point_at, _old_sub = seg_start, seg_end, _point_at, _sub


def seg_start(s):  # noqa: F811
    return s[1] if s[0] == "C" else _old_seg_start(s)


def seg_end(s):  # noqa: F811
    return s[4] if s[0] == "C" else _old_seg_end(s)


def _point_at(s, t):  # noqa: F811
    return _bez(s, t) if s[0] == "C" else _old_point_at(s, t)


def _sub(s, t0, t1):  # noqa: F811
    return _bez_sub(s, t0, t1) if s[0] == "C" else _old_sub(s, t0, t1)


def parse(d):
    """Parse an absolute path (M L H V C Z only) into closed/open contours."""
    import re
    toks = re.findall(r"[MLHVCZ]|-?\d*\.?\d+(?:e-?\d+)?", d)
    i, cur, start = 0, None, None
    contours, segs = [], []
    cmd = None
    def num():
        nonlocal i
        v = float(toks[i]); i += 1
        return v
    while i < len(toks):
        if toks[i].isalpha():
            cmd = toks[i]; i += 1
            if cmd == "Z":
                if segs:
                    if not close(cur, start):
                        segs.append(L(cur, start))
                    contours.append((segs, True))
                segs = []
                cur = start
                continue
        if cmd == "M":
            if segs:
                contours.append((segs, False))
                segs = []
            cur = (num(), num()); start = cur; cmd = "L"
        elif cmd == "L":
            p = (num(), num()); segs.append(L(cur, p)); cur = p
        elif cmd == "H":
            p = (num(), cur[1]); segs.append(L(cur, p)); cur = p
        elif cmd == "V":
            p = (cur[0], num()); segs.append(L(cur, p)); cur = p
        elif cmd == "C":
            c1 = (num(), num()); c2 = (num(), num()); p = (num(), num())
            segs.append(C(cur, c1, c2, p)); cur = p
    if segs:
        contours.append((segs, False))
    return contours


def xform_segs(segs, s=1.0, tx=0.0, ty=0.0, mirror_x=None):
    """Scale about the origin, translate, and optionally mirror about x = m."""
    def f(p):
        x, y = p[0] * s + tx, p[1] * s + ty
        if mirror_x is not None:
            x = 2 * mirror_x - x
        return (x, y)
    out = []
    for g in segs:
        if g[0] == "L":
            out.append(L(f(g[1]), f(g[2])))
        elif g[0] == "C":
            out.append(C(f(g[1]), f(g[2]), f(g[3]), f(g[4])))
        else:
            c, r, a0, a1 = g[1:]
            if mirror_x is not None:
                out.append(A(f(c), r * s, math.pi - a0, math.pi - a1))
            else:
                out.append(A(f(c), r * s, a0, a1))
    return out


# ---------- clipping with bridges that follow the clearance outline ----------

def _reverse(s):
    if s[0] == "L":
        return L(s[2], s[1])
    if s[0] == "C":
        return C(s[4], s[3], s[2], s[1])
    c, r, a0, a1 = s[1:]
    return A(c, r, a1, a0)


def _locate(segs, p, n=400):
    """(segment index, parameter) of the point of a contour nearest to p."""
    best = (1e9, 0, 0.0)
    for i, s in enumerate(segs):
        for k in range(n + 1):
            t = k / n
            q = _point_at(s, t)
            d = (q[0] - p[0]) ** 2 + (q[1] - p[1]) ** 2
            if d < best[0]:
                best = (d, i, t)
    _, i, t = best
    lo, hi = max(0.0, t - 1.0 / n), min(1.0, t + 1.0 / n)
    def dist(x):
        q = _point_at(segs[i], x)
        return (q[0] - p[0]) ** 2 + (q[1] - p[1]) ** 2
    for _ in range(80):
        m1, m2 = lo + (hi - lo) / 3, hi - (hi - lo) / 3
        if dist(m1) < dist(m2):
            hi = m2
        else:
            lo = m1
    return i, (lo + hi) / 2


def _walk(segs, i, t, j, u):
    """Forward run along a closed contour from (i, t) to (j, u)."""
    n = len(segs)
    if i == j and t <= u:
        return [_sub(segs[i], t, u)] if u - t > 1e-6 else []
    out = [_sub(segs[i], t, 1.0)] if t < 1 - 1e-6 else []
    k = (i + 1) % n
    while k != j:
        out.append(segs[k])
        k = (k + 1) % n
    if u > 1e-6:
        out.append(_sub(segs[j], 0.0, u))
    return out


def bridge_along(contour, inside):
    """A bridge function for clip_closed_sdf_bridged: it joins two cut points
    along `contour` (the clearance outline, e.g. a grown circle or rounded
    rectangle), taking the way round that runs through the removed part,
    which is where `inside(p) < 0` (the shape being clipped)."""
    def bridge(p, q):
        i, t = _locate(contour, p)
        j, u = _locate(contour, q)
        fwd = _walk(contour, i, t, j, u)
        back = [_reverse(s) for s in reversed(_walk(contour, j, u, i, t))]
        def score(run):
            if not run:
                return 1e9
            pts = [_point_at(s, k / 8) for s in run for k in range(1, 8)]
            return sum(inside(x) for x in pts) / len(pts)
        run = list(fwd if score(fwd) <= score(back) else back)
        if run and not close(p, seg_start(run[0])):
            run.insert(0, L(p, seg_start(run[0])))
        if run and not close(seg_end(run[-1]), q):
            run.append(L(seg_end(run[-1]), q))
        return run
    return bridge


def clip_closed_sdf_bridged(segs, f, bridge):
    """Like clip_closed_sdf, but each cut is closed with bridge(p, q)."""
    pieces = [s for s, keep in sum((_split_sdf(s, f) for s in segs), []) if keep]
    if not pieces:
        return []
    k = 0
    for i in range(len(pieces)):
        if not close(seg_end(pieces[i - 1]), seg_start(pieces[i])):
            k = i
            break
    pieces = pieces[k:] + pieces[:k]
    out = []
    for s in pieces:
        if out and not close(seg_end(out[-1]), seg_start(s)):
            out += bridge(seg_end(out[-1]), seg_start(s)) or [L(seg_end(out[-1]), seg_start(s))]
        out.append(s)
    if not close(seg_end(out[-1]), seg_start(out[0])):
        out += bridge(seg_end(out[-1]), seg_start(out[0])) or [L(seg_end(out[-1]), seg_start(out[0]))]
    return out
