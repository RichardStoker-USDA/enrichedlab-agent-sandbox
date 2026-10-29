args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 1L) stop("Usage: Rscript prepare_reference.R OUTPUT_DIRECTORY")
root <- args[[1L]]
dir.create(file.path(root, "inputs"), recursive = TRUE, showWarnings = FALSE)
data("ToothGrowth", package = "datasets", envir = environment())
stopifnot(is.data.frame(ToothGrowth), nrow(ToothGrowth) > 0L)
write.csv(ToothGrowth, file.path(root, "inputs", "observations.csv"), row.names = FALSE)
saveRDS(ToothGrowth, file.path(root, "inputs", "original.rds"), version = 3)
writeLines(c(
  "Public demonstration data: R datasets::ToothGrowth.",
  "Guinea pigs, not mice or a private laboratory dataset.",
  "Source: https://stat.ethz.ch/R-manual/R-patched/library/datasets/html/ToothGrowth.html",
  paste("R version:", getRversion()),
  paste("datasets package:", packageVersion("datasets")),
  "The data are exported by R from the pinned runtime; an LLM supplies no measurements.",
  "Keep the original study limitations and R dataset documentation with any interpretation."
), file.path(root, "inputs", "source.txt"))
cat("reference_input_exported\n")
