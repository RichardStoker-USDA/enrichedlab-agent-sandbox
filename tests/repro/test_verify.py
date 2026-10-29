"""The rerun kit's verifier (src/broker/repro/verify.py) on synthetic packages.

A stub stands in for Rscript, so no R and no analysis code runs: it writes the tables a rerun would write, writes
changed ones, or fails. Run with: python3 -m unittest discover -s tests/repro
"""
import copy
import csv
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

VERIFY = Path(__file__).resolve().parents[2] / "src" / "broker" / "repro" / "verify.py"
RECIPE_SCHEMA = "sandbox.enrichedlab.org/reference-r-parameters/v1"
RENDER_TEMPLATE_SCHEMA = "enrichedlab.template-analysis-spec.v1"
TABLE = b"group,mean\nA,1.5\nB,2.25\n"
SOURCE_TABLE = b"nutrient_id,amount\n1003,1.5\n1008,2.25\n"
PARAMS = b'{"group":"nutrient_id","value":"amount"}\n'
RENDER_RECORD = {"columns": ["group", "estimate"],
                 "rows": [{"group": "A", "estimate": 1.5}, {"group": "B", "estimate": 2.25}],
                 "method": "Synthetic reviewed method"}
R_SESSION = {
    "r_version": "R version 4.4.3",
    "platform": "x86_64-pc-linux-gnu",
    "running": "Ubuntu 24.04",
    "locale": "C.UTF-8",
    "blas": "libblas.so",
    "lapack": "liblapack.so",
    "lapack_version": "3.12.0",
    "base_packages": ["stats", "graphics", "base"],
    "attached": ["ggplot2"],
    "packages": {"stats": "4.4.3", "jsonlite": "1.9.1", "ggplot2": "3.5.2"},
}

