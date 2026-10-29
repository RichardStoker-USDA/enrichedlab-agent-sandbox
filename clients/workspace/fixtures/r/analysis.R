# Synthetic demonstration analysis for the research workspace preview. It follows the
# reviewed-tool layout: exact inputs in, tables and base R figures out, nothing imputed.
# Rscript analysis.R INPUT_DIRECTORY OUTPUT_DIRECTORY
args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 2L) stop("Usage: Rscript analysis.R INPUT_DIRECTORY OUTPUT_DIRECTORY")
input <- args[[1L]]
output <- args[[2L]]
if (dir.exists(output) && length(list.files(output, all.files = TRUE, no.. = TRUE))) {
  stop("Output directory must be empty; preserve each earlier attempt")
}
dir.create(output, recursive = TRUE, showWarnings = FALSE)

x <- read.csv(file.path(input, "measurements.csv"), stringsAsFactors = FALSE)
stopifnot(identical(names(x), c("sample_id", "condition", "response")), nrow(x) > 0L,
          !anyNA(x), all(is.finite(x$response)), !anyDuplicated(x$sample_id))
x$condition <- factor(x$condition, levels = c("reference", "diet_a", "diet_b"))
stopifnot(!anyNA(x$condition))
m <- read.csv(file.path(input, "metabolites.csv"), check.names = FALSE, stringsAsFactors = FALSE)
stopifnot(identical(names(m)[1:2], c("sample_id", "condition")), !anyNA(m),
          all(m$condition %in% c("reference", "diet_a")))

# Descriptive summary for each diet.
summary <- do.call(rbind, lapply(levels(x$condition), function(level) {
  v <- x$response[x$condition == level]
  data.frame(condition = level, n = length(v), mean = round(mean(v), 3), median = round(median(v), 3),
             standard_deviation = round(sd(v), 3))
}))
write.csv(summary, file.path(output, "condition_summary.csv"), row.names = FALSE)

# Difference from the reference diet with 95% confidence intervals (one-way linear model).
fit <- lm(response ~ condition, data = x)
ci <- confint(fit)
effects <- data.frame(term = c("diet_a vs reference", "diet_b vs reference"),
                      estimate = round(coef(fit)[-1], 3), lower_95 = round(ci[-1, 1], 3), upper_95 = round(ci[-1, 2], 3))
write.csv(effects, file.path(output, "condition_effects.csv"), row.names = FALSE)

# Feature-wise Welch t-tests, diet A against the reference, with Benjamini-Hochberg q-values.
features <- names(m)[-(1:2)]
a <- m$condition == "diet_a"
ref <- m$condition == "reference"
changes <- do.call(rbind, lapply(features, function(f) {
  v <- m[[f]]
  data.frame(feature = f, log2_fold_change = mean(v[a]) - mean(v[ref]), p_value = t.test(v[a], v[ref])$p.value)
}))
changes$q_value <- p.adjust(changes$p_value, method = "BH")
changes <- changes[order(changes$p_value), ]
changes$log2_fold_change <- round(changes$log2_fold_change, 3)
changes$p_value <- signif(changes$p_value, 4)
changes$q_value <- signif(changes$q_value, 4)
write.csv(changes, file.path(output, "metabolite_changes.csv"), row.names = FALSE)

