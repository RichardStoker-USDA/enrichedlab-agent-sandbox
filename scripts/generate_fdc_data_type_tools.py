#!/usr/bin/env python3
"""Writes the reviewed FoodData Central tools for the SR Legacy, FNDDS and Branded Foods sources that labs are granted:
find foods, one food's nutrients and foods ranked by one nutrient, in profiles/reviewed_queries/fdc-<kind>/ as
<tool>-<yyyy>-<mm>/ with tool.json, template.sql and description.md.

The templates follow the reviewed Foundation tools (profiles/reviewed_queries/fdc-foundation/), with each data type's
own table in place of foundation_food: sr_legacy_food (with its NDB number), survey_fndds_food (with the food code and
the WWEIA category, whose column names changed in October 2020) and branded_food (brand owner, brand name, GTIN or UPC
and branded category; find also matches the brand fields). Each kind table is unique on fdc_id and each WWEIA code is
unique, so no join repeats a food. Facts per source come from tests/fixtures/reviewed_queries/fdc_data_type_sources.json
and each description states its source's live check from fdc_data_type_checks.json (counts only).

    python3 scripts/generate_fdc_data_type_tools.py [--check]
"""
import argparse
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCES = ROOT / "tests/fixtures/reviewed_queries/fdc_data_type_sources.json"
CHECKS = ROOT / "tests/fixtures/reviewed_queries/fdc_data_type_checks.json"
MONTHS = {"04": "April", "10": "October", "12": "December"}
KINDS = {
    "sr-legacy": {"folder": "fdc-sr-legacy", "prefix": "fdc-sr", "noun": "SR Legacy food", "nouns": "SR Legacy foods"},
    "fndds": {"folder": "fdc-fndds", "prefix": "fdc-fndds", "noun": "FNDDS survey food", "nouns": "FNDDS survey foods"},
    "branded": {"folder": "fdc-branded", "prefix": "fdc-branded", "noun": "Branded Food", "nouns": "Branded Foods"},
}
ORDER_FIND = """           row_number() OVER (
               ORDER BY (lower(f.description) = lower($1)) DESC, length(f.description) ASC,
                        f.description COLLATE "C" ASC, f.fdc_id ASC
           ) AS ordinal"""
ORDER_RANK = """           row_number() OVER (
               ORDER BY CASE WHEN $2 = 'lowest' THEN fn.amount END ASC NULLS LAST,
                        CASE WHEN $2 = 'highest' THEN fn.amount END DESC NULLS LAST,
                        f.description COLLATE "C" ASC, f.fdc_id ASC, fn.id ASC
           ) AS ordinal"""


def find_template(kind, s):
    if kind == "sr-legacy":
        columns, joins, where, out = ("f.fdc_id, k.ndb_number, f.description, c.description AS food_category, f.publication_date",
                                      "    JOIN sr_legacy_food k ON k.fdc_id = f.fdc_id\n"
                                      "    LEFT JOIN food_category c ON c.id = f.food_category_id\n",
                                      "f.description ILIKE '%' || $1 || '%'",
                                      "fdc_id, ndb_number, description, food_category, publication_date")
    elif kind == "fndds":
        columns, joins, where, out = ("f.fdc_id, k.food_code, f.description, w.wweia_food_category_description AS wweia_category, "
                                      "f.publication_date",
                                      "    JOIN survey_fndds_food k ON k.fdc_id = f.fdc_id\n"
                                      f"    LEFT JOIN wweia_food_category w ON w.{s['wweia_code']} = k.{s['food_wweia']}\n",
                                      "f.description ILIKE '%' || $1 || '%'",
                                      "fdc_id, food_code, description, wweia_category, publication_date")
    else:
        columns, joins, where, out = ("f.fdc_id, f.description, k.brand_owner, k.brand_name, k.gtin_upc, k.branded_food_category, "
                                      "f.publication_date",
                                      "    JOIN branded_food k ON k.fdc_id = f.fdc_id\n",
                                      "(f.description ILIKE '%' || $1 || '%' OR k.brand_name ILIKE '%' || $1 || '%'\n"
                                      "           OR k.brand_owner ILIKE '%' || $1 || '%')",
                                      "fdc_id, description, brand_owner, brand_name, gtin_upc, branded_food_category, publication_date")
    return (f"WITH matched AS (\n    SELECT {columns},\n{ORDER_FIND}\n    FROM food f\n{joins}    WHERE {where}\n)\n"
            f"SELECT {out}\nFROM matched\nWHERE ordinal <= $2\nORDER BY ordinal\nLIMIT 100\n")


