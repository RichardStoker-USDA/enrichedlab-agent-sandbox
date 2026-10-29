# Model-written analysis (unvalidated)

An R block written by the research model was given to the reviewed template `template://r-generated-analysis/v2`.
Outcome: succeeded.

The template supplied the data, opened every plot device and wrote every file. It runs
a block in a separate R process where the block's own code can name only allowlisted R
functions. That narrows what a block can do; the isolation boundary is the sandbox.
Before the block ran, the probe in `boundary.json` found no network access, no service-account token and a read-only root and input.
The template ran the block again on a changed copy of the input and refuses tables
whose numbers do not change. Code can still compute a value that does not come from
the data, so these results are unvalidated until a scientist reviews the code and the
output.

- `code/block.R`: the exact model-written block. `code/block.diff` is the only change
  from the reviewed template, `code/template.R`.
- `admission.json`: the template's check of the block before it ran.
- `boundary.json`: the isolation probe the template ran before the block.
- `results/` tables (CSV) and `figures/` plots (PDF, and SVG when available).
- `run.json`: status, seed, R version, digests, the input-dependence check, tables and figures.
- `input/`: the contract and the exact dataset the block received.
- `logs/`: template log, the block's printed output and warnings.
- `SHA256SUMS`: a checksum for every file in this package.

## Rerun

ENRICHEDLAB_TEMPLATE_RERUN=local Rscript --vanilla code/template.R input code/block.R rerun 20260923

Create an empty `rerun` directory first. Tables in `rerun/results` should match
`results` exactly; figures are redrawn from the same code and data.
