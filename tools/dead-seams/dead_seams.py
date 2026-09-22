# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

"""Report public API that nothing in the shipping code calls.

A recurring shape of defect in this repository is a feature that is fully
written, correct, and covered by tests - and that nothing ever calls. It is
invisible in review, because the function is right there and reads fine. The
only question that finds it is "who calls this?", and the answer is "nobody".

Three instances turned up in one day (2026-09-22):

  * `setTurnClient` on iOS and Android - the TURN relay was fetched by nobody,
    so a call between two networks never connected;
  * `uploadFromCache` on web - the backup was never refreshed after signup;
  * and the same method also passed an empty MLS snapshot, so its first caller
    would have restored contacts and no groups.

This reports CANDIDATES, never verdicts. Read each one and decide whether it is
deliberate (an API kept for a future caller, a trait method, a test seam) or a
feature that quietly does not run.

Cross-language callers are the reason this is not a one-line grep. A Rust
`pub fn build_backup_payload` is called from Swift, Kotlin and TypeScript as
`buildBackupPayload`, so every name is looked up under both spellings; without
that the whole FFI surface would be reported as dead.

What it cannot see, and so may report as dead when it is not:

  * anything reached by reflection, dynamic dispatch or a string name;
  * a protocol or trait method called through the protocol rather than the
    concrete type;
  * a symbol used only by generated code that this script skips.

And what it may MISS: a function called only from dead code is still reported
as live, because something does name it.
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# Directories that hold no hand-written callers: build output, vendored code,
# and generated bindings (whose contents mirror declarations rather than using
# them).
SKIP_DIR_PARTS = {
    ".git",
    "node_modules",
    "target",
    "build",
    "DerivedData",
    "wasm",
    "vendor",
    ".gradle",
    "Pods",
}

# A declaration only counts as used when something OUTSIDE a test names it.
# A function whose only callers are its own tests is exactly the shape this
# script exists to surface.
TEST_MARKERS = (
    "/tests/",
    "/test/",
    "/fuzz/",
    "Tests/",
    "/androidTest/",
    ".test.ts",
    ".test.tsx",
    "_test.go",
)


def is_test_path(path: Path) -> bool:
    text = str(path)
    if any(marker in text for marker in TEST_MARKERS):
        return True
    return path.name.endswith(("Test.kt", "Tests.swift", "_test.rs"))


# UniFFI and wasm-bindgen write these; every symbol in them mirrors a Rust
# declaration rather than calling one, so scanning them reports the whole
# generated surface and hides the hand-written seams underneath.
GENERATED_FILES = {
    "lakko_ffi_uniffi.swift",
    "lakko_ffi_uniffi.kt",
    "lakko_ffi_extension.swift",
    "lakko_ffi_extension.kt",
}


def should_skip(path: Path) -> bool:
    if path.name in GENERATED_FILES or "uniffi" in path.parts:
        return True
    return any(part in SKIP_DIR_PARTS for part in path.parts)


# Declaration patterns per language. Each yields the declared name in group 1.
RUST_DECL = re.compile(r"^\s*pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+([a-z_][a-z0-9_]*)\s*[(<]", re.M)
TS_DECL = re.compile(
    r"^\s*export\s+(?:async\s+)?function\s+([A-Za-z_$][\w$]*)",
    re.M,
)
# Class methods, which is what most of the web's real API is - `uploadFromCache`,
# the method this script was partly written for, is one. Matched as an indented
# name-paren-body; the keyword list below is what keeps `if (...) {` out.
TS_METHOD = re.compile(
    r"^\s{2,}(?:public\s+|private\s+|protected\s+)?(?:static\s+)?(?:async\s+)?"
    r"([A-Za-z_$][\w$]*)\s*\([^)]*\)\s*(?::[^;{]+)?\{",
    re.M,
)
TS_NOT_METHODS = {
    "if", "for", "while", "switch", "catch", "function", "return", "constructor",
    "do", "else", "try", "typeof", "await", "new", "throw",
}
SWIFT_DECL = re.compile(
    r"^\s*(?:public\s+|internal\s+)?(?:static\s+|final\s+|class\s+)?"
    r"func\s+([A-Za-z_][\w]*)\s*[(<]",
    re.M,
)
KOTLIN_DECL = re.compile(
    r"^\s*(?:public\s+)?(?:suspend\s+)?fun\s+(?:<[^>]*>\s*)?([A-Za-z_][\w]*)\s*\(", re.M
)

LANGS = {
    ".rs": ("rust", RUST_DECL, [REPO / "crates", REPO / "services"]),
    ".ts": ("web", TS_DECL, [REPO / "apps" / "web" / "src"]),
    ".tsx": ("web", TS_DECL, [REPO / "apps" / "web" / "src"]),
    ".swift": ("ios", SWIFT_DECL, [REPO / "apps" / "ios"]),
    ".kt": ("android", KOTLIN_DECL, [REPO / "apps" / "android"]),
}

IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.S)
# `//` only when it is not the `://` of a URL, so a doc line carrying a link
# does not swallow the identifiers after it.
LINE_COMMENT = re.compile(r"(?<!:)//[^\n]*")


def strip_comments(text: str) -> str:
    """Remove comments before counting identifiers.

    Load-bearing, not tidiness. The two defects this script was written for
    were both NAMED in a doc comment in their own file - `uploadFromCache` in
    the class doc above it, `setTurnClient` in the property doc beside it - so
    a counter that reads comments calls them used and reports nothing.
    """
    def blank(match: re.Match[str]) -> str:
        # Newlines are preserved so reported line numbers still point at the
        # real file; a report whose lines are off is worse than none.
        return re.sub(r"[^\n]", " ", match.group(0))

    return LINE_COMMENT.sub(blank, BLOCK_COMMENT.sub(blank, text))


def camel(snake: str) -> str:
    head, *rest = snake.split("_")
    return head + "".join(p[:1].upper() + p[1:] for p in rest if p)


def snake(camel_name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", camel_name).lower()


def spellings(name: str) -> set[str]:
    """Every way this name is written across the repository's languages."""
    return {name, camel(name), snake(name)}