def nutrient_join(s):
    # FNDDS 2022-10 and 2024-10 keep the nutrient number in food_nutrient.nutrient_id (the fixture's nutrient_key).
    return "n.nutrient_nbr = fn.nutrient_id::text" if s["nutrient_key"] == "number" else "n.id = fn.nutrient_id"


def nutrients_template(kind, s):
    table = {"sr-legacy": "sr_legacy_food", "fndds": "survey_fndds_food", "branded": "branded_food"}[kind]
    return ("SELECT n.id AS nutrient_id, n.nutrient_nbr, n.name AS nutrient, n.unit_name AS unit, fn.amount,\n"
            "       fn.data_points, fn.min AS min_amount, fn.max AS max_amount, fn.median AS median_amount\n"
            f"FROM food_nutrient fn\nJOIN nutrient n ON {nutrient_join(s)}\n"
            f"JOIN {table} k ON k.fdc_id = fn.fdc_id\nWHERE fn.fdc_id = $1\n"
            "ORDER BY n.rank ASC NULLS LAST, n.nutrient_nbr COLLATE \"C\" ASC NULLS LAST, n.name COLLATE \"C\" ASC, fn.id ASC\n"
            "LIMIT 500\n")


def rank_template(kind, s):
    if kind == "sr-legacy":
        columns, joins, out = ("f.fdc_id, k.ndb_number, f.description, c.description AS food_category",
                               "    JOIN sr_legacy_food k ON k.fdc_id = fn.fdc_id\n    JOIN food f ON f.fdc_id = fn.fdc_id\n"
                               "    JOIN nutrient n ON n.id = fn.nutrient_id\n"
                               "    LEFT JOIN food_category c ON c.id = f.food_category_id\n",
                               "fdc_id, ndb_number, description, food_category")
    elif kind == "fndds":
        columns, joins, out = ("f.fdc_id, k.food_code, f.description, w.wweia_food_category_description AS wweia_category",
                               "    JOIN survey_fndds_food k ON k.fdc_id = fn.fdc_id\n    JOIN food f ON f.fdc_id = fn.fdc_id\n"
                               f"    JOIN nutrient n ON {nutrient_join(s)}\n"
                               f"    LEFT JOIN wweia_food_category w ON w.{s['wweia_code']} = k.{s['food_wweia']}\n",
                               "fdc_id, food_code, description, wweia_category")
    else:
        columns, joins, out = ("f.fdc_id, f.description, k.brand_owner, k.brand_name",
                               "    JOIN branded_food k ON k.fdc_id = fn.fdc_id\n    JOIN food f ON f.fdc_id = fn.fdc_id\n"
                               "    JOIN nutrient n ON n.id = fn.nutrient_id\n",
                               "fdc_id, description, brand_owner, brand_name")
    return (f"WITH ranked AS (\n    SELECT {columns}, n.name AS nutrient,\n           n.unit_name AS unit, fn.amount,\n"
            f"{ORDER_RANK}\n    FROM food_nutrient fn\n{joins}"
            f"    WHERE {'n.id' if s['nutrient_key'] == 'number' else 'fn.nutrient_id'} = $1 AND fn.amount IS NOT NULL\n)\n"
            f"SELECT {out}, nutrient, unit, amount\nFROM ranked\nWHERE ordinal <= $3\nORDER BY ordinal\nLIMIT 100\n")


def column(name, kind_type, description):
    return {"name": name, "type": kind_type, "description": description}


def duplicates_sentence(tool, pairs):
    if pairs == 0:
        return " Each food and nutrient pair has one amount in this release."
    tail = "listed" if tool == "nutrients" else "ranked, so such a food can appear twice"
    return f" The release publishes two amounts for one food and nutrient in {pairs} cases; both rows are {tail}."


