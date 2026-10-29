"""Write a deck at the edge of what the writer's slide estimate accepts, to look at in PowerPoint.

Usage: python3 slide_limits.py <agent-sandbox-broker binary> <out.pptx>

For each box a slide has (the title, a heading, paragraphs, wide capitals, a full table, a tall cell, a caption,
cells and paragraphs that start with tabs or spaces, and text in CJK, Cyrillic, symbols and dingbats) it finds with the writer itself the most content the box takes, checks
that one more is refused, and writes one deck of those slides with 24 provenance entries. Every slide should show its
text whole, inside its box and above the footer. The content is invented.
"""
import json
import pathlib
import subprocess
import sys

BINARY, OUT = sys.argv[1], sys.argv[2]
FIGURE = next(s["figure"]["png"] for s in json.loads(
    pathlib.Path(__file__).with_name("pptx.request.json").read_text())["sections"] if "figure" in s)
CJK, CYRILLIC = "食品成分表", "ЖИРНЫЕ "
SYMBOLS, DINGBATS = "→↔≠≤≥∑ ", "✓✔★ "


def body(title="Slide limits", sections=None):
    return {"kind": "pptx", "title": title, "generated_at": "2026-10-17T18:05:00Z",
            "sections": sections or [{"heading": "H", "paragraphs": ["p"]}],
            "provenance": [{"label": "Run", "value": "1"}]}


def written(request):
    run = subprocess.run([BINARY, "write-document"], input=json.dumps(request).encode(), capture_output=True)
    return run.returncode == 0, run.stdout, run.stderr.decode().strip()


def most(make, high=600):
    low = 0
    while low < high:
        mid = (low + high + 1) // 2
        low, high = (mid, high) if written(make(mid))[0] else (low, mid - 1)
    return low


def table(cell, rows=15, columns=4, caption="Caption.", ref="Reference."):
    return {"heading": "Table", "table": {"columns": [f"Column {c + 1}" for c in range(columns)],
                                          "rows": [[cell(r, c) for c in range(columns)] for r in range(rows)],
                                          "caption": caption, "ref": ref}}


def text(heading, unit):
    return lambda k: body(sections=[{"heading": heading, "paragraphs": [(unit * k).strip()]}])


PROBES = {
    "title": lambda k: body(title=("Vitamin D intake " * k).strip()),
    "heading": lambda k: body(sections=[{"heading": ("Table heading words " * k).strip(), "paragraphs": ["p"]}]),
    "six paragraphs": lambda k: body(sections=[{"heading": "Six paragraphs",
                                                 "paragraphs": [("word " * k).strip()] * 6}]),
    "wide capitals": text("Wide capitals", "WWWW "),
    "full table": lambda k: body(sections=[table(lambda _r, _c: ("cell text " * k).strip())]),
    "tall cell": lambda k: body(sections=[table(lambda _r, c: ("line\n" * k).strip() if c == 0 else "x", rows=3)]),
    "caption": lambda k: body(sections=[table(lambda _r, _c: "x", rows=2, caption=("caption words " * k).strip(),
                                              ref=("reference words " * 7).strip())]),
    "leading tabs": lambda k: body(sections=[table(lambda r, _c: "\t" * k + f"value {r + 1}", rows=3)]),
    "leading spaces": text("Leading spaces", " " * 40 + "value "),
    "CJK": text("CJK paragraph", CJK),
    "Cyrillic": text("Cyrillic capitals", CYRILLIC),
    "symbols": text("Arrows and math", SYMBOLS),
    "dingbats": text("Dingbats", DINGBATS),
    "CJK heading": lambda k: body(sections=[{"heading": CJK * k, "paragraphs": ["p"]}]),
    "CJK table": lambda k: body(sections=[table(lambda _r, _c: CJK * k, rows=8)]),
}

title, sections = "Slide limits", []
for name, make in PROBES.items():
    k = most(make)
    accepted, refused = written(make(k)), written(make(k + 1))
    # The caption stops at the body's 300-character limit, every other box at the slide estimate.
    assert k > 0 and accepted[0] and not refused[0], name
    print(f"{name}: {k} accepted; {k + 1} refused ({refused[2].rsplit(': ', 1)[-1]})")
    if name == "title":
        title = make(k)["title"]
    else:
        sections.extend(make(k)["sections"])
sections.append({"heading": "Figure", "figure": {"png": FIGURE, "caption": ("caption words " * 9).strip(),
                                                    "ref": ("reference words " * 7).strip()}})
deck = body(title=title, sections=sections)
deck["provenance"] = [{"label": f"Note {n}", "value": ("provenance words " * 17).strip()} for n in range(24)]
accepted, file, why = written(deck)
assert accepted, why
pathlib.Path(OUT).write_bytes(file)
print(f"{OUT}: {len(sections) + 1} slides before the provenance")