def source_files() -> list[Path]:
    out: list[Path] = []
    for root in (REPO / "crates", REPO / "services", REPO / "apps", REPO / "tools"):
        if not root.exists():
            continue
        for path in root.rglob("*"):
            if path.is_file() and path.suffix in LANGS and not should_skip(path):
                out.append(path)
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--lang",
        choices=sorted({v[0] for v in LANGS.values()}),
        help="report one language only",
    )
    parser.add_argument(
        "--min-name-length",
        type=int,
        default=4,
        help="skip very short names, which collide with unrelated identifiers "
        "and drown the report (default: 4)",
    )
    args = parser.parse_args()

    files = source_files()

    # One pass over the corpus: which identifiers does each file name at all.
    # Cheaper than a search per declaration, and the answer is the same.
    names_in_file: dict[Path, set[str]] = {}
    for path in files:
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        names_in_file[path] = IDENTIFIER.findall(strip_comments(text))

    # Declarations, per language.
    declarations: list[tuple[str, str, Path, int]] = []
    for path in files:
        lang, pattern, roots = LANGS[path.suffix]
        if not any(str(path).startswith(str(r)) for r in roots):
            continue
        if is_test_path(path):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        stripped = strip_comments(text)
        patterns = [pattern]
        if lang == "web":
            patterns.append(TS_METHOD)
        for pat in patterns:
            for match in pat.finditer(stripped):
                name = next((g for g in match.groups() if g), None)
                if name is None or len(name) < args.min_name_length:
                    continue
                if pat is TS_METHOD and name in TS_NOT_METHODS:
                    continue
                line = stripped.count("\n", 0, match.start()) + 1
                declarations.append((lang, name, path, line))

    # How many times each file DECLARES each name. Subtracted from the raw
    # count below, because a declaration is not a use - and without this two
    # platforms that each declare the same dead seam make each other look
    # alive, which is exactly the shape worth catching: the iOS and Android
    # `setTurnClient` covered for one another while neither had a caller.
    declared_here: dict[Path, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    for _lang, name, path, _line in declarations:
        declared_here[path][name] += 1

    # A declaration is live when some non-test file NAMES one of its spellings
    # somewhere other than a declaration of that same name.
    dead: dict[str, list[tuple[str, Path, int]]] = defaultdict(list)
    for lang, name, path, line in declarations:
        if args.lang and lang != args.lang:
            continue
        wanted = spellings(name)
        used = False
        for other, names in names_in_file.items():
            if is_test_path(other):
                continue
            hits = sum(1 for n in names if n in wanted)
            # Its own file counts too: a function called only from where it
            # lives is over-exported, not dead, and reporting it would bury the
            # ones that run nowhere. Declarations never count, here or
            # anywhere.
            for spelling in wanted:
                hits -= declared_here[other].get(spelling, 0)
            if hits > 0:
                used = True
                break
        if not used:
            dead[lang].append((name, path, line))

    total = sum(len(v) for v in dead.values())
    print("dead seams: public API that nothing outside its own file names.")
    print("Candidates, not verdicts - read each one. See the module docstring")
    print("for what this cannot see.\n")
    for lang in sorted(dead):
        rows = sorted(dead[lang], key=lambda r: (str(r[1]), r[2]))
        print(f"== {lang} ({len(rows)}) ==")
        for name, path, line in rows:
            print(f"  {path.relative_to(REPO)}:{line}  {name}")
        print()
    print(f"{total} candidate(s) across {len(files)} files.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
