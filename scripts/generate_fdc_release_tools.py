#!/usr/bin/env python3
"""Writes the reviewed FoodData Central Foundation tools for every enrolled Foundation release from the reviewed
April 2026 tools in profiles/reviewed_queries/fdc-foundation/<tool>/, one folder per tool and release:
<tool>-<yyyy>-<mm>/ with tool.json, template.sql and description.md.

What differs by release comes from tests/fixtures/reviewed_queries/fdc_foundation_releases.json: the source and its
title, the stored type of food.publication_date, and whether the release publishes food_category. A release without
food_category returns food_category as NULL text and drops that join; nothing else in a template changes. The
templates use unqualified relation names, so the read transaction's search path binds them to the release's schema.

    python3 scripts/generate_fdc_release_tools.py [--check] [--probes FILE]

--check compares instead of writing and exits 1 on any difference. Each description states its release's live check
(counts only) from tests/fixtures/reviewed_queries/fdc_foundation_release_checks.json, {"<release>": {"reader":
"<role>", "<tool>": "<sentence>"}}; --probes names another such file.
"""
import argparse
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROFILES = ROOT / "profiles/reviewed_queries/fdc-foundation"
RELEASES = ROOT / "tests/fixtures/reviewed_queries/fdc_foundation_releases.json"
CHECKS = ROOT / "tests/fixtures/reviewed_queries/fdc_foundation_release_checks.json"
BASE_RELEASE = "2026-04"
TOOLS = ("fdc-find-foods", "fdc-food-nutrients", "fdc-rank-foods-by-nutrient")
MONTHS = {"04": "April", "10": "October", "12": "December"}
CATEGORY_JOIN = "    LEFT JOIN food_category c ON c.id = f.food_category_id\n"
# April 2026's purposes state that release's duplicate amounts; each copy states its own release's count instead.
APRIL_DUPLICATES = {
    "fdc-food-nutrients": " The release publishes two amounts for one food and nutrient in four cases (nutrients 1106 and "
                          "2066); both rows are listed.",
    "fdc-rank-foods-by-nutrient": " The release publishes two amounts for one food and nutrient in four cases (nutrients "
                                  "1106 and 2066); both rows are ranked, so such a food can appear twice.",
}
# What each reviewed template does, as its April 2026 description states it, without that release's own data facts
# (a release's counts come from its live check instead). {nutrients} is the release's nutrient profile tool.
BEHAVIOUR = {
    "fdc-find-foods": (
        "The phrase is matched with `ILIKE '%' || phrase || '%'` against `food.description`, restricted to the "
        "release's `foundation_food` rows, so sample, sub-sample and acquisition foods are left out. SQL wildcards `%` "
        "and `_` keep their meaning. Ordering: an exact case-insensitive match first, then shorter descriptions, then "
        "the description in byte order and the FDC id, so selection at the limit is stable. A row-number predicate "
        "applies the typed limit (1 to 100, default 25) while the outer SELECT keeps the guard's fixed LIMIT 100. Empty "
        "text lists every Foundation Food."),
    "fdc-food-nutrients": (
        "Lists `food_nutrient` rows joined to `nutrient` for one FDC id that is a Foundation Food in the release (an id "
        "of a sample or sub-sample food returns no rows). Amounts are per 100 g as USDA publishes them; `data_points`, "
        "`min`, `max` and `median` are passed through unchanged (null where unpublished). Ordering follows the "
        "release's `nutrient.rank`, then the nutrient number and name in byte order and the row id. Where the release "
        "publishes two amounts for one food and nutrient, both rows are listed; duplicates are not merged."),
    "fdc-rank-foods-by-nutrient": (
        "Ranks Foundation Foods by `food_nutrient.amount` (per 100 g) for one nutrient id, highest or lowest first "
        "(closed choice, default highest), with a typed limit of 1 to 100 (default 20) and the guard's fixed LIMIT 100 "
        "on the outer SELECT. Rows without an amount are left out. Ties order by description in byte order, FDC id and "
        "row id, so selection at the limit is stable. Where the release publishes two amounts for one food and "
        "nutrient, both rows are ranked, so such a food can appear twice. Nutrient ids come from `{nutrients}`."),
}


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise SystemExit(f"the reviewed template changed: {old.strip()!r} found {text.count(old)} times")
    return text.replace(old, new)


def template_for(tool, base, release):
    if "food_category" in release["relations"] or tool == "fdc-food-nutrients":
        return base
    text = replace_once(base, "c.description AS food_category", "NULL::text AS food_category")
    return replace_once(text, CATEGORY_JOIN, "")


def release_words(release_key):
    year, month = release_key.split("-")
    return f"{MONTHS[month]} {year}"


