#!/usr/bin/env python3
"""Builds vendors/VENDORS.md: every vendor in the catalog, by category, with
its icon, so the catalog can be read on GitHub. Run again after adding or
changing a vendor:

    scripts/vendors-md.py

With --check it only says whether the file is up to date, and fails if not.
Needs Python 3.11 or later (tomllib).
"""

import sys
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parent.parent
vendors_dir = root / "vendors"
out = vendors_dir / "VENDORS.md"

# In the order vendors/README.md lists them, with the heading each gets.
categories = {
    "ai": "AI",
    "dev-tools": "Developer tools",
    "infrastructure": "Infrastructure",
    "observability": "Observability",
    "security": "Security",
    "data": "Data",
    "design": "Design",
    "productivity": "Productivity",
    "communication": "Communication",
    "analytics": "Analytics",
    "marketing": "Marketing",
    "sales": "Sales",
    "support": "Support",
    "finance": "Finance",
    "hr": "HR",
    "legal": "Legal",
    "education": "Education",
    "other": "Other",
}

pricing_names = {
    "seat": "per seat",
    "usage": "usage",
    "plan": "plan",
    "one_time": "one-time",
}


def icon(domain: str) -> str:
    url = f"https://www.google.com/s2/favicons?domain={domain}&sz=64"

    return f'<img src="{url}" width="20" height="20" alt="">'


def cell(text: str) -> str:
    """Text that can't break a table row."""

    return text.replace("|", "\\|")


def anchor(heading: str) -> str:
    """The anchor GitHub gives a heading."""

    return heading.lower().replace(" ", "-")


def load() -> list[dict]:
    vendors = []

    for path in sorted(vendors_dir.glob("*.toml")):
        if path.name.startswith("_"):
            continue

        with path.open("rb") as file:
            vendor = tomllib.load(file)

        vendor["id"] = path.stem
        vendors.append(vendor)

    return vendors


def load_processors() -> list[dict]:
    with (vendors_dir / "_processors.toml").open("rb") as file:
        return tomllib.load(file).get("processor", [])


def row(vendor: dict) -> str:
    id = vendor["id"]

    name = f"{icon(id)} **{cell(vendor['name'])}**"

    domains = [f"[{id}](https://{id})"]
    domains += [f"{domain}" for domain in vendor.get("domains", [])]

    pricing = ", ".join(pricing_names.get(p, p) for p in vendor.get("pricing", []))

    aliases = "<br>".join(cell(alias) for alias in vendor.get("aliases", []))

    return f"| {name} | {'<br>'.join(domains)} | {pricing} | {aliases} |"


def build() -> str:
    vendors = load()
    processors = load_processors()

    by_category: dict[str, list[dict]] = {}

    for vendor in vendors:
        by_category.setdefault(vendor["category"], []).append(vendor)

    present = [c for c in categories if c in by_category]

    lines = [
        "# Vendors",
        "",
        "<!-- Made by scripts/vendors-md.py from the files in this folder: don't edit, run it again. -->",
        "",
        f"The catalog knows **{len(vendors)} vendors**. To add one, see [README.md](README.md).",
        "",
        " · ".join(f"[{categories[c]}](#{anchor(categories[c])}) ({len(by_category[c])})" for c in present),
        "",
    ]

    for category in present:
        listed = sorted(by_category[category], key=lambda v: v["name"].lower())

        lines += [
            f"## {categories[category]}",
            "",
            "| Vendor | Domains | Pricing | Also on receipts as |",
            "|---|---|---|---|",
        ]

        lines += [row(vendor) for vendor in listed]
        lines.append("")

    lines += [
        "## Payment processors",
        "",
        "Their receipts are for other vendors, so their domains never identify a vendor.",
        "",
        "| Processor | Domains |",
        "|---|---|",
    ]

    for processor in sorted(processors, key=lambda p: p["name"].lower()):
        domains = processor.get("domains", [])
        first = domains[0] if domains else ""

        lines.append(f"| {icon(first)} **{cell(processor['name'])}** | {'<br>'.join(domains)} |")

    lines.append("")

    return "\n".join(lines)


def main() -> int:
    text = build()

    if "--check" in sys.argv[1:]:
        current = out.read_text() if out.exists() else ""

        if current != text:
            print(f"{out.relative_to(root)} is out of date: run scripts/vendors-md.py", file=sys.stderr)
            return 1

        return 0

    out.write_text(text)
    print(f"wrote {out.relative_to(root)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
