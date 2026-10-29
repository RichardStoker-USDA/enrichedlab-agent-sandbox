#!/usr/bin/env python3
"""Inspect original broker downloads and build an offline R reference workbench."""
import argparse
import csv
import hashlib
import html
import itertools
import json
import math
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import zipfile


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def extract(archive, target):
    target.mkdir(mode=0o700)
    with tarfile.open(archive) as source:
        members = source.getmembers()
        if len(members) > 1000 or sum(m.size for m in members) > 64 * 1024 * 1024:
            raise ValueError("reference archive exceeds limits")
        if len({m.name for m in members}) != len(members):
            raise ValueError("duplicate archive member")
        for member in members:
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts or not (member.isfile() or member.isdir()):
                raise ValueError("unsafe archive member")
        source.extractall(target, filter="data")
    expected = set()
    for line in (target / "SHA256SUMS").read_text().splitlines():
        checksum, name = line.split("  ", 1)
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or name in expected:
            raise ValueError("invalid checksum entry")
        expected.add(name)
        if sha(target / name) != checksum:
            raise ValueError("original archive checksum mismatch")
    actual = {p.relative_to(target).as_posix() for p in target.rglob("*") if p.is_file()}
    if actual != expected | {"SHA256SUMS"}:
        raise ValueError("archive inventory differs")


def verify(root):
    manifest = json.loads((root / "review_manifest.json").read_text())
    names = [item["path"] for item in manifest["files"]]
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()}
    if len(names) != len(set(names)) or actual != set(names) | {"review_manifest.json"}:
        raise ValueError("review file inventory differs")
    for item in manifest["files"]:
        name = PurePosixPath(item["path"])
        if name.is_absolute() or ".." in name.parts:
            raise ValueError("unsafe manifest path")
        path = root / name
        if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
            raise ValueError("unsafe manifest file")
        if path.stat().st_size != item["bytes"] or sha(path) != item["sha256"]:
            raise ValueError("review file changed: " + str(name))
    print(json.dumps({"integrity": "passed", "files": len(manifest["files"])}))


def rerun(root, output):
    verify(root)
    output.mkdir(mode=0o700)
    results = []
    for method in ("mean", "median"):
        original = root / method
        params = json.loads((original / "run_parameters.json").read_text())
        archive = output / f"{method}.tar.gz"
        with (output / f"{method}.log").open("w") as log:
            subprocess.run(["Rscript", "--vanilla", str(original / "code/bootstrap_replay.R"),
                            str(original / "input/source.zip"), str(archive),
                            params["statistic"], str(params["seed"])], check=True,
                           stdout=log, stderr=subprocess.STDOUT)
        repeat = output / method
        extract(archive, repeat)
        comparisons = []
        for name in ("results/bootstrap_intervals.csv", "results/bootstrap_resamples.csv",
                     "results/descriptive/summary.csv"):
            with (original / name).open(newline="") as first, (repeat / name).open(newline="") as second:
                a, b = csv.reader(first), csv.reader(second)
                if next(a) != next(b):
                    raise ValueError("rerun columns differ")
                count = 0
                for x, y in itertools.zip_longest(a, b):
                    if x is None or y is None or len(x) != len(y):
                        raise ValueError("rerun shape differs")
                    for left, right in zip(x, y):
                        if left != right and not math.isclose(float(left), float(right), rel_tol=1e-12, abs_tol=1e-12):
                            raise ValueError("rerun value differs")
                    count += 1
            comparisons.append({"file": name, "rows": count, "match": True})
        results.append({"method": method, "comparisons": comparisons})
    receipt = {"localReplay": results, "scientificReview": "pending", "tolerance": "absolute=relative=1e-12"}
    (output / "comparison.json").write_text(json.dumps(receipt, indent=2))
    print(json.dumps(receipt))