def duplicates_sentence(tool, pairs):
    if pairs == 0:
        return " Each food and nutrient pair has one amount in this release."
    tail = "listed" if tool == "fdc-food-nutrients" else "ranked, so such a food can appear twice"
    return f" The release publishes two amounts for one food and nutrient in {pairs} cases; both rows are {tail}."


def definition_for(tool, base, release_key, release, checks):
    d = json.loads(json.dumps(base))
    d["key"] = f"{tool}-{release_key}"
    d["source"] = release["source"]
    d["title"] = f"{base['title']} ({release_words(release_key)})"
    if tool in APRIL_DUPLICATES:
        d["purpose"] = replace_once(base["purpose"], APRIL_DUPLICATES[tool],
                                    duplicates_sentence(tool, checks[release_key]["duplicate_pairs"]))
    no_category = "food_category" not in release["relations"]
    if no_category and tool != "fdc-food-nutrients":
        d["purpose"] = d["purpose"] + " This release publishes no food categories, so food_category is always empty."
    for parameter in d["parameters"]:
        parameter["description"] = parameter["description"].replace(
            "fdc-food-nutrients", f"fdc-food-nutrients-{release_key}")
        assert len(parameter["description"]) <= 300
    for column in d["output_columns"]:
        if column["name"] == "publication_date":
            column["type"] = release["publication_date_type"]
        if column["name"] == "food_category" and no_category:
            column["description"] = "Empty: this release publishes no food categories"
    assert len(d["title"]) <= 120 and len(d["purpose"]) <= 1000 and d["row_cap"] <= release["row_cap"]
    return d


def description_for(tool, definition, release_key, release, probes):
    differences = []
    if "food_category" not in release["relations"] and tool != "fdc-food-nutrients":
        differences.append("the release has no `food_category` table, so the template returns `NULL::text AS "
                           "food_category` and drops that join")
    if release["publication_date_type"] != "text" and tool == "fdc-find-foods":
        differences.append(f"`food.publication_date` is stored as `{release['publication_date_type']}`, and the "
                           "output column says so")
    lines = [
        f"# {definition['title']}, version 1",
        "",
        f"Generated by `scripts/generate_fdc_release_tools.py` from the reviewed `fdc-foundation-{BASE_RELEASE}` tool "
        f"`{tool}` for `{release['source']}`, {release['title']}. The template uses unqualified relation names, which "
        f"the read transaction binds to the release's schema `{release['schema']}`.",
        "",
        "Differences from April 2026: " + ("; ".join(differences) if differences else "none") + ".",
        "",
        BEHAVIOUR[tool].format(nutrients=f"fdc-food-nutrients-{release_key}"),
    ]
    probe = (probes or {}).get(release_key, {})
    if tool in probe:
        lines += ["", f"Checked on October 17, 2026 as `{probe['reader']}` in a read-only transaction (counts only): "
                      f"{probe[tool]}"]
    return "\n".join(lines) + "\n"


def wrap(text, width=120):
    """Wrap markdown paragraphs at the repository's width, leaving headings alone."""
    out = []
    for paragraph in text.split("\n\n"):
        if paragraph.startswith("#") or not paragraph.strip():
            out.append(paragraph.strip("\n"))
            continue
        words, line, lines = paragraph.split(), "", []
        for word in words:
            if line and len(line) + 1 + len(word) > width:
                lines.append(line)
                line = word
            else:
                line = f"{line} {word}" if line else word
        if line:
            lines.append(line)
        out.append("\n".join(lines))
    return "\n\n".join(out).rstrip("\n") + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--probes", default=str(CHECKS))
    args = parser.parse_args()
    releases = json.loads(RELEASES.read_text())["releases"]
    probes = json.loads(pathlib.Path(args.probes).read_text())
    differ = []
    for tool in TOOLS:
        base_dir = PROFILES / tool
        base = json.loads((base_dir / "tool.json").read_text())
        assert base["source"] == f"fdc-foundation-{BASE_RELEASE}"
        base_template = (base_dir / "template.sql").read_text()
        for release_key, release in sorted(releases.items()):
            if release_key == BASE_RELEASE:
                continue
            definition = definition_for(tool, base, release_key, release, probes)
            files = {
                "tool.json": json.dumps(definition, indent=2) + "\n",
                "template.sql": template_for(tool, base_template, release),
                "description.md": wrap(description_for(tool, definition, release_key, release, probes)),
            }
            folder = PROFILES / definition["key"]
            for name, content in files.items():
                path = folder / name
                if args.check:
                    if not path.exists() or path.read_text() != content:
                        differ.append(str(path.relative_to(ROOT)))
                else:
                    folder.mkdir(exist_ok=True)
                    path.write_text(content)
    if args.check and differ:
        print("out of date:", *differ, sep="\n  ")
        sys.exit(1)
    print("checked" if args.check else "wrote", (len(releases) - 1) * len(TOOLS), "tools")


if __name__ == "__main__":
    main()