blue <- "#022851"; gold <- "#FFBF00"; mist <- "#9AADC2"; axis_ink <- "#53647A"
labels <- c(reference = "Reference", diet_a = "Diet A", diet_b = "Diet B")
draw_response <- function() {
  par(mar = c(4.6, 5, 3.4, 1), las = 1, family = "sans", col.axis = axis_ink)
  boxplot(response ~ condition, data = x, names = labels[levels(x$condition)], outline = FALSE,
          col = "white", border = blue, lwd = 1.2, boxwex = 0.55, ylim = extendrange(x$response, f = 0.08),
          xlab = "Test diet", ylab = "Plasma response score (synthetic units)", main = "Response by test diet")
  set.seed(7)
  stripchart(response ~ condition, data = x, vertical = TRUE, method = "jitter", jitter = 0.13, add = TRUE,
             pch = 21, cex = 1.05, col = blue, bg = gold)
  mtext("Simulated demonstration data, 12 participants per diet", side = 3, line = 0.3, cex = 0.8, col = axis_ink)
}
draw_volcano <- function() {
  par(mar = c(4.6, 5, 3.4, 1), las = 1, family = "sans", col.axis = axis_ink)
  y <- -log10(changes$p_value)
  up <- changes$q_value < 0.05 & changes$log2_fold_change >= 1
  down <- changes$q_value < 0.05 & changes$log2_fold_change <= -1
  plot(changes$log2_fold_change, y, type = "n", xlab = "log2 fold change, diet A vs reference",
       ylab = expression(-log[10] ~ italic(p)), main = "Feature changes: diet A vs reference", bty = "l",
       xlim = extendrange(changes$log2_fold_change, f = 0.14), ylim = c(0, max(y) * 1.12))
  abline(v = c(-1, 1), lty = 2, col = mist)
  cutoff <- max(changes$p_value[changes$q_value < 0.05])
  abline(h = -log10(cutoff), lty = 2, col = mist)
  points(changes$log2_fold_change[!(up | down)], y[!(up | down)], pch = 16, cex = 0.8, col = "#B3C1D1")
  points(changes$log2_fold_change[down], y[down], pch = 21, cex = 1.05, col = blue, bg = blue)
  points(changes$log2_fold_change[up], y[up], pch = 21, cex = 1.05, col = blue, bg = gold)
  top <- head(which(up | down), 6L)
  text(changes$log2_fold_change[top], y[top], changes$feature[top], pos = ifelse(changes$log2_fold_change[top] > 0, 4, 2),
       cex = 0.62, col = blue, offset = 0.35, xpd = NA)
  legend("top", bty = "n", cex = 0.74, pch = c(21, 21, 16), pt.bg = c(gold, blue, "#B3C1D1"), col = c(blue, blue, "#B3C1D1"),
         legend = c("Higher with diet A", "Lower with diet A", "Below the thresholds"), horiz = TRUE)
  mtext(sprintf("%d simulated features; Welch t-tests, Benjamini-Hochberg q < 0.05 and |log2 fold change| >= 1", nrow(changes)),
        side = 3, line = 0.3, cex = 0.78, col = axis_ink)
}
save_figure <- function(name, draw, width = 7.5, height = 5.4) {
  pdf(file.path(output, paste0(name, ".pdf")), width = width, height = height, useDingbats = FALSE)
  draw(); invisible(dev.off())
  svg(file.path(output, paste0(name, ".svg")), width = width, height = height)
  draw(); invisible(dev.off())
}
save_figure("response_by_diet", draw_response)
save_figure("feature_volcano", draw_volcano)
pdf(file.path(output, "model_checks.pdf"), width = 7, height = 5.4, useDingbats = FALSE)
par(las = 1, col.axis = axis_ink)
plot(fit, which = 1, caption = "Residuals against fitted values")
plot(fit, which = 2, caption = "Normal quantiles of the residuals")
invisible(dev.off())

writeLines(c(
  "Method: per-diet count, mean, median and standard deviation; a one-way linear model for differences",
  "from the reference diet with 95% confidence intervals; feature-wise Welch t-tests for diet A against the",
  "reference with Benjamini-Hochberg q-values. Residual plots check the linear model's assumptions.",
  "Every input value is simulated for this preview. No values are imputed, filtered or generated by a model.",
  "Jittered point positions in the box plot are display offsets, not altered measurements.",
  "Scientific review is pending; a preview result has no scientific interpretation."
), file.path(output, "method.txt"))
writeLines(c(R.version.string, "Graphics: base R pdf() and Cairo svg() devices",
             paste("Packages:", paste(sort(c(sessionInfo()$basePkgs)), collapse = ", "))),
           file.path(output, "environment.txt"))
cat(sprintf("Read %d measurements and %d features. Wrote 4 tables, 5 figure files and model checks.\n", nrow(x), length(features)))
cat("analysis_completed;scientific_review_pending\n")
