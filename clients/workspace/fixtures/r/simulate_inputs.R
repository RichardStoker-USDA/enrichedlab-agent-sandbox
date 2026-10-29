# Simulated inputs for the research workspace preview. Every value is invented; nothing
# here is research data or derived from it.
# Rscript simulate_inputs.R INPUT_DIRECTORY
args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 1L) stop("Usage: Rscript simulate_inputs.R INPUT_DIRECTORY")
dir.create(args[[1L]], recursive = TRUE, showWarnings = FALSE)
set.seed(20260923)

# Plasma response score for three test diets, 12 simulated participants each.
per_diet <- 12L
diets <- c("reference", "diet_a", "diet_b")
measurements <- data.frame(
  sample_id = sprintf("p%02d", seq_len(per_diet * length(diets))),
  condition = rep(diets, each = per_diet),
  response = round(c(rnorm(per_diet, 8.9, 0.9), rnorm(per_diet, 12.3, 1.1), rnorm(per_diet, 10.7, 1.0)), 2)
)
write.csv(measurements, file.path(args[[1L]], "measurements.csv"), row.names = FALSE)

# log2 intensities for 400 simulated features in the reference and diet A participants.
# A minority of features carry a simulated shift of varying size; the rest are noise.
features <- sprintf("feature_%03d", seq_len(400L))
baseline <- rnorm(length(features), 16, 1.5)
effect <- numeric(length(features))
changed <- sample(length(features), 60L)
effect[changed] <- sample(c(-1, 1), length(changed), replace = TRUE) * rgamma(length(changed), shape = 2.2, rate = 2.4)
simulate <- function(shift) t(vapply(seq_len(per_diet), function(i) baseline + shift + rnorm(length(features), 0, 0.6), numeric(length(features))))
intensities <- rbind(simulate(0), simulate(effect))
colnames(intensities) <- features
metabolites <- data.frame(sample_id = measurements$sample_id[seq_len(2L * per_diet)],
                          condition = rep(diets[1:2], each = per_diet), round(intensities, 3), check.names = FALSE)
write.csv(metabolites, file.path(args[[1L]], "metabolites.csv"), row.names = FALSE)