def definitions(kind, source_key, s, pairs):
    k, release = KINDS[kind], s["release"]
    words = f"{MONTHS[release[5:]]} {release[:4]}"
    nutrients_key = f"{k['prefix']}-food-nutrients-{release}"
    identity = {"sr-legacy": [column("ndb_number", "integer", "NDB number of the SR Legacy food")],
                "fndds": [column("food_code", "integer", "FNDDS food code")],
                "branded": []}[kind]
    category = {"sr-legacy": [column("food_category", "text", "Food category description as published")],
                "fndds": [column("wweia_category", "text", "WWEIA food category description as published")],
                "branded": [column("brand_owner", "text", "Brand owner as published"),
                            column("brand_name", "text", "Brand name as published, where given")]}[kind]
    find_extra = {"branded": [column("gtin_upc", "text", "GTIN or UPC as published"),
                              column("branded_food_category", "text", "Branded food category as published")]}.get(kind, [])
    match = {"sr-legacy": "whose description contains a phrase",
             "fndds": "whose description contains a phrase",
             "branded": "whose description, brand name or brand owner contains a phrase"}[kind]
    per = {"branded": "per 100 g as USDA calculates it from the label"}.get(kind, "per 100 g")
    find_fields = {"sr-legacy": "FDC id, NDB number, description, food category and publication date",
                   "fndds": "FDC id, food code, description, WWEIA category and publication date",
                   "branded": "FDC id, description, brand owner, brand name, GTIN or UPC, branded category and publication date"}[kind]
    rank_fields = {"sr-legacy": "FDC id, NDB number, description, food category",
                   "fndds": "FDC id, food code, description, WWEIA category",
                   "branded": "FDC id, description, brand owner, brand name"}[kind]
    find = {
        "key": f"{k['prefix']}-find-foods-{release}", "version": 1, "source": source_key,
        "title": f"Find {k['nouns']} by description ({words})",
        "purpose": (f"Find {k['nouns']} in this FoodData Central release {match} (case-insensitive; SQL wildcards % and _ keep "
                    f"their meaning), with each food's {find_fields}. An exact description match comes first, then shorter "
                    "descriptions."),
        "parameters": [
            {"name": "query", "description": f"Phrase to look for; empty text lists every {k['noun']}", "type": "text",
             "required": True, "max_length": 256},
            {"name": "limit", "description": "Maximum rows", "type": "integer", "required": False, "minimum": 1,
             "maximum": 100, "default": 25}],
        "output_columns": ([column("fdc_id", "integer", "FoodData Central id of the food")] + identity
                           + [column("description", "text", "Food description as published")]
                           + (category if kind != "branded" else category + find_extra)
                           + [column("publication_date", s["publication_date_type"], "Publication date as published")]),
        "row_cap": 100, "state": "enabled"}
    nutrients = {
        "key": nutrients_key, "version": 1, "source": source_key,
        "title": f"Nutrient profile of one {k['noun']} ({words})",
        "purpose": (f"List every nutrient this FoodData Central release reports for one {k['noun']}: the nutrient's id, number, "
                    f"name and unit, the amount {per} of the food, and the analytical data points with their minimum, "
                    "maximum and median where published. Rows follow the release's nutrient rank." + duplicates_sentence("nutrients", pairs)),
        "parameters": [{"name": "fdc_id", "description": f"FoodData Central id of a {k['noun']} in this release",
                        "type": "integer", "required": True, "minimum": 1, "maximum": 2147483647}],
        "output_columns": [
            column("nutrient_id", "integer", "Nutrient id in this release"),
            column("nutrient_nbr", "text", "Nutrient number as published"),
            column("nutrient", "text", "Nutrient name"), column("unit", "text", "Unit of the amount"),
            column("amount", "numeric", f"Amount {per} of the food, as published"),
            column("data_points", "integer", "Number of analytical data points behind the amount, where published"),
            column("min_amount", "numeric", "Lowest analytical value, where published"),
            column("max_amount", "numeric", "Highest analytical value, where published"),
            column("median_amount", "numeric", "Median analytical value, where published")],
        "row_cap": 500, "state": "enabled"}
    rank = {
        "key": f"{k['prefix']}-rank-foods-by-nutrient-{release}", "version": 1, "source": source_key,
        "title": f"Rank {k['nouns']} by one nutrient ({words})",
        "purpose": (f"Rank the {k['nouns']} in this FoodData Central release by their amount {per} of one nutrient, highest or "
                    f"lowest first, with each food's {rank_fields}, the nutrient's name and unit and the amount. Foods "
                    "without a published amount for the nutrient are left out. Ties order by description, then FDC id."
                    + duplicates_sentence("rank", pairs)),
        "parameters": [
            {"name": "nutrient_id", "description": f"Nutrient id in this release ({nutrients_key} lists them, for example 1003 "
                                                   "protein or 1162 vitamin C)",
             "type": "integer", "required": True, "minimum": 1, "maximum": 99999},
            {"name": "order", "description": "highest or lowest amount first", "type": "enum", "required": False,
             "choices": ["highest", "lowest"], "default": "highest"},
            {"name": "limit", "description": "Maximum rows", "type": "integer", "required": False, "minimum": 1,
             "maximum": 100, "default": 20}],
        "output_columns": ([column("fdc_id", "integer", "FoodData Central id of the food")] + identity
                           + [column("description", "text", "Food description as published")] + category
                           + [column("nutrient", "text", "Nutrient name"), column("unit", "text", "Unit of the amount"),
                              column("amount", "numeric", f"Amount {per} of the food, as published")]),
        "row_cap": 100, "state": "enabled"}
    for d in (find, nutrients, rank):
        assert len(d["title"]) <= 120 and len(d["purpose"]) <= 1000 and d["row_cap"] <= s["row_cap"]
        assert all(len(p["description"]) <= 300 for p in d["parameters"])
    return [(find, find_template(kind, s)), (nutrients, nutrients_template(kind, s)), (rank, rank_template(kind, s))]


