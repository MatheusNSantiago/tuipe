#!/usr/bin/env python3
"""Import tuipe's approved assets from the frozen Monkeytype revision.

The script intentionally accepts a local checkout instead of scraping a moving
HTTP endpoint. It verifies the checkout revision, copies the six word packs
and two quote corpora byte-for-byte, extracts exactly the approved palettes,
and records source paths plus SHA-256 hashes in one deterministic manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
from pathlib import Path


REVISION = "781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5"
WORD_PACKS = {
    "portuguese/common": "portuguese.json",
    "portuguese/1k": "portuguese_1k.json",
    "portuguese/5k": "portuguese_5k.json",
    "english/common": "english.json",
    "english/1k": "english_1k.json",
    "english/5k": "english_5k.json",
}
QUOTE_PACKS = ("portuguese", "english")
THEMES = (
    "arch",
    "serika_dark",
    "serika",
    "catppuccin",
    "dracula",
    "nord",
    "gruvbox_dark",
    "rose_pine",
    "solarized_dark",
    "monokai",
)
THEME_FIELDS = (
    "bg",
    "main",
    "caret",
    "sub",
    "subAlt",
    "text",
    "error",
    "errorExtra",
    "colorfulError",
    "colorfulErrorExtra",
)


def revision(source: Path) -> str:
    return subprocess.check_output(
        ["git", "-C", str(source), "rev-parse", "HEAD"], text=True
    ).strip()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def theme_block(source: str, name: str) -> dict[str, str]:
    escaped = re.escape(name)
    pattern = rf'^  (?:"{escaped}"|{escaped}): \{{(?P<body>.*?)^  \}},$'
    match = re.search(pattern, source, flags=re.MULTILINE | re.DOTALL)
    if match is None:
        raise ValueError(f"theme {name!r} was not found in themes.ts")
    fields = dict(
        re.findall(r'^    (\w+): "(#[0-9a-fA-F]+)",', match.group("body"), re.MULTILINE)
    )
    missing = set(THEME_FIELDS) - fields.keys()
    if missing:
        raise ValueError(f"theme {name!r} is missing {sorted(missing)!r}")
    return {field: fields[field] for field in THEME_FIELDS}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True, help="checkout at the frozen revision")
    parser.add_argument("--output", type=Path, default=Path("assets"))
    args = parser.parse_args()

    source = args.source.resolve()
    if revision(source) != REVISION:
        raise SystemExit(f"expected {REVISION}, got {revision(source)}")

    output = args.output.resolve()
    if output.exists():
        shutil.rmtree(output)
    languages = output / "languages"
    quotes = output / "quotes"
    themes = output / "themes"
    languages.mkdir(parents=True)
    quotes.mkdir()
    themes.mkdir()

    entries: list[dict[str, str]] = []
    language_root = source / "frontend/static/languages"
    for name, source_name in WORD_PACKS.items():
        destination = languages / f"{name.replace('/', '_')}.json"
        origin = language_root / source_name
        shutil.copyfile(origin, destination)
        entries.append({"kind": "word_pack", "name": name, "source": str(origin.relative_to(source)), "sha256": sha256(destination)})

    quote_root = source / "frontend/static/quotes"
    for language in QUOTE_PACKS:
        destination = quotes / f"{language}.json"
        origin = quote_root / f"{language}.json"
        shutil.copyfile(origin, destination)
        entries.append({"kind": "quotes", "name": language, "source": str(origin.relative_to(source)), "sha256": sha256(destination)})

    source_themes = (source / "frontend/src/ts/constants/themes.ts").read_text()
    for name in THEMES:
        destination = themes / f"{name}.json"
        destination.write_text(json.dumps(theme_block(source_themes, name), indent=2, sort_keys=True) + "\n")
        entries.append({"kind": "theme", "name": name, "source": "frontend/src/ts/constants/themes.ts", "sha256": sha256(destination)})

    manifest = {"monkeytype_revision": REVISION, "entries": entries}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
