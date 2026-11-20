# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
"""Draw the Lakko in-app icon set into docs/brand/icons/svg (one SVG per icon).

Run from the repository root: `python3 tools/brand/icon-drawing/build.py`;
then `just export-icons` writes the client copies."""
import os, sys
import icons  # noqa: F401  (populates ICONS via decorators)
from icons import ICONS, SW

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "../../../docs/brand/icons/svg")
os.makedirs(OUT, exist_ok=True)

STROKE = (f'fill="none" stroke="currentColor" stroke-width="{SW}" '
          'stroke-linecap="round" stroke-linejoin="round"')
FILLSTROKE = (f'fill="currentColor" fill-rule="evenodd" stroke="currentColor" stroke-width="{SW}" '
              'stroke-linecap="round" stroke-linejoin="round"')
FILL = 'fill="currentColor" fill-rule="evenodd"'


def svg(name, elems):
    body = []
    for kind, d in elems:
        attrs = {"s": STROKE, "fs": FILLSTROKE, "f": FILL}[kind]
        body.append(f'  <path d="{d}" {attrs}/>')
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">\n'
            '  <!-- SPDX-License-Identifier: AGPL-3.0-or-later -->\n'
            '  <!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->\n'
            + "\n".join(body) + "\n</svg>\n")


for name, fn in ICONS.items():
    elems = fn()
    with open(os.path.join(OUT, f"{name}.svg"), "w") as f:
        f.write(svg(name, elems))
print(len(ICONS), "icons")
