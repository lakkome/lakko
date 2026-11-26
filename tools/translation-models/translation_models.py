#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
"""Mirror Mozilla's Firefox Translations models for Lakko (ADR-0100).

Operator tool. It runs on the operator's machine, never on a client.

  fetch   download the Remote Settings changeset of `translations-models-v2`
          and every selected model file into a cache directory, checking
          Mozilla's hashes of the compressed and the decompressed bytes.
  build   concatenate the files of each language group (ADR-0100 section 4)
          into one file named by its SHA-256, and write the model manifest
          the web build ships (`apps/web/src/translation/lakko-engine/models.json`).
  verify  re-read the group files and check every hash and size the manifest
          states, decompressing each model file.

Mozilla's `.zst` bytes are never changed: a group file is their
concatenation, and the manifest gives each file's offset and length in it.

Standard library only (Python 3.14 or later for `compression.zstd`).
Run with `python3 -I`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
import urllib.request
from pathlib import Path

try:
    from compression import zstd
except ImportError:  # pragma: no cover - depends on the interpreter
    sys.exit("error: Python 3.14 or later is required (compression.zstd)")

COLLECTION = "translations-models-v2"
CHANGESET_URL = (
    "https://firefox.settings.services.mozilla.com/v1/buckets/main/collections/"
    f"{COLLECTION}/changeset?_expected=0"
)
ATTACHMENT_BASE = "https://firefox-settings-attachments.cdn.mozilla.net/"
USER_AGENT = "lakko-translation-models/1"
MANIFEST_FORMAT = 1

# Records a desktop browser uses: no filter, or the desktop-only filter.
# Android-only and nightly-only records are left out.
DESKTOP_FILTERS = frozenset(["", "env.appinfo.OS != 'Android'"])

# The order files are placed in a group file and listed in the manifest.
FILE_TYPES = ("model", "lex", "vocab", "srcvocab", "trgvocab")

# ADR-0100 section 4: 19 groups of three, by Mozilla language code.
# The order of groups and of languages within a group is the table order;
# it decides group ids and the byte layout of each group file.
GROUPS: tuple[tuple[str, str, str], ...] = (
    ("ru", "uk", "be"),
    ("pl", "cs", "sk"),
    ("sr", "hr", "bs"),
    ("bg", "sl", "ro"),
    ("lt", "lv", "et"),
    ("sv", "da", "nb"),
    ("fi", "is", "hu"),
    ("de", "nl", "af"),
    ("es", "pt", "gl"),
    ("fr", "ca", "eu"),
    ("it", "el", "sq"),
    ("tr", "az", "ug"),
    ("ar", "he", "fa"),
    ("hi", "ur", "bn"),
    ("mr", "gu", "ml"),
    ("ta", "te", "kn"),
    ("zh-Hans", "zh-Hant", "ja"),
    ("ko", "vi", "th"),
    ("id", "ms", "tl"),
)

HEX64 = re.compile(r"^[0-9a-f]{64}$")
CHUNK = 1 << 20


class ToolError(Exception):
    """A fatal problem; the message is printed and the tool exits non-zero."""


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


# --------------------------------------------------------------------------
# Version comparison (the Mozilla toolkit version format: "3.0", "3.0a1").
# Each dot-separated part is number, string, number, rest; a part with a
# string ("0a1") is a pre-release and sorts below the same part without one.
# --------------------------------------------------------------------------

_PART = re.compile(r"^(\d*)([^\d]*)(\d*)(.*)$")


def _part_key(part: str) -> tuple:
    match = _PART.match(part)
    if match is None:  # cannot happen: every group is optional
        raise ToolError(f"unparseable version part {part!r}")
    num_a, str_b, num_c, extra = match.groups()
    # An absent string is greater than any present one ("1.0" > "1.0a1").
    return (
        int(num_a) if num_a else 0,
        (1, "") if str_b == "" else (0, str_b),
        int(num_c) if num_c else 0,
        (1, "") if extra == "" else (0, extra),
    )


_VERSION_PARTS = 8


def version_key(version: str) -> tuple:
    parts = version.split(".")
    if len(parts) > _VERSION_PARTS:
        raise ToolError(f"version {version!r} has more than {_VERSION_PARTS} parts")
    # A missing part reads as "0", so "3" == "3.0" and "3.0a1" < "3.0".
    parts += ["0"] * (_VERSION_PARTS - len(parts))
    return tuple(_part_key(p) for p in parts)


# --------------------------------------------------------------------------
# Selection
# --------------------------------------------------------------------------


def direction_key(source: str, target: str) -> str:
    return f"{source}-{target}"


def is_complete(files: dict) -> bool:
    """A direction's file set Firefox can load: a model, a lexical shortlist,
    and either one shared vocabulary or a source and a target vocabulary."""
    if "model" not in files or "lex" not in files:
        return False
    return "vocab" in files or ("srcvocab" in files and "trgvocab" in files)


def select_records(changeset: dict) -> tuple[dict, list[str]]:
    """Return {direction: {"version", "architecture", "files": {type: record}}}
    and a list of notes (directions whose newest version was incomplete, or
    which have more file types than the usual three)."""
    changes = changeset.get("changes")
    if not isinstance(changes, list):
        raise ToolError("changeset has no `changes` list")

    by_direction: dict[str, dict[str, dict[str, dict]]] = {}
    for record in changes:
        if record.get("deleted"):
            continue
        if record.get("filter_expression", "") not in DESKTOP_FILTERS:
            continue
        for field in (
            "sourceLanguage",
            "targetLanguage",
            "fileType",
            "version",
            "architecture",
            "name",
            "decompressedHash",
            "decompressedSize",
            "attachment",
        ):
            if field not in record:
                raise ToolError(f"record {record.get('id')} has no `{field}`")
        file_type = record["fileType"]
        if file_type not in FILE_TYPES:
            raise ToolError(f"record {record.get('id')}: unknown fileType {file_type!r}")
        attachment = record["attachment"]
        for field in ("hash", "location", "size"):
            if field not in attachment:
                raise ToolError(f"record {record.get('id')}: attachment has no `{field}`")
        if not HEX64.match(attachment["hash"]) or not HEX64.match(record["decompressedHash"]):
            raise ToolError(f"record {record.get('id')}: hash is not 64 lowercase hex")
        key = direction_key(record["sourceLanguage"], record["targetLanguage"])
        versions = by_direction.setdefault(key, {})
        files = versions.setdefault(record["version"], {})
        if file_type in files:
            raise ToolError(
                f"{key} version {record['version']}: two desktop records of type {file_type}"
            )
        files[file_type] = record

    notes: list[str] = []
    selected: dict = {}
    for key in sorted(by_direction):
        versions = by_direction[key]
        chosen = None
        for version in sorted(versions, key=version_key, reverse=True):
            if is_complete(versions[version]):
                chosen = version
                break
            notes.append(
                f"{key}: version {version} is incomplete "
                f"({', '.join(t for t in FILE_TYPES if t in versions[version])}), skipped"
            )
        if chosen is None:
            raise ToolError(f"{key}: no version has a complete file set")
        files = versions[chosen]
        architectures = {r["architecture"] for r in files.values()}
        if len(architectures) != 1:
            raise ToolError(f"{key} version {chosen}: files disagree on architecture {architectures}")
        if set(files) != {"model", "lex", "vocab"}:
            notes.append(f"{key}: version {chosen} has file types {', '.join(t for t in FILE_TYPES if t in files)}")
        selected[key] = {
            "version": chosen,
            "architecture": architectures.pop(),
            "files": files,
        }
    return selected, notes


def group_layout(selected: dict) -> list[list[tuple[str, str, dict]]]:
    """Per group, the (direction, file type, record) list in file order.
    Fails if the groups do not cover the selected languages exactly."""
    languages: set[str] = set()
    for key, entry in selected.items():
        files = entry["files"]
        source = files["model"]["sourceLanguage"]
        target = files["model"]["targetLanguage"]
        if (source == "en") == (target == "en"):
            raise ToolError(f"{key}: a direction must pair a language with English")
        languages.add(target if source == "en" else source)

    seen: dict[str, int] = {}
    for index, group in enumerate(GROUPS, start=1):
        for language in group:
            if language in seen:
                raise ToolError(f"{language} is in group {seen[language]} and group {index}")
            seen[language] = index
    missing = sorted(languages - set(seen))
    if missing:
        raise ToolError(f"languages in no group: {', '.join(missing)}")
    extra = sorted(set(seen) - languages)
    if extra:
        raise ToolError(f"group languages with no directions: {', '.join(extra)}")

    layout = []
    for group in GROUPS:
        entries = []
        for language in group:
            for key in (direction_key("en", language), direction_key(language, "en")):
                if key not in selected:
                    raise ToolError(f"{key}: direction missing for group language {language}")
                files = selected[key]["files"]
                for file_type in FILE_TYPES:
                    if file_type in files:
                        entries.append((key, file_type, files[file_type]))
        layout.append(entries)
    return layout


# --------------------------------------------------------------------------
# I/O helpers
# --------------------------------------------------------------------------


def http_get(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=120) as response:
        if response.status != 200:
            raise ToolError(f"GET {url}: HTTP {response.status}")
        return response.read()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while block := handle.read(CHUNK):
            digest.update(block)
    return digest.hexdigest()


def write_atomic(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=".tmp-")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def check_decompressed(name: str, compressed: bytes, record: dict) -> None:
    try:
        plain = zstd.decompress(compressed)
    except zstd.ZstdError as error:
        raise ToolError(f"{name}: zstd decompression failed: {error}") from error
    if len(plain) != record["decompressedSize"]:
        raise ToolError(
            f"{name}: decompressed size {len(plain)} != {record['decompressedSize']}"
        )
    if hashlib.sha256(plain).hexdigest() != record["decompressedHash"]:
        raise ToolError(f"{name}: decompressed SHA-256 does not match decompressedHash")


def cached_path(cache: Path, record: dict) -> Path:
    return cache / "files" / f"{record['attachment']['hash']}.zst"


def load_changeset(cache: Path) -> dict:
    path = cache / "changeset.json"
    if not path.exists():
        raise ToolError(f"{path} not found; run `fetch` first")
    return json.loads(path.read_bytes())


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------


def cmd_fetch(args: argparse.Namespace) -> None:
    cache = Path(args.cache)
    log(f"fetching changeset {CHANGESET_URL}")
    raw = http_get(CHANGESET_URL)
    changeset = json.loads(raw)
    selected, notes = select_records(changeset)
    group_layout(selected)  # fail before downloading if the groups do not fit
    write_atomic(cache / "changeset.json", raw)
    for note in notes:
        log(f"note: {note}")

    records = [
        entry["files"][t]
        for key in sorted(selected)
        for entry in [selected[key]]
        for t in FILE_TYPES
        if t in entry["files"]
    ]
    total = sum(r["attachment"]["size"] for r in records)
    log(f"{len(selected)} directions, {len(records)} files, {total} bytes compressed")

    done = 0
    for index, record in enumerate(records, start=1):
        attachment = record["attachment"]
        path = cached_path(cache, record)
        name = f"{record['sourceLanguage']}-{record['targetLanguage']} {record['fileType']}"
        if path.exists() and path.stat().st_size == attachment["size"] and sha256_file(path) == attachment["hash"]:
            data = path.read_bytes()
            state = "cached"
        else:
            data = http_get(ATTACHMENT_BASE + attachment["location"])
            if len(data) != attachment["size"]:
                raise ToolError(f"{name}: downloaded {len(data)} bytes, expected {attachment['size']}")
            if hashlib.sha256(data).hexdigest() != attachment["hash"]:
                raise ToolError(f"{name}: SHA-256 of the download does not match attachment.hash")
            state = "downloaded"
        check_decompressed(name, data, record)
        if state == "downloaded":
            write_atomic(path, data)
        done += attachment["size"]
        log(f"[{index}/{len(records)}] {state} {name} ({attachment['size']} bytes, {done * 100 // total}%)")
    log(f"fetch complete: changeset timestamp {changeset['timestamp']}")


def cmd_build(args: argparse.Namespace) -> None:
    cache = Path(args.cache)
    out = Path(args.out)
    changeset = load_changeset(cache)
    selected, notes = select_records(changeset)
    layout = group_layout(selected)
    for note in notes:
        log(f"note: {note}")

    out.mkdir(parents=True, exist_ok=True)
    directions: dict = {}
    groups = []
    for group_id, (languages, entries) in enumerate(zip(GROUPS, layout), start=1):
        digest = hashlib.sha256()
        fd, tmp = tempfile.mkstemp(dir=out, prefix=".tmp-")
        offset = 0
        try:
            with os.fdopen(fd, "wb") as handle:
                for key, file_type, record in entries:
                    attachment = record["attachment"]
                    path = cached_path(cache, record)
                    if not path.exists():
                        raise ToolError(f"{path} not in the cache; run `fetch`")
                    data = path.read_bytes()
                    if len(data) != attachment["size"] or hashlib.sha256(data).hexdigest() != attachment["hash"]:
                        raise ToolError(f"{path}: cached file does not match attachment.hash; run `fetch`")
                    handle.write(data)
                    digest.update(data)
                    entry = directions.setdefault(
                        key,
                        {
                            "group": group_id,
                            "architecture": selected[key]["architecture"],
                            "version": selected[key]["version"],
                            "files": {},
                        },
                    )
                    entry["files"][file_type] = {
                        "name": record["name"],
                        "offset": offset,
                        "length": len(data),
                        "sha256": attachment["hash"],
                        "decompressedSha256": record["decompressedHash"],
                        "decompressedBytes": record["decompressedSize"],
                    }
                    offset += len(data)
                handle.flush()
                os.fsync(handle.fileno())
            file_hash = digest.hexdigest()
            os.chmod(tmp, 0o644)
            os.replace(tmp, out / file_hash)
        except BaseException:
            Path(tmp).unlink(missing_ok=True)
            raise
        groups.append(
            {"id": group_id, "languages": list(languages), "file": file_hash, "bytes": offset}
        )
        log(f"group {group_id:2d} {' '.join(languages):<22} {offset:>11} bytes  {file_hash}")

    if len(directions) != len(selected):
        raise ToolError(f"{len(selected) - len(directions)} directions are in no group")

    manifest = {
        "format": MANIFEST_FORMAT,
        "collection": COLLECTION,
        "collectionTimestamp": changeset["timestamp"],
        "groups": groups,
        "directions": directions,
    }
    text = json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    write_atomic(Path(args.manifest), text.encode("ascii"))
    total = sum(g["bytes"] for g in groups)
    log(f"build complete: {len(groups)} groups, {len(directions)} directions, {total} bytes")


def cmd_verify(args: argparse.Namespace) -> None:
    manifest = json.loads(Path(args.manifest).read_bytes())
    directory = Path(args.dir)
    if manifest.get("format") != MANIFEST_FORMAT:
        raise ToolError(f"unknown manifest format {manifest.get('format')!r}")
    if manifest.get("collection") != COLLECTION:
        raise ToolError(f"unexpected collection {manifest.get('collection')!r}")

    groups = {}
    for group in manifest["groups"]:
        file_hash = group["file"]
        if not HEX64.match(file_hash):
            raise ToolError(f"group {group['id']}: file name is not 64 lowercase hex")
        data = (directory / file_hash).read_bytes()
        if len(data) != group["bytes"]:
            raise ToolError(f"group {group['id']}: {len(data)} bytes, manifest says {group['bytes']}")
        if hashlib.sha256(data).hexdigest() != file_hash:
            raise ToolError(f"group {group['id']}: SHA-256 does not match its name")
        groups[group["id"]] = (group, data)

    slices: dict[int, list[tuple[int, int]]] = {group_id: [] for group_id in groups}
    for key in sorted(manifest["directions"]):
        entry = manifest["directions"][key]
        group, data = groups[entry["group"]]
        if not is_complete(entry["files"]):
            raise ToolError(f"{key}: incomplete file set {sorted(entry['files'])}")
        for file_type, meta in entry["files"].items():
            start, length = meta["offset"], meta["length"]
            if start < 0 or length <= 0 or start + length > len(data):
                raise ToolError(f"{key} {file_type}: slice outside group {group['id']}")
            piece = data[start : start + length]
            if hashlib.sha256(piece).hexdigest() != meta["sha256"]:
                raise ToolError(f"{key} {file_type}: .zst SHA-256 mismatch")
            check_decompressed(
                f"{key} {file_type}",
                piece,
                {"decompressedSize": meta["decompressedBytes"], "decompressedHash": meta["decompressedSha256"]},
            )
            slices[entry["group"]].append((start, length))
        log(f"ok {key}")
    for group_id, (group, data) in groups.items():
        # The files must tile the group file exactly: no gap, no overlap.
        position = 0
        for start, length in sorted(slices[group_id]):
            if start != position:
                raise ToolError(f"group {group_id}: gap or overlap at byte {position}")
            position += length
        if position != len(data):
            raise ToolError(f"group {group_id}: files cover {position} of {len(data)} bytes")
    log(f"verify complete: {len(groups)} groups, {len(manifest['directions'])} directions, all hashes match")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    fetch = sub.add_parser("fetch", help="download and check the model files")
    fetch.add_argument("--cache", required=True, help="cache directory")
    fetch.set_defaults(func=cmd_fetch)

    build = sub.add_parser("build", help="write the group files and the manifest")
    build.add_argument("--cache", required=True, help="cache directory filled by fetch")
    build.add_argument("--out", required=True, help="directory for the group files")
    build.add_argument("--manifest", required=True, help="path of the manifest to write")
    build.set_defaults(func=cmd_build)

    verify = sub.add_parser("verify", help="check the group files against the manifest")
    verify.add_argument("--manifest", required=True, help="manifest written by build")
    verify.add_argument("--dir", required=True, help="directory holding the group files")
    verify.set_defaults(func=cmd_verify)

    args = parser.parse_args(argv)
    try:
        args.func(args)
    except ToolError as error:
        log(f"error: {error}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
