# Rerun kit

Every project evidence package carries `rerun/`, so a scientist can check every file, read what each analysis did, open
its tables in SAS or JMP, and run each analysis again and compare its results with the retained ones, on a laptop or an
HPC.

The kit adds no code to the analysis packages: each retained package (`tasks/<analysis>/files/payload.tar.gz`) already
holds its code, input or source-table record, results, run record and its own README. The kit does not change them.

## Files

| Path | Built from | Content |
|---|---|---|
| `rerun/README.md` | the snapshot and `src/broker/repro/hpc_sites.json` | how to check, unpack and rerun, the step-by-step hand-off for SCINet Atlas and Ceres, and the analyses in the order the verifier numbers them |
| `rerun/analyses.json` | the snapshot | each retained analysis package with its path and SHA-256, plan-step title, method, objective, status, configured settings and people's reviews; the tested R image |
| `rerun/verify.py` | `src/broker/repro/verify.py`, the same bytes in every package | the verifier below |
| `rerun/slurm/rerun.sbatch` | `src/broker/repro/rerun.sbatch` | the template and recipe rerun on a Slurm cluster with Apptainer and `--reviewed-only --formats template,recipe`: it pulls the tested R image by its digest when missing (into a file of the job's own, then renamed into place, so two first runs never meet a half-written image), keeps Apptainer's cache in the job's scratch, quotes the image path for the verifier, and stops with what to do when Apptainer or Python 3.8 is missing |

All four are listed in `SHA256SUMS` and the manifest; `counts.rerun_analyses` says how many packages the kit covers.

## The verifier

`python3 rerun/verify.py`, run from the unpacked package folder, needs Python 3.8 or later and nothing else:

1. Checks every line of the package's `SHA256SUMS`.
2. Unpacks each analysis package listed in `rerun/analyses.json`, after checking its SHA-256, into
   `extracted/<nn>-<title>/`. Each package must have a safe relative path listed in the outer `SHA256SUMS`. The verifier
   opens it without following links. Only regular files and folders are written; a link, device, traversal, symlinked
   path component or path outside the folder stops the run.
3. Checks the analysis package's own `SHA256SUMS`.
4. Writes `METHOD.md` per analysis (method, settings, runtime and code provenance, input provenance, results with
   checksums, people's reviews, and the rerun command), plus
   `extracted/tables.csv`, `extracted/import_all.sas` (one `PROC IMPORT` per result table into `WORK`) and
   `extracted/open_all.jsl` (one `Open` per table).

With `--rerun` it also reruns three package formats, each into a new folder (`rerun`, then `rerun-2`, …) so nothing
retained is overwritten, with R's output in `<folder>.log`:

- An analysis written by a reviewed template, whose `run.json` names the template and seed:
  `ENRICHEDLAB_TEMPLATE_RERUN=local Rscript --vanilla code/template.R input code/block.R <folder> <seed>`. Its tables are
  the ones `run.json` lists.
- The reviewed recipe that replays itself, whose `run_parameters.json` (`sandbox.enrichedlab.org/reference-r-parameters/v1`)
  names the statistic and seed: `Rscript --vanilla code/bootstrap_replay.R input/source.zip <folder>.tar.gz <statistic>
  <seed>`, whose archive is unpacked into the folder like any package. Its tables are the CSV files under `results/`
  that its own `SHA256SUMS` names; they also reach `tables.csv` and the SAS and JMP imports.
- A render-service template analysis, detected only by `spec.json` schema
  `enrichedlab.template-analysis-spec.v1`. Before running, the verifier checks the SHA-256 in each of the spec's three
  code records, finds the named source analysis in `rerun/analyses.json`, and checks that its unpacked source table has
  the recorded SHA-256. Every source and code path must be a safe relative regular file listed in its package's
  `SHA256SUMS`; the verifier refuses links and opens each file without following them. It copies that table to
  `<folder>/data.csv` and `code/params.json` to `<folder>/params.json`, then runs
  `Rscript --vanilla code/<template>.R <folder>` after removing `R_PROFILE`, `R_PROFILE_USER`, `R_ENVIRON`,
  `R_ENVIRON_USER`, `R_LIBS_USER`, `R_DEFAULT_PACKAGES`, `R_SCRIPT_DEFAULT_PACKAGES` and `R_TESTS` from the child
  environment, so R reads no startup file and attaches no package before the template. `R_LIBS` and `R_LIBS_SITE`
  stay: the R installation and the libraries they name are the rerunner's own, and they supply the packages the
  template loads. Before R starts, it reads and authenticates retained
  `results/record.json`. It compares those saved bytes with a new, regular, non-linked `results_exact.json` created by
  that rerun; changing or linking either side is refused. JSON shapes and strings must match exactly, while numbers use
  the relative tolerance. The report records only the number of numbers compared and different and the largest relative
  difference, never result values.

`--formats` takes a comma-separated subset of `template`, `recipe` and `render-template`; all three run by default.
An excluded or unsupported package is reported as not rerun, not failed. With `--reviewed-only`, as the Slurm job runs
it, an analysis whose block the research agent wrote (a template run without configured settings) is rerun only when a
named person's newest review accepted it; the others are reported as not rerun, so code a model wrote never runs on an
HPC on the model's word. Render-service template code is reviewed code that no model wrote, so `--reviewed-only` does rerun it.
When any package checksum fails, no analysis is rerun at all, since the review records in
`rerun/analyses.json` are then in doubt. Without the option, on a scientist's own computer, every analysis reruns, so a
reviewer can rerun one before accepting it. `--work NAME` sends the results to a folder of that name in the package
instead of `extracted/`; a name other than `extracted` must not exist yet, so the Slurm job, which passes
`extracted-<job id>`, never shares a folder with another job from the same package. `--rscript` is split like a shell
command before `{dir}` is filled in, so a folder path with spaces stays one argument. A run that exits other than 0
fails its analysis whatever output it left, and its results are then marked not compared. Each result table of a
successful template or recipe run is compared with the retained one: identical bytes, within a relative tolerance
(1e-9 by default, `--tolerance`, which must be finite and non-negative), missing or different.
`extracted/rerun_report.json` records per analysis its format, exit and outcome (matches, differs, failed, not rerun),
per table those comparison counts or, for `render-template`, the JSON-number comparison counts, never values, and the
totals. `--rscript` sets how R starts; `{dir}` becomes the analysis folder, for example
`docker run --rm --network none -e ENRICHEDLAB_TEMPLATE_RERUN=local -v {dir}:{dir} -w {dir} <image> Rscript`.

Exit status: 0 when every checksum matches and, with `--rerun`, every analysis was rerun with matching results; 1 when a
checksum or file is wrong; 2 when a rerun failed or a retained result is missing or differs; 3 when some analysis could
not be rerun or had nothing to compare and nothing else is wrong. When several apply, the lowest number is reported. The
verifier prints counts and outcomes only. `METHOD.md` reports the template's own input-dependence record as the template
wrote it; the verifier does not repeat that check.

## SCINet hand-off

The first SCINet path is a hand-off the scientist runs. `hpc_handoff(root)` in
`src/broker/evidence_package.rs` writes the steps for each SCINet site from `src/broker/repro/hpc_sites.json`: sign in
through Open OnDemand, upload the package (Globus past Open OnDemand's 1 GB limit), unpack it with `tar
--keep-old-files` so an earlier copy is never overwritten, check it, submit the job with the project's Slurm account,
follow it and bring `extracted-JOB_ID/rerun_report.json` and the job log back. Code writes
every step from the site table and the package's folder name; no model writes them. The same steps go into
`rerun/README.md` with the SCINet guides they come from and the date they were checked, and the package preview returns
them as `hpc_handoff` for the workspace's evidence package dialog. When SCINet changes an address, a collection or a
rule, update the site table and its `checked` date; the README says the guide wins over a stale step.

## R environment

The original template and recipe analyses ran with R 4.6.1. The kit names
`docker.io/library/r-base@sha256:4e6fb1eebb29725c9c524851fb9a4bd2bd152254bcc3778cb44fd1c3d7fe9d1b`
(R 4.6.1), which reproduced two earlier packages' tables byte for byte or within the last digit.

The render service runs its reviewed analysis templates in R 4.4; an offline production-package check
used R 4.4.2. Those templates load ggplot2, ragg, svglite, scales, jsonlite and sometimes feature-specific packages. The
pinned base-R Slurm image has none of them, so `rerun.sbatch` passes `--formats template,recipe`. A render-template
analysis remains in the report as not rerun until the kit has a pinned image with that environment. It can be rerun
locally with the service's R 4.4 environment and the command in its `METHOD.md`.

## Checks

`cargo test --locked --lib evidence_package` checks that the kit lists every analysis package, carries the exact
verifier and Slurm bytes, is in `SHA256SUMS` and is deterministic, and that the SCINet steps name each site's own
sign-in address, Globus collection and account rule and the package's own file names.
`python3 -m unittest discover -s tests/repro` (also in CI) runs the verifier on synthetic packages with a stub in place
of R: a template and a recipe that match, a
render-template match and numeric difference, startup-file isolation, changed reviewed code, changed or absent source
data, unsafe or linked package/source paths, unlisted manifest files, retained-record replacement, linked rerun output,
finite non-negative tolerance validation, format selection, `--reviewed-only`, METHOD.md provenance, a failed run with
no tables (exit 2), a format it cannot replay (exit 3), changed tables, a damaged package that outranks a rerun result
(exit 1), and the recipe's tables in the catalog and imports; it also runs the Slurm script with stub `apptainer` and
`python3` commands (pull once by digest, cache off the home folder, a failed pull, an old Python, a start outside the
package). A live package is checked end to end after each release.
