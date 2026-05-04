#!/usr/bin/env python3
"""Validate local href/src references in the static website."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
WEBSITE = ROOT / "website"
ATTR_RE = re.compile(r"""\b(?:href|src)=["']([^"']+)["']""", re.IGNORECASE)
EXTERNAL_SCHEMES = {"http", "https", "mailto", "tel", "data", "javascript"}


def local_target(raw: str) -> str | None:
    if not raw or raw.startswith("#"):
        return None
    parsed = urlsplit(raw)
    if parsed.scheme in EXTERNAL_SCHEMES or parsed.netloc:
        return None
    path = parsed.path
    if not path or path.startswith("#"):
        return None
    if path == "/":
        return "index.html"
    return path.lstrip("/")


def main() -> int:
    errors: list[str] = []
    for html in sorted(WEBSITE.glob("*.html")):
        body = html.read_text(encoding="utf-8")
        for raw in ATTR_RE.findall(body):
            target = local_target(raw)
            if target is None:
                continue
            path = WEBSITE / target
            if not path.exists():
                errors.append(f"{html.relative_to(ROOT)} -> {raw} missing {path.relative_to(ROOT)}")

    if errors:
        print("Broken website links:")
        for error in errors:
            print(f"  - {error}")
        return 1

    print("Website links OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