def table(path):
    rows = []
    with path.open(newline="", encoding="utf-8") as source:
        for index, values in enumerate(csv.reader(source)):
            tag = "th" if index == 0 else "td"
            rows.append("<tr>" + "".join(f"<{tag}>{html.escape(v)}</{tag}>" for v in values) + "</tr>")
    return "".join(rows)


def build(source, output):
    output.mkdir(mode=0o700)
    lifecycle = json.loads((source / "lifecycle_receipt.json").read_text())
    acceptance = json.loads((source / "replay_acceptance.json").read_text())
    if not lifecycle["computePodsAbsent"] or not lifecycle["sandboxesAbsent"]:
        raise ValueError("compute teardown receipt required")
    for name in ("lifecycle_receipt.json", "replay_acceptance.json"):
        shutil.copyfile(source / name, output / name)
    shutil.copyfile(Path(__file__), output / "retained_review.py")
    panels = []
    history = []
    for method in ("mean", "median"):
        archive = f"{method}_scientific_replay.tar.gz"
        receipt = json.loads((source / f"{method}_download_receipt.json").read_text())
        if sha(source / archive) != receipt["sha256"] or (source / archive).stat().st_size != receipt["bytes"]:
            raise ValueError("download receipt mismatch")
        if receipt["task"]["status"] != "completed" or receipt["unauthenticatedStatus"] != 401:
            raise ValueError("completed authenticated download receipt required")
        shutil.copyfile(source / archive, output / archive)
        shutil.copyfile(source / f"{method}_download_receipt.json", output / f"{method}_download_receipt.json")
        extract(output / archive, output / method)
        root = output / method
        params = json.loads((root / "run_parameters.json").read_text())
        measured = next(x for x in acceptance["localReplay"] if x["method"] == method)
        if measured["downloadSha256"] != receipt["sha256"] or not all(x["numericMatch"] for x in measured["comparisons"]):
            raise ValueError("local replay receipt does not match download")
        subprocess.run(["pdftoppm", "-png", "-singlefile", "-scale-to", "1500",
                        str(root / "results/bootstrap_intervals.pdf"), str(output / f"{method}_figure")],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        panels.append(f'''<section id="{method}" class="analysis" {'hidden' if method == 'median' else ''}>
<div class="figure-heading"><div><h2>Group {method} and uncertainty</h2><p class="quiet">Percentile bootstrap · {params['resamplesPerGroup']:,} resamples per group</p></div><a href="{method}/results/bootstrap_intervals.pdf">Open R figure PDF</a></div>
<figure class="figure"><img src="{method}_figure.png" alt="R-rendered percentile bootstrap intervals for the group {method}."></figure>
<p class="caption">The image previews the original R PDF. Points show each group's {method}; vertical lines show the 2.5th to 97.5th percentiles of resampled estimates.</p>
<details><summary>Inspect the exact result table</summary><div class="table-scroll"><table>{table(root / 'results/bootstrap_intervals.csv')}</table></div><p><a href="{method}/results/bootstrap_intervals.csv">Interval CSV</a> · <a href="{method}/results/bootstrap_resamples.csv">All resampled values</a></p></details>
<div class="lineage"><h3>Follow this result</h3><p><a href="{method}/input/reference/inputs/observations.csv">Original measurements</a> → <a href="{method}/code/bootstrap_replay.R">Exact R method</a> → <a href="{method}/results/bootstrap_intervals.pdf">R figure</a></p></div>
<details><summary>Read method, source and reproduction details</summary><p>Seed: <code>{params['seed']}</code>. Scientific review: pending.</p><p><a href="{method}/input/reference/inputs/source.txt">Data origin</a> · <a href="{method}/method.txt">Method and assumptions</a> · <a href="{method}/environment.txt">Cluster R environment</a> · <a href="{method}/stdout.log">R execution log</a></p><pre>{html.escape((root / 'README.md').read_text())}</pre></details></section>''')
        events = receipt["events"]["events"]
        labels = {"task_submitted": "Analysis requested", "task_claimed": "Worker reserved",
                  "agent_started": "Worker began task", "sandbox_created": "Isolated workspace created",
                  "sandbox_pod_observed": "Compute workspace found", "sandbox_terminal_observed": "Compute finished",
                  "agent_completed": "Results retained"}
        event_rows = "".join(f"<li><span title='{html.escape(e['code'])}'>{html.escape(labels.get(e['code'], e['code'].replace('_', ' ')))}</span><time>{e['occurred_at'][2]:02d}:{e['occurred_at'][3]:02d}:{e['occurred_at'][4]:02d} UTC</time></li>" for e in events)
        history.append(f'''<details><summary>{method.title()} task: completed, retained and downloaded</summary><p>Task <code>{html.escape(receipt['taskId'])}</code></p><ol class="run-history">{event_rows}</ol><p><a href="{method}_download_receipt.json">API events and download receipt</a> · <a href="{archive}" download>Original downloaded package</a></p><code class="checksum">SHA-256 {receipt['sha256']}</code></details>''')
    base = Path(__file__).with_name("review.html").read_text()
    style = base.split("<style>", 1)[1].split("</style>", 1)[0]
    page = '''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Retained R analysis record</title><style>''' + style + '''
html{scroll-behavior:smooth}.topbar a{color:white;text-decoration:none;font-weight:600}.page-heading{padding-top:24px;max-width:850px}nav{padding:0 0 16px;border-bottom:1px solid var(--rule)}.method-switch button{background:var(--surface);color:var(--ink);border:1px solid var(--rule);border-radius:3px}.analysis[hidden]{display:none}.checksum{display:block;overflow-wrap:anywhere}details{margin:1.1rem 0}pre{white-space:pre-wrap;overflow-wrap:anywhere}.method-switch{display:flex;gap:.4rem;margin:1rem 0}.method-switch button{padding:.65rem 1rem;min-width:7rem}.method-switch [aria-pressed=true]{background:#022851;color:white;border-color:#022851}[data-theme=dark] .method-switch [aria-pressed=true]{border-color:#ffbf00}.run-history li{display:flex;justify-content:space-between;gap:1rem}.document{padding-top:2rem}nav a{margin-right:1.3rem}@media(prefers-reduced-motion:reduce){html{scroll-behavior:auto}}</style></head><body><main class="workspace">
<header class="topbar"><a href="#results">Research / Analysis record</a><button class="theme" id="theme">Dark mode</button></header>
<div class="page-heading"><p class="eyebrow">PUBLIC REFERENCE STUDY</p><h1>ToothGrowth: bootstrap analysis</h1><p>Two reviewed R recipes, one unchanged input. Inspect the data and method, then reproduce the results in your own R environment.</p></div>
<nav aria-label="Analysis record"><a href="#results">Results</a><a href="#history">Run history</a><a href="#reproduce">Reproduce</a></nav>
<section id="results"><div class="method-switch" aria-label="Choose analysis"><button data-method="mean" aria-pressed="true">Mean</button><button data-method="median" aria-pressed="false">Median</button></div><div class="result-layout"><div>''' + "".join(panels) + '''</div><aside class="review-panel"><h2>What is verified</h2><dl class="status-list"><div><dt>Cluster execution</dt><dd>Completed</dd></div><div><dt>Downloaded files</dt><dd>Checksums match</dd></div><div><dt>Local R replay</dt><dd>Numeric results match</dd></div><div class="pending"><dt>Scientific review</dt><dd>Awaiting a scientist</dd></div></dl><h3>Review the science</h3><ol><li>Confirm the data and experimental design fit the question.</li><li>Assess independence and the suitability of within-group resampling.</li><li>Inspect the figures and rerun the exact method.</li></ol><p class="quiet">These are public guinea-pig data from R's ToothGrowth dataset, not a private mouse study. These descriptive intervals establish no causal or confirmatory finding.</p></aside></div></section>
<section id="history" class="document"><h2>What happened to the work</h2><p>The authenticated caller selected a reviewed recipe. The broker assigned its recorded runtime profile, copied and sealed the input, then admitted the isolated compute. An output service retained the original result bytes. The compute was removed before the downloaded packages were checked.</p><p>''' + str(lifecycle["overlapSeconds"]) + ''' seconds of measured overlap; separate task, input and output identities. This is a two-task qualification. It does not establish general autoscaling.</p>''' + "".join(history) + '''<p><a href="lifecycle_receipt.json">Compute identity and teardown receipt</a> · <a href="replay_acceptance.json">Local replay comparison</a></p><details><summary>How tool choice and memory work in this example</summary><p>A trusted caller chose the mean or median catalog recipe. No model chose or wrote this code. The runtime owns paths, placement, resources and teardown. Template-generated and fully custom code remain separate future execution tiers.</p><p>Task IDs, ordered events, exact artifacts and attempt predecessors form the recovery record. Context summaries must reference that record. This example does not yet implement a long-horizon research agent or automatic context pruning.</p></details></section>
<section id="reproduce" class="document"><h2>Reproduce in RStudio or a terminal</h2><ol class="instructions"><li><h3>Extract the full review package</h3><p>Open this folder as your working directory. Original task archives and their extracted files are both included.</p></li><li><h3>Verify the files</h3><pre>python3 retained_review.py verify .</pre><p>Python 3.12 or later. File hashes establish integrity relative to this manifest; they are not a signature or scientific approval.</p></li><li><h3>Run both methods and compare</h3><pre>python3 retained_review.py rerun . --output ../local_replay</pre><p>Use RStudio's Terminal tab or your usual terminal with R installed. Choose a new output directory. This reruns the exact code and compares all resampled values and numeric summaries, with absolute and relative tolerance 1e-12. Original results remain unchanged. PDF appearance may differ between R installations.</p></li></ol></section><footer class="footer"><span>Scientific review pending</span><a href="review_manifest.json">Files and checksums</a></footer></main><script>
const root=document.documentElement,theme=document.getElementById('theme');try{root.dataset.theme=localStorage.getItem('enrichedlab-review-theme')||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light')}catch(e){root.dataset.theme='light'}function label(){theme.textContent=root.dataset.theme==='dark'?'Light mode':'Dark mode'}label();theme.onclick=()=>{root.dataset.theme=root.dataset.theme==='dark'?'light':'dark';try{localStorage.setItem('enrichedlab-review-theme',root.dataset.theme)}catch(e){}label()};for(const b of document.querySelectorAll('[data-method]'))b.onclick=()=>{for(const x of document.querySelectorAll('[data-method]')){const active=x===b;x.setAttribute('aria-pressed',String(active));document.getElementById(x.dataset.method).hidden=!active}};
</script></body></html>'''
    (output / "index.html").write_text(page, encoding="utf-8")
    files = [{"path": p.relative_to(output).as_posix(), "bytes": p.stat().st_size, "sha256": sha(p)}
             for p in sorted(output.rglob("*")) if p.is_file()]
    (output / "review_manifest.json").write_text(json.dumps({"schema": "retained-r-review/v1", "scientificReview": "pending", "files": files}, indent=2))
    verify(output)
    with zipfile.ZipFile(output.with_suffix(".zip"), "x", compression=zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(output.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(output))
    print(json.dumps({"review": str(output), "zipSha256": sha(output.with_suffix('.zip'))}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("build", "verify", "rerun"))
    parser.add_argument("source", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    if args.operation == "verify":
        verify(source)
    else:
        if args.output is None:
            parser.error("--output is required")
        output = args.output.resolve()
        if output.exists() or output.is_relative_to(source):
            parser.error("choose a new output directory outside the original package")
        (build if args.operation == "build" else rerun)(source, output)