def wrap(text, width=120):
    out = []
    for paragraph in text.split("\n\n"):
        if paragraph.startswith("#") or not paragraph.strip():
            out.append(paragraph.strip("\n"))
            continue
        line, lines = "", []
        for word in paragraph.split():
            if line and len(line) + 1 + len(word) > width:
                lines.append(line)
                line = word
            else:
                line = f"{line} {word}" if line else word
        lines.append(line)
        out.append("\n".join(lines))
    return "\n\n".join(out).rstrip("\n") + "\n"


def description(definition, kind, source_key, s, check):
    k = KINDS[kind]
    table = {"sr-legacy": "`sr_legacy_food`", "fndds": "`survey_fndds_food`", "branded": "`branded_food`"}[kind]
    lines = [f"# {definition['title']}, version 1", "",
             f"Generated by `scripts/generate_fdc_data_type_tools.py` for `{source_key}`, {s['title']}. It follows the "
             f"reviewed Foundation tool of the same purpose with {table} in place of `foundation_food`, so only "
             f"{k['nouns']} are read; that table is unique on `fdc_id`, so no food repeats. Unqualified relation names bind "
             f"to the source's schema `{s['schema']}`."]
    if kind == "branded" and definition["key"].startswith("fdc-branded-find-foods"):
        lines += ["", "The phrase is matched against the description, the brand name and the brand owner; the exact-match "
                      "ordering looks at the description only. Empty text lists every Branded Food, about two million rows "
                      "sorted, so it is the slowest call."]
    if kind == "fndds" and "food-nutrients" in definition["key"]:
        lines += ["", "FNDDS amounts are per 100 g of the food as consumed; FNDDS publishes no analytical data points, so "
                      "`data_points`, `min_amount`, `max_amount` and `median_amount` are empty where the release leaves them so."]
    if s["nutrient_key"] == "number" and "find-foods" not in definition["key"]:
        lines += ["", "This release stores the nutrient number (for example 203 for protein) in `food_nutrient.nutrient_id`, "
                      "not the nutrient id, so the template joins `nutrient.nutrient_nbr` to it and selects by `nutrient.id`; "
                      "the lead still names nutrients by id (1003 for protein). Every number this release uses names "
                      "exactly one nutrient, so the join repeats no row."]
    if kind == "fndds":
        lines += ["", f"The WWEIA category joins `wweia_food_category.{s['wweia_code']}` to `survey_fndds_food.{s['food_wweia']}`, "
                      "the column names this release uses; each WWEIA code is unique."]
    if check:
        lines += ["", f"Checked on October 17, 2026 as `{check[0]}` in a read-only transaction (counts only): {check[1]}"]
    return wrap("\n".join(lines))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    sources = json.loads(SOURCES.read_text())["sources"]
    checks = json.loads(CHECKS.read_text()) if CHECKS.exists() else {"sources": {}, "tools": {}}
    differ, written = [], 0
    for source_key, s in sorted(sources.items()):
        kind = s["kind"]
        source_check = checks["sources"].get(source_key)
        pairs = source_check["duplicate_pairs"] if source_check else 0
        for definition, template in definitions(kind, source_key, s, pairs):
            sentence = checks["tools"].get(definition["key"])
            check = (source_check["reader"], sentence) if source_check and sentence else None
            files = {"tool.json": json.dumps(definition, indent=2) + "\n", "template.sql": template,
                     "description.md": description(definition, kind, source_key, s, check)}
            folder = ROOT / "profiles/reviewed_queries" / KINDS[kind]["folder"] / definition["key"]
            for name, content in files.items():
                path = folder / name
                if args.check:
                    if not path.exists() or path.read_text() != content:
                        differ.append(str(path.relative_to(ROOT)))
                else:
                    folder.mkdir(parents=True, exist_ok=True)
                    path.write_text(content)
            written += 1
    if args.check and differ:
        print("out of date:", *differ, sep="\n  ")
        sys.exit(1)
    print("checked" if args.check else "wrote", written, "tools")


if __name__ == "__main__":
    main()
