# Preview: a model-written block in a reviewed template

`package/` is the output of a reviewed R template (`template://r-generated-analysis/v2`; its `template.R` digest is
in `package/run.json`) run on the simulated `../input/measurements.csv` with the block in `package/code/block.R`. It
ran with the profile's command in a pinned `r-base` image with no network, a read-only root and read-only input, so
`package/boundary.json` records an isolation probe, and `package/run.json` records the rerun on a changed copy of the
input (`input_dependence`). Every value is simulated.

`admission.json` is the broker's admission report for that block; `src/broker/codegen.rs` checks that the block
still produces exactly this report. To regenerate the package, stage `package/input/contract.dcf`, the dataset and the
template's `run.sh` and `template.R` in an empty folder, run the template, and replace `package/` with the run's
unpacked `/artifacts/payload`, leaving out its `SHA256SUMS` (the preview packager writes its own).
