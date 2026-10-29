#!/usr/bin/env python3
"""Run every setting combination of a configured recipe through the real template.

Usage: check_configured_recipe.py <recipe dir> <template.R> <contract.dcf> <dataset csv>

The template belongs to the site repository, so this is a maintainer check, not a unit test. It renders each
combination the way the coordinator does, runs `template.R` as a local rerun (isolation recorded, not enforced) and
requires status `succeeded` and a passed input-dependence rerun. It prints counts only.
"""
import itertools, json, os, pathlib, shutil, subprocess, sys, tempfile

recipe, template, contract, dataset = (pathlib.Path(a) for a in sys.argv[1:5])
block = (recipe / "block.R").read_text()
settings = json.loads((recipe / "configuration.json").read_text())["settings"]
keys = sorted(settings)
failures = []
combinations = list(itertools.product(*(sorted(settings[k]["choices"]) for k in keys)))
with tempfile.TemporaryDirectory(prefix="configured-recipe-") as root:
    work = pathlib.Path(root) / "work"
    (work / "data").mkdir(parents=True)
    shutil.copy(contract, work / "contract.dcf")
    shutil.copy(dataset, work / "data" / pathlib.Path(dataset).name)
    for n, choice in enumerate(combinations):
        rendered = block
        for key, value in zip(keys, choice):
            for slot, text in settings[key]["choices"][value]["slots"].items():
                rendered = rendered.replace("{{" + slot + "}}", text)
        assert "{{" not in rendered
        case = pathlib.Path(root) / f"case{n}"
        (case / "out").mkdir(parents=True)
        (case / "block.R").write_text(rendered)
        env = dict(os.environ, ENRICHEDLAB_TEMPLATE_RERUN="local")
        subprocess.run(["Rscript", "--vanilla", str(template), str(work), str(case / "block.R"), str(case / "out"), "20260925"],
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=600)
        run = json.loads((case / "out" / "run.json").read_text())
        dependence = (run.get("input_dependence") or {}).get("outcome")
        if run.get("status") != "succeeded" or dependence != "passed":
            failures.append({"settings": dict(zip(keys, choice)), "status": run.get("status"), "error": run.get("error"),
                             "input_dependence": dependence})
print(json.dumps({"combinations": len(combinations), "failed": len(failures), "failures": failures[:5]}, indent=1))
sys.exit(1 if failures else 0)
