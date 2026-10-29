#!/usr/bin/env python3
"""Build and verify a portable reference-method package without reading data into an LLM."""
import argparse
import csv
import hashlib
import html
import json
import math
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import zipfile
import sys
from urllib.parse import quote

ROOTS = ("inputs", "scripts", "outputs")
FACTS = {"run_id", "job_uid", "image", "image_id", "runtime_class", "source_revision", "started_at", "finished_at", "exit_code"}
MAX_BYTES = 8 * 1024 * 1024


def digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def safe_file(root, name):
    parts = PurePosixPath(name)
    if parts.is_absolute() or ".." in parts.parts or not parts.parts:
        raise ValueError("unsafe package path")
    p = root.joinpath(*parts.parts)
    if any(q.is_symlink() for q in (p, *p.parents) if q != root.parent):
        raise ValueError("symlink in package path")
    if not p.is_file() or not p.resolve().is_relative_to(root.resolve()) or p.stat().st_size > MAX_BYTES:
        raise ValueError("invalid package file")
    return p


def inventory(root):
    names = []
    for directory in ROOTS:
        for p in sorted((root / directory).rglob("*")):
            if p.is_symlink():
                raise ValueError("symlinks are not allowed")
            if p.is_file():
                name = p.relative_to(root).as_posix()
                safe_file(root, name)
                names.append(name)
    if len(names) > 1000:
        raise ValueError("reference package file limit exceeded")
    return names


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def build(root, facts_path, output):
    if output.exists() or (root / "manifest.json").exists():
        raise ValueError("preserve the existing package; choose a new directory")
    facts = json.loads(facts_path.read_text())
    if set(facts) != FACTS or facts["exit_code"] != 0:
        raise ValueError("actual successful workload facts required")
    if not re.fullmatch(r"[a-f0-9]{40}", facts["source_revision"]) or not re.fullmatch(r".+@sha256:[a-f0-9]{64}", facts["image"]):
        raise ValueError("immutable code and runtime references required")
    for name in ("inputs/observations.csv", "inputs/original.rds", "inputs/source.txt", "scripts/analysis.R", "scripts/package.py", "outputs/summary.csv", "outputs/data_checks.csv", "outputs/figure.pdf", "outputs/figure.svg", "outputs/environment.txt", "outputs/method.txt", "outputs/graphics.txt", "outputs/container_stdout.log"):
        safe_file(root, name)
    with (root / "outputs/data_checks.csv").open(newline="") as f:
        checks = list(csv.DictReader(f))
    if not checks or any(row["result"] != "passed" for row in checks):
        raise ValueError("input compatibility checks did not pass")
    write_json(root / "execution.json", facts)
    shutil.copyfile(Path(__file__).with_name("README.md"), root / "README.md")
    # These support files are used after collection, not by the R computation.
    for name in ("package.py", "review.html", "README.md"):
        source = Path(__file__).with_name(name)
        target = root / "scripts" / name
        if source.resolve() != target.resolve():
            shutil.copyfile(source, target)
    write_json(root / "packaging.json", {"builder_sha256": digest(Path(__file__)),
               "review_template_sha256": digest(Path(__file__).with_name("review.html")),
               "python_version": sys.version.split()[0], "execution_source_revision": facts["source_revision"],
               "support_files": ["scripts/package.py", "scripts/review.html", "scripts/README.md"],
               "role": "Post-execution collection and presentation; analysis.R and original data are unchanged"})
    names = inventory(root) + ["execution.json", "packaging.json", "README.md"]
    files = [{"path": name, "bytes": (root / name).stat().st_size, "sha256": digest(root / name),
              "role": "input" if name.startswith("inputs/") else "method" if name.startswith("scripts/") else "result" if name.startswith("outputs/") else "provenance"} for name in names]
    manifest = {"schema": "scientific-replay-reference/v1", "scientific_review": "pending", "demonstration": True,
                "numeric_rerun_tolerance": {"relative": 1e-12, "absolute": 1e-12}, "execution": facts, "files": files}
    table = []
    with (root / "outputs/summary.csv").open(newline="") as f:
        reader = csv.reader(f)
        for i, row in enumerate(reader):
            tag = "th" if i == 0 else "td"
            table.append("<tr>" + "".join(f"<{tag}>{html.escape(v)}</{tag}>" for v in row) + "</tr>")
    file_rows = "".join(f'<li><a href="{html.escape(x["path"], quote=True)}">{html.escape(x["path"])}</a><code>{x["sha256"]}</code></li>' for x in files)
    fact_rows = "".join(f"<dt>{html.escape(k.replace('_', ' '))}</dt><dd>{html.escape(str(v))}</dd>" for k, v in facts.items())
    page = Path(__file__).with_name("review.html").read_text()
    for token, value in {"{{SUMMARY_ROWS}}": "".join(table), "{{FILE_ROWS}}": file_rows, "{{FACT_ROWS}}": fact_rows,
                         "{{METHOD_CODE}}": html.escape((root / "scripts/analysis.R").read_text()),
                         "{{PACKAGE_URL}}": "../" + quote(output.name)}.items():
        page = page.replace(token, value)
    (root / "index.html").write_text(page, encoding="utf-8")
    entities = [{"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}, "conformsTo": {"@id": "https://w3id.org/ro/crate/1.1"}},
                {"@id": "./", "@type": "Dataset", "name": "Scientific replay reference", "description": "Public reference study; scientific review pending.", "hasPart": [{"@id": x["path"]} for x in files]},
                {"@id": "#analysis", "@type": "CreateAction", "name": "Descriptive R analysis", "instrument": {"@id": "scripts/analysis.R"}, "object": {"@id": "inputs/observations.csv"}, "result": [{"@id": x["path"]} for x in files if x["role"] == "result"], "startTime": facts["started_at"], "endTime": facts["finished_at"]}]
    entities.extend({"@id": x["path"], "@type": "File", "contentSize": x["bytes"], "sha256": x["sha256"]} for x in files)
    write_json(root / "ro-crate-metadata.json", {"@context": "https://w3id.org/ro/crate/1.1/context", "@graph": entities})
    for name in ("index.html", "ro-crate-metadata.json"):
        manifest["files"].append({"path": name, "bytes": (root / name).stat().st_size, "sha256": digest(root / name), "role": "provenance"})
    write_json(root / "manifest.json", manifest)
    checksums = [(x["sha256"], x["path"]) for x in manifest["files"]] + [(digest(root / "manifest.json"), "manifest.json")]
    (root / "manifest-sha256.txt").write_text("".join(f"{sha}  {name}\n" for sha, name in checksums))
    verify(root)
    with zipfile.ZipFile(output, "x", compression=zipfile.ZIP_DEFLATED) as z:
        for name in sorted([x["path"] for x in manifest["files"]] + ["manifest.json", "manifest-sha256.txt"]):
            z.write(root / name, name)
    print(json.dumps({"package": output.name, "bytes": output.stat().st_size, "sha256": digest(output), "scientific_review": "pending"}))


def verify(root):
    manifest = json.loads(safe_file(root, "manifest.json").read_text())
    if manifest.get("schema") != "scientific-replay-reference/v1":
        raise ValueError("unsupported manifest")
    records = manifest["files"]
    names = [x["path"] for x in records]
    if len(names) != len(set(names)) or len(names) > 1000:
        raise ValueError("duplicate or excessive manifest entries")
    for x in records:
        p = safe_file(root, x["path"])
        if p.stat().st_size != x["bytes"] or digest(p) != x["sha256"]:
            raise ValueError("package checksum mismatch: " + x["path"])
    if set(inventory(root)) != {x for x in names if x.split("/")[0] in ROOTS}:
        raise ValueError("unexpected file in original input, method or result directory")
    expected = "".join(f'{x["sha256"]}  {x["path"]}\n' for x in records) + f'{digest(root / "manifest.json")}  manifest.json\n'
    if safe_file(root, "manifest-sha256.txt").read_text() != expected:
        raise ValueError("manifest checksum list differs")
    print(json.dumps({"integrity": "passed", "files": len(records), "scientific_review": manifest["scientific_review"]}))


def compare(root, output):
    def read(path):
        with path.open(newline="") as f:
            reader = csv.DictReader(f)
            return reader.fieldnames, list(reader)
    fields, expected = read(root / "outputs/summary.csv")
    other_fields, actual = read(output / "summary.csv")
    if fields != other_fields or len(expected) != len(actual):
        raise ValueError("rerun summary shape differs")
    for a, b in zip(expected, actual):
        for key in fields:
            if key in ("supplement", "n") or not a[key]:
                same = a[key] == b[key]
            else:
                same = math.isclose(float(a[key]), float(b[key]), rel_tol=1e-12, abs_tol=1e-12)
            if not same:
                raise ValueError("rerun result differs")
    print(json.dumps({"numeric_rerun": "passed", "tolerance": "relative=absolute=1e-12", "scientific_review": "pending"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("build", "verify", "rerun", "compare"):
        p = sub.add_parser(command)
        p.add_argument("root", type=Path)
        if command == "build":
            p.add_argument("--runtime-facts", required=True, type=Path)
            p.add_argument("--output", required=True, type=Path)
        if command == "compare":
            p.add_argument("--rerun-output", required=True, type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    if args.command == "build":
        build(root, args.runtime_facts, args.output)
    else:
        verify(root)
        if args.command == "rerun":
            output = root / "rerun"
            if output.exists():
                raise ValueError("rerun directory already exists; retain it and choose a fresh extraction")
            subprocess.run(["Rscript", str(root / "scripts/analysis.R"), str(root / "inputs/observations.csv"), str(output)], check=True)
            compare(root, output)
        elif args.command == "compare":
            compare(root, args.rerun_output)


if __name__ == "__main__":
    main()