STUB = textwrap.dedent('''
    import io, json, os, sys, tarfile
    from pathlib import Path
    mode = os.environ["STUB_MODE"]
    args = sys.argv[1:]
    if mode == "fail":
        sys.exit(7)
    if "-e" in args:
        print(json.dumps({
            "r_version": os.environ.get("STUB_R_VERSION", "R version 4.4.3"),
            "packages": json.loads(os.environ.get(
                "STUB_R_PACKAGES", '{"stats":"4.4.3","jsonlite":"1.9.1","ggplot2":"3.5.2"}')),
        }, sort_keys=True))
        sys.exit(0)
    script_at = 1 if args and args[0] == "--vanilla" else 0
    script = args[script_at]
    render = script.startswith("code/") and script != "code/template.R" and script != "code/bootstrap_replay.R"
    startup_names = ("R_PROFILE", "R_PROFILE_USER", "R_ENVIRON", "R_ENVIRON_USER", "R_LIBS_USER",
                     "R_DEFAULT_PACKAGES", "R_SCRIPT_DEFAULT_PACKAGES", "R_TESTS")
    unsafe_startup = render and ((args[0] != "--vanilla" and Path(".Rprofile").is_file())
                                 or any(os.environ.get(name) for name in startup_names))
    if unsafe_startup:
        marker = os.environ.get("STARTUP_MARKER")
        if marker:
            Path(marker).write_text("startup code ran")
        mode = "differ"
    table = b"group,mean\\nA,1.5\\nB,9.75\\n" if mode == "differ" else b"group,mean\\nA,1.5\\nB,2.25\\n"
    if render:
        out = Path(args[script_at + 1])
        assert (out / "data.csv").is_file()
        assert (out / "params.json").is_file()
        if mode == "render-missing":
            sys.exit(0)
        record = {"columns": ["group", "estimate"],
                  "rows": [{"group": "A", "estimate": 1.5},
                           {"group": "B", "estimate": 2.25 if mode == "match" else 9.75}],
                  "method": "Synthetic reviewed method"}
        if mode == "overwrite-retained":
            record["rows"][1]["estimate"] = 8.5
            Path("results/record.json").write_text(json.dumps(record))
        if mode == "symlink-output":
            (out / "results_exact.json").symlink_to(Path.cwd() / "results" / "record.json")
        else:
            (out / "results_exact.json").write_text(json.dumps(record))
        (out / "results.json").write_text(json.dumps(record))
    elif args[1] == "code/template.R":
        out = Path(args[4]) / "results"
        out.mkdir(parents=True)
        (out / "means.csv").write_bytes(table)
    else:
        with tarfile.open(args[3], "w:gz") as tar:
            for name, data in (("results/bootstrap_intervals.csv", table), ("exit_code.txt", b"0\\n")):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
''')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def archive(files, listed=None):
    """A package archive with its own SHA256SUMS, as the templates and the recipe write them."""
    files = dict(files)
    manifest = files if listed is None else {name: data for name, data in files.items() if name in listed}
    files["SHA256SUMS"] = "".join("{}  {}\n".format(digest(data), name)
                                    for name, data in sorted(manifest.items())).encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for name, data in sorted(files.items()):
            info = tarfile.TarInfo("./" + name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def archive_with_symlink(files, name, target, expected):
    files = dict(files)
    manifest = dict(files)
    manifest[name] = expected
    files["SHA256SUMS"] = "".join("{}  {}\n".format(digest(data) if isinstance(data, bytes) else data, path)
                                    for path, data in sorted(manifest.items())).encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for path, data in sorted(files.items()):
            info = tarfile.TarInfo("./" + path)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        link = tarfile.TarInfo("./" + name)
        link.type = tarfile.SYMTYPE
        link.linkname = str(target)
        tar.addfile(link)
    return buffer.getvalue()


def template(seed=7, tables=True, dependence=None, source_table=False):
    run = {"template_ref": "template://r-generated-analysis/v2", "seed": seed,
           "tables": [{"name": "means", "file": "results/means.csv", "rows": 2, "columns": 2}] if tables else []}
    if dependence:
        run["input_dependence"] = {"outcome": dependence}
    return {"run.json": json.dumps(run).encode(), "code/template.R": b"# template\n", "code/block.R": b"# block\n",
            "input/data.csv": b"x\n1\n", **({"results/means.csv": TABLE} if tables else {}),
            **({"results/source.csv": SOURCE_TABLE} if source_table else {})}


def recipe():
    params = {"schemaVersion": RECIPE_SCHEMA, "statistic": "mean", "seed": 11, "resamplesPerGroup": 100000}
    return {"run_parameters.json": json.dumps(params).encode(), "code/bootstrap_replay.R": b"# recipe\n",
            "input/source.zip": b"PK synthetic", "results/bootstrap_intervals.csv": TABLE, "exit_code.txt": b"0\n",
            "README.md": b"# Scientific replay\n"}


def other():
    return {"README.md": b"# An older package\n", "results/summary.csv": TABLE}


def task_id(number):
    return "{:08d}-0000-4000-8000-000000000000".format(number)


def render_template(table_sha=None, source_task=None, template_bytes=b"# reviewed template\n", template_sha=None):
    common = b"# shared code\n"
    record = json.dumps(RENDER_RECORD, sort_keys=True).encode()
    template_sha = template_sha or digest(template_bytes)
    spec = {
        "schema": RENDER_TEMPLATE_SCHEMA,
        "feature": "group-summary",
        "method": "Synthetic reviewed method",
        "service": {"version": "0.7.1", "image_digest": "sha256:" + "a" * 64,
                    "r_session": R_SESSION},
        "settings": {"group": "nutrient_id", "value": "amount"},
        "settings_used": {"group": "nutrient_id", "value": "amount", "na_rm": True},
        "code": {
            "template": {"path": "code/group-summary.R", "sha256": template_sha},
            "shared": {"path": "code/_common.R", "sha256": digest(common)},
            "settings": {"path": "code/params.json", "sha256": digest(PARAMS)},
        },
        "source": {"task_id": source_task or task_id(1), "table_path": "results/source.csv",
                   "table_sha256": table_sha or digest(SOURCE_TABLE), "rows": 2,
                   "package_sha256": "b" * 64},
        "files": [{"path": "results/record.json", "role": "record", "sha256": digest(record)}],
    }
    return {"spec.json": json.dumps(spec).encode(), "code/group-summary.R": template_bytes,
            "code/_common.R": common, "code/params.json": PARAMS, "results/record.json": record,
            "README.md": b"# Reviewed template analysis\n"}


class Verifier(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="verify-codetest-"))
        self.addCleanup(shutil.rmtree, self.root)
        stub = self.root.parent / (self.root.name + "-rscript.py")
        stub.write_text(STUB, encoding="utf-8")
        self.addCleanup(stub.unlink)
        self.rscript = "{} {}".format(sys.executable, stub)

    def package(self, packages, extra=None):
        (self.root / "rerun").mkdir()
        shutil.copy(VERIFY, self.root / "rerun" / "verify.py")
        entries, listed = [], {}
        for n, files in enumerate(packages, 1):
            path = "tasks/{:08d}-0000-4000-8000-000000000000/files/payload.tar.gz".format(n)
            data = files if isinstance(files, bytes) else archive(files)
            (self.root / path).parent.mkdir(parents=True)
            (self.root / path).write_bytes(data)
            entries.append(dict({"task_id": "{:08d}-0000-4000-8000-000000000000".format(n), "title": "Analysis {}".format(n),
                                 "package": path, "package_sha256": digest(data)}, **((extra or {}).get(n) or {})))
            listed[path] = data
        listed["rerun/analyses.json"] = json.dumps({"analyses": entries}).encode()
        (self.root / "rerun" / "analyses.json").write_bytes(listed["rerun/analyses.json"])
        listed["rerun/verify.py"] = VERIFY.read_bytes()
        (self.root / "SHA256SUMS").write_text("".join("{}  {}\n".format(digest(d), n) for n, d in sorted(listed.items())))

    def rewrite_outer_sums(self):
        catalog = json.loads((self.root / "rerun" / "analyses.json").read_text())
        entries = [(analysis["package_sha256"], analysis["package"]) for analysis in catalog["analyses"]]
        for name in ("rerun/analyses.json", "rerun/verify.py"):
            entries.append((digest((self.root / name).read_bytes()), name))
        (self.root / "SHA256SUMS").write_text("".join("{}  {}\n".format(value, name)
                                                       for value, name in sorted(entries)))

    def verify(self, *args, mode="match", environment=None):
        child_env = dict(os.environ, STUB_MODE=mode)
        child_env.update(environment or {})
        result = subprocess.run([sys.executable, str(self.root / "rerun" / "verify.py"), *args], cwd=self.root,
                                env=child_env, capture_output=True, text=True, timeout=120)
        report = self.root / "extracted" / "rerun_report.json"
        return result.returncode, json.loads(report.read_text()) if report.is_file() else None, result.stdout

    def test_template_and_recipe_are_rerun_and_compared(self):
        self.package([template(), recipe()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 0, out)
        self.assertEqual(report["counts"], {"analyses": 2, "matched": 2, "failed_or_different": 0, "not_rerun": 0})
        self.assertEqual([a["format"] for a in report["analyses"]], ["template", "recipe"])
        self.assertTrue(all(a["rerun"] == "matches" for a in report["analyses"]))
        self.assertNotIn("2.25", out, "the verifier prints outcomes, never values")

    def test_render_template_is_staged_rerun_and_compared_as_json(self):
        self.package([template(source_table=True), render_template()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 0, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["format"], "render-template")
        self.assertEqual(rendered["rerun"], "matches")
        self.assertEqual(rendered["record"], {
            "retained": "results/record.json", "rerun": "rerun/results_exact.json", "outcome": "identical",
            "numbers_compared": 2, "numbers_different": 0, "largest_relative_difference": 0.0})
        folder = self.root / "extracted" / "02-analysis-2" / "rerun"
        self.assertEqual((folder / "data.csv").read_bytes(), SOURCE_TABLE)
        self.assertEqual((folder / "params.json").read_bytes(), PARAMS)
        self.assertNotIn("2.25", out, "the verifier prints outcomes, never values")

    def test_render_template_ignores_analysis_and_environment_startup_files(self):
        rendered = render_template()
        rendered[".Rprofile"] = b"# would change the result and write a marker\n"
        self.package([template(source_table=True), rendered])
        profile = self.root.parent / (self.root.name + "-R_PROFILE_USER")
        marker = self.root.parent / (self.root.name + "-startup-marker")
        profile.write_text("# would change the result and write a marker\n")
        self.addCleanup(lambda: profile.unlink(missing_ok=True))
        self.addCleanup(lambda: marker.unlink(missing_ok=True))
        environment = {name: str(profile) for name in
                       ("R_PROFILE", "R_PROFILE_USER", "R_ENVIRON", "R_ENVIRON_USER", "R_LIBS_USER", "R_TESTS")}
        # A default package list would attach a package from a library path before the template runs.
        environment.update(R_DEFAULT_PACKAGES="startupmarker", R_SCRIPT_DEFAULT_PACKAGES="startupmarker",
                           R_LIBS=str(self.root.parent))
        environment["STARTUP_MARKER"] = str(marker)
        code, report, out = self.verify("--rerun", "--rscript", self.rscript, environment=environment)
        self.assertEqual(code, 0, out)
        self.assertEqual(report["analyses"][1]["rerun"], "matches")
        self.assertFalse(marker.exists(), "R startup code ran")

    def test_render_template_number_beyond_tolerance_differs(self):
        self.package([template(source_table=True), render_template()])
        code, report, _ = self.verify("--rerun", "--rscript", self.rscript, mode="differ")
        self.assertEqual(code, 2)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "differs")
        self.assertEqual(rendered["record"]["numbers_compared"], 2)
        self.assertEqual(rendered["record"]["numbers_different"], 1)
        self.assertGreater(rendered["record"]["largest_relative_difference"], 0)

    def test_render_template_missing_results_exact_fails(self):
        self.package([template(source_table=True), render_template()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript, mode="render-missing")
        self.assertEqual(code, 2, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "failed")
        self.assertEqual(rendered["record"]["outcome"], "results_exact.json missing or unreadable")
        self.assertEqual(rendered["record"]["numbers_compared"], 0)

    def test_render_template_cannot_replace_the_retained_record_and_self_match(self):
        self.package([template(source_table=True), render_template()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript, mode="overwrite-retained")
        self.assertEqual(code, 2, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "failed")
        self.assertEqual(rendered["record"]["outcome"], "refused: retained record changed during rerun")

    def test_render_template_cannot_link_its_output_to_the_retained_record(self):
        self.package([template(source_table=True), render_template()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript, mode="symlink-output")
        self.assertEqual(code, 2, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "failed")
        self.assertTrue(rendered["record"]["outcome"].startswith("refused:"))

    def test_render_template_with_changed_reviewed_code_is_not_run(self):
        original = b"# reviewed template\n"
        changed = b"# changed reviewed template\n"
        self.package([template(source_table=True),
                      render_template(template_bytes=changed, template_sha=digest(original))])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["format"], "render-template")
        self.assertIn("template code file is missing or its SHA-256 differs", rendered["rerun"])
        self.assertFalse((self.root / "extracted" / "02-analysis-2" / "rerun").exists())

    def test_render_template_with_changed_source_table_is_not_run(self):
        self.package([template(source_table=True), render_template(table_sha="c" * 64)])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        rendered = report["analyses"][1]
        self.assertIn("source table's SHA-256 differs", rendered["rerun"])
        self.assertFalse((self.root / "extracted" / "02-analysis-2" / "rerun").exists())

    def test_absolute_analysis_package_path_is_refused(self):
        self.package([template()])
        catalog = self.root / "rerun" / "analyses.json"
        data = json.loads(catalog.read_text())
        relative = data["analyses"][0]["package"]
        data["analyses"][0]["package"] = str((self.root / relative).resolve())
        catalog.write_text(json.dumps(data))
        self.rewrite_outer_sums()
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 1, out)
        self.assertEqual(report["analyses"][0]["package"], "missing or changed")

    def test_parent_traversal_analysis_package_path_is_refused(self):
        self.package([template()])
        catalog = self.root / "rerun" / "analyses.json"
        data = json.loads(catalog.read_text())
        relative = data["analyses"][0]["package"]
        data["analyses"][0]["package"] = "tasks/../" + relative
        catalog.write_text(json.dumps(data))
        self.rewrite_outer_sums()
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 1, out)
        self.assertEqual(report["analyses"][0]["package"], "missing or changed")

    def test_analysis_package_must_be_listed_in_the_outer_manifest(self):
        self.package([template()])
        entries = []
        for name in ("rerun/analyses.json", "rerun/verify.py"):
            entries.append((digest((self.root / name).read_bytes()), name))
        (self.root / "SHA256SUMS").write_text("".join("{}  {}\n".format(value, name)
                                                       for value, name in entries))
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 1, out)
        self.assertEqual(report["analyses"][0]["package"], "missing or changed")

    def test_analysis_package_symlink_to_an_external_archive_is_refused(self):
        self.package([template()])
        catalog = self.root / "rerun" / "analyses.json"
        data = json.loads(catalog.read_text())
        original = self.root / data["analyses"][0]["package"]
        external = self.root.parent / (self.root.name + "-external-package.tar.gz")
        external.write_bytes(original.read_bytes())
        self.addCleanup(lambda: external.unlink(missing_ok=True))
        linked = self.root / "linked" / "payload.tar.gz"
        linked.parent.mkdir()
        linked.symlink_to(external)
        data["analyses"][0]["package"] = "linked/payload.tar.gz"
        catalog.write_text(json.dumps(data))
        self.rewrite_outer_sums()
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 1, out)
        self.assertEqual(report["analyses"][0]["package"], "missing or changed")

    def test_symlinked_source_file_is_refused(self):
        external = self.root.parent / (self.root.name + "-external-source.csv")
        external.write_bytes(SOURCE_TABLE)
        self.addCleanup(lambda: external.unlink(missing_ok=True))
        source = archive_with_symlink(template(), "results/source.csv", external, digest(SOURCE_TABLE))
        self.package([source, render_template()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 1, out)
        self.assertEqual(report["analyses"][0]["package"], "missing or changed")
        self.assertIn("source analysis package was not unpacked", report["analyses"][1]["rerun"])

    def test_render_source_and_code_must_be_listed_in_their_package_manifests(self):
        source_files = template(source_table=True)
        source = archive(source_files, listed=set(source_files) - {"results/source.csv"})
        render_files = render_template()
        render = archive(render_files, listed=set(render_files) - {"code/_common.R"})
        self.package([source, render])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        self.assertIn("shared code file is not listed", report["analyses"][1]["rerun"])

        shutil.rmtree(self.root / "extracted")
        self.root.joinpath("tasks", task_id(2), "files", "payload.tar.gz").write_bytes(
            archive(render_template()))
        catalog = self.root / "rerun" / "analyses.json"
        data = json.loads(catalog.read_text())
        payload = self.root / data["analyses"][1]["package"]
        data["analyses"][1]["package_sha256"] = digest(payload.read_bytes())
        catalog.write_text(json.dumps(data))
        self.rewrite_outer_sums()
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        self.assertIn("source table is not listed", report["analyses"][1]["rerun"])

    def test_render_template_source_analysis_must_be_in_the_package(self):
        self.package([render_template(source_task=task_id(99))])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        self.assertIn("source analysis is absent", report["analyses"][0]["rerun"])

    def test_formats_can_exclude_render_templates_without_failing_them(self):
        self.package([template(source_table=True), render_template()])
        code, report, out = self.verify("--rerun", "--formats", "template,recipe", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "not rerun: format render-template was excluded by --formats")
        self.assertEqual(report["counts"], {"analyses": 2, "matched": 1, "failed_or_different": 0, "not_rerun": 1})

    def test_reviewed_only_reruns_render_service_template_code(self):
        self.package([template(source_table=True), render_template()], extra={1: {"settings": {"configured": True}}})
        code, report, out = self.verify("--rerun", "--reviewed-only", "--rscript", self.rscript)
        self.assertEqual(code, 0, out)
        self.assertEqual(report["analyses"][1]["rerun"], "matches")

    def test_render_template_method_sheet_records_how_it_ran(self):
        package = render_template()
        spec = json.loads(package["spec.json"])
        self.package([template(source_table=True), package])
        code, _, out = self.verify()
        self.assertEqual(code, 0, out)
        sheet = (self.root / "extracted" / "02-analysis-2" / "METHOD.md").read_text()
        for expected in [
                "- Feature: `group-summary`",
                "- Method label: `Synthetic reviewed method`",
                "- Service version: `0.7.1`",
                "- Service image digest: `sha256:{}".format("a" * 64) + "`",
                "- R version: `R version 4.4.3`",
                "- Platform: `x86_64-pc-linux-gnu`",
                "- OS: `Ubuntu 24.04`",
                "- R packages:",
                "  - `ggplot2 3.5.2`",
                "  - `jsonlite 1.9.1`",
                "  - `stats 4.4.3`",
                '- Settings used by the template: `{"group":"nutrient_id","na_rm":true,"value":"amount"}`',
                "- Source analysis: `{}`".format(task_id(1)),
                "- Source table: `results/source.csv`, 2 rows, SHA-256 `{}`".format(digest(SOURCE_TABLE)),
                "- Template code: `code/group-summary.R`, SHA-256 `{}`".format(
                    spec["code"]["template"]["sha256"]),
                "- Shared code: `code/_common.R`, SHA-256 `{}`".format(spec["code"]["shared"]["sha256"]),
                "- Settings file: `code/params.json`, SHA-256 `{}`".format(
                    spec["code"]["settings"]["sha256"]),
                "- Rerun command: `Rscript --vanilla code/group-summary.R rerun`",
        ]:
            self.assertIn(expected, sheet)

    def test_render_r_session_with_empty_values_is_shown_without_them(self):
        # Render-service 0.7.1 writes ten fields and leaves the OS, BLAS, LAPACK and its version empty when R reports
        # none; the sheet keeps what was recorded and leaves out the empty lines.
        package = render_template()
        spec = json.loads(package["spec.json"])
        for key in ("running", "blas", "lapack", "lapack_version"):
            spec["service"]["r_session"][key] = ""
        package["spec.json"] = json.dumps(spec).encode()
        self.package([template(source_table=True), package])
        code, _, out = self.verify()
        self.assertEqual(code, 0, out)
        sheet = (self.root / "extracted" / "02-analysis-2" / "METHOD.md").read_text()
        self.assertIn("- R version: `R version 4.4.3`", sheet)
        self.assertIn("- Platform: `x86_64-pc-linux-gnu`", sheet)
        self.assertNotIn("- OS:", sheet)
        self.assertNotIn("- R environment: not recorded", sheet)

    def test_recorded_r_session_uses_the_render_services_rules(self):
        loader = importlib.util.spec_from_file_location("package_verify", VERIFY)
        module = importlib.util.module_from_spec(loader)
        loader.loader.exec_module(module)

        def run_with(change):
            session = copy.deepcopy(R_SESSION)
            change(session)
            return {"service": {"r_session": session}}

        accepted = [
            lambda s: None,
            lambda s: s.update(running="", blas="", lapack="", lapack_version=""),
            lambda s: s.update(locale="x" * 400),
        ]
        refused = [
            lambda s: s.pop("lapack_version"),
            lambda s: s.update(hostname="node1"),
            lambda s: s.update(lapack_version="3.12 beta"),
            lambda s: s.update(lapack=".liblapack.so"),
            lambda s: s.update(blas="/usr/lib/libblas.so"),
            lambda s: s.update(locale="x" * 401),
            lambda s: s.update(running="Linux\nforged"),
            lambda s: s.update(packages={}),
        ]
        for index, change in enumerate(accepted):
            with self.subTest(accepted=index):
                self.assertIsNotNone(module.recorded_r_session(run_with(change)))
        for index, change in enumerate(refused):
            with self.subTest(refused=index):
                self.assertIsNone(module.recorded_r_session(run_with(change)))

    def test_render_rerun_reports_a_local_r_version_difference_without_failing(self):
        self.package([template(source_table=True), render_template()])
        code, report, out = self.verify(
            "--rerun", "--rscript", self.rscript,
            environment={"STUB_R_VERSION": "R version 4.5.0"})
        self.assertEqual(code, 0, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "matches")
        environment = rendered["r_environment"]
        self.assertEqual(environment["recorded"]["r_version"], "R version 4.4.3")
        self.assertEqual(environment["local"]["r_version"], "R version 4.5.0")
        self.assertEqual(
            environment["local"]["packages"],
            {"ggplot2": "3.5.2", "jsonlite": "1.9.1", "stats": "4.4.3"})
        self.assertEqual(
            environment["differences"],
            ["R version: recorded 'R version 4.4.3', local 'R version 4.5.0'"])
        self.assertIn("R environment information: R version", out)

    def test_invalid_render_r_session_is_not_shown_or_compared(self):
        package = render_template()
        spec = json.loads(package["spec.json"])
        spec["service"]["r_session"]["blas"] = "/forged/libblas.so"
        package["spec.json"] = json.dumps(spec).encode()
        self.package([template(source_table=True), package])

        code, report, out = self.verify("--rerun", "--rscript", self.rscript)

        self.assertEqual(code, 0, out)
        rendered = report["analyses"][1]
        self.assertEqual(rendered["rerun"], "matches")
        self.assertNotIn("r_environment", rendered)
        sheet = (self.root / "extracted" / "02-analysis-2" / "METHOD.md").read_text()
        self.assertIn("- R environment: not recorded", sheet)
        self.assertNotIn("/forged/libblas.so", sheet)
        self.assertNotIn("/forged/libblas.so", out)

    def test_tolerance_must_be_finite_and_non_negative(self):
        self.package([template()])
        for value in ("nan", "inf", "-1"):
            with self.subTest(value=value):
                result = subprocess.run(
                    [sys.executable, str(self.root / "rerun" / "verify.py"), "--tolerance", value],
                    cwd=self.root, capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, 2)
                self.assertIn("tolerance must be a finite, non-negative number", result.stderr)

    def test_a_failed_run_fails_even_with_no_tables(self):
        # The audit's case: a template run that exits 7 and lists no tables used to leave status 0.
        self.package([template(tables=False)])
        code, report, _ = self.verify("--rerun", "--rscript", self.rscript, mode="fail")
        self.assertEqual(code, 2)
        self.assertEqual(report["analyses"][0]["rerun"], "failed")
        self.assertEqual(report["analyses"][0]["exit_code"], 7)

    def test_a_format_it_cannot_replay_is_counted_not_passed(self):
        self.package([template(), other()])
        code, report, out = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        self.assertEqual(report["counts"]["not_rerun"], 1)
        self.assertTrue(report["analyses"][1]["rerun"].startswith("not rerun"))
        self.assertIn("1 of 2 analyses rerun with matching tables", out)

    def test_a_changed_table_is_a_difference(self):
        self.package([template(), recipe()])
        code, report, _ = self.verify("--rerun", "--rscript", self.rscript, mode="differ")
        self.assertEqual(code, 2)
        self.assertEqual(report["counts"]["failed_or_different"], 2)
        self.assertEqual({t["outcome"] for a in report["analyses"] for t in a["tables"]}, {"different"})

    def test_a_damaged_package_outranks_a_rerun_result(self):
        self.package([template(), recipe()])
        target = self.root / "tasks/00000002-0000-4000-8000-000000000000/files/payload.tar.gz"
        target.write_bytes(target.read_bytes() + b"x")
        code, report, _ = self.verify("--rerun", "--rscript", self.rscript, mode="differ")
        self.assertEqual(code, 1)
        self.assertEqual(report["analyses"][1]["package"], "missing or changed")

    def test_recipe_tables_reach_the_catalog_and_imports(self):
        self.package([recipe(), other()])
        code, report, _ = self.verify()
        self.assertEqual(code, 0)
        self.assertIsNone(report)
        with open(self.root / "extracted" / "tables.csv", newline="") as f:
            rows = list(csv.reader(f))
        self.assertEqual([r[1] for r in rows[1:]], ["bootstrap_intervals", "summary"])
        self.assertEqual([r[3:5] for r in rows[1:]], [["2", "2"], ["2", "2"]])
        self.assertEqual((self.root / "extracted" / "import_all.sas").read_text().count("proc import"), 2)
        sheet = (self.root / "extracted" / "01-analysis-1" / "METHOD.md").read_text()
        self.assertIn("code/bootstrap_replay.R input/source.zip rerun.tar.gz mean 11", sheet)

    # On an HPC the job reruns code the agent wrote only after a named person accepted it; a configured
    # method's block (rendered from its settings) and a recipe need no acceptance.
    def test_reviewed_only_skips_agent_code_no_person_accepted(self):
        accepted = {"decision": "accepted", "reviewer_kind": "person", "recorded_at": "2026-10-04T08:00:00Z"}
        later_change = {"decision": "needs_changes", "reviewer_kind": "person", "recorded_at": "2026-10-04T09:00:00Z"}
        service = {"decision": "accepted", "reviewer_kind": "service_account", "recorded_at": "2026-10-04T09:00:00Z"}
        self.package([template(), template(), template(), template(), template(), recipe()], extra={
            2: {"reviews": [accepted]}, 3: {"reviews": [accepted, later_change]}, 4: {"reviews": [service]},
            5: {"settings": {"statistic": "mean"}}})
        code, report, out = self.verify("--rerun", "--reviewed-only", "--rscript", self.rscript)
        self.assertEqual(code, 3, out)
        outcomes = [a["rerun"] for a in report["analyses"]]
        skipped = "not rerun: the agent wrote its code and no person has accepted it"
        self.assertEqual(outcomes, [skipped, "matches", skipped, skipped, "matches", "matches"])
        self.assertIn("no person on the project has accepted it", out)
        # Without the option every analysis reruns, as on a scientist's own computer.
        shutil.rmtree(self.root / "extracted")
        code, report, _ = self.verify("--rerun", "--rscript", self.rscript)
        self.assertEqual(code, 0)

    # The review records decide what runs, so a package whose checksums fail runs nothing under
    # --reviewed-only; here an acceptance was added to the catalog after export.
    def test_a_package_with_a_bad_checksum_runs_no_code_when_reviewed_only(self):
        self.package([template(), recipe()])
        catalog = self.root / "rerun" / "analyses.json"
        data = json.loads(catalog.read_text())
        data["analyses"][0]["reviews"] = [{"decision": "accepted", "reviewer_kind": "person",
                                           "recorded_at": "2026-10-04T08:00:00Z"}]
        catalog.write_text(json.dumps(data))
        code, report, out = self.verify("--rerun", "--reviewed-only", "--rscript", self.rscript)
        self.assertEqual(code, 1, out)
        self.assertEqual({a["rerun"] for a in report["analyses"]},
                         {"not rerun: the package's checksums do not match, so its review records cannot be trusted"})
        # No analysis got a rerun folder or log, so R never started.
        self.assertFalse([p for p in (self.root / "extracted").glob("*/rerun*")])

    # Two jobs from one package never share an output folder.
    def test_each_run_can_have_a_folder_of_its_own(self):
        self.package([template()])
        code, report, out = self.verify("--rerun", "--work", "extracted-41", "--rscript", self.rscript)
        self.assertEqual(code, 0, out)
        self.assertTrue((self.root / "extracted-41" / "rerun_report.json").is_file())
        self.assertFalse((self.root / "extracted").exists())
        again = subprocess.run([sys.executable, str(self.root / "rerun" / "verify.py"), "--work", "extracted-41"],
                               cwd=self.root, capture_output=True, text=True, timeout=60)
        self.assertNotEqual(again.returncode, 0)
        self.assertIn("already exists", again.stderr)
        bad = subprocess.run([sys.executable, str(self.root / "rerun" / "verify.py"), "--work", "../outside"],
                             cwd=self.root, capture_output=True, text=True, timeout=60)
        self.assertNotEqual(bad.returncode, 0)

    def test_a_folder_with_spaces_stays_one_argument(self):
        spaced = Path(tempfile.mkdtemp(prefix="verify codetest "))
        self.addCleanup(shutil.rmtree, spaced)
        self.root = spaced / "package folder"
        self.root.mkdir()
        stub = spaced / "r script.py"
        stub.write_text(STUB, encoding="utf-8")
        self.package([template()])
        # env receives the analysis folder as one argument only when the template is split before {dir} is filled in.
        code, report, out = self.verify("--rerun", "--rscript", "env RERUN_DIR={{dir}} {} '{}'".format(sys.executable, stub))
        self.assertEqual(code, 0, out)
        self.assertEqual(report["analyses"][0]["rerun"], "matches")

    def test_the_method_sheet_reports_the_templates_own_record(self):
        self.package([template(dependence="not_checked")])
        self.verify()
        sheet = (self.root / "extracted" / "01-analysis-1" / "METHOD.md").read_text()
        self.assertIn("Input-dependence check, as the template recorded it: `not_checked`", sheet)
        self.assertNotIn("the tables changed when the input changed", sheet)


if __name__ == "__main__":
    unittest.main()
