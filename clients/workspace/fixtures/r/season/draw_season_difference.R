# The season figure for the workspace preview's long run: the canopy-to-air temperature difference by block and hour
# of day, drawn in base R from the comparison step's table. Every value is simulated. A block without rows stays an
# empty column; nothing is filled in.
# Rscript draw_season_difference.R INPUT_CSV OUTPUT_DIRECTORY
args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 2L) stop("Usage: Rscript draw_season_difference.R INPUT_CSV OUTPUT_DIRECTORY")
d <- read.csv(args[[1L]], stringsAsFactors = FALSE)
stopifnot(identical(names(d), c("block", "hour", "difference_c")), nrow(d) > 0L, !anyNA(d),
          !anyDuplicated(d[c("block", "hour")]))
output <- args[[2L]]
dir.create(output, recursive = TRUE, showWarnings = FALSE)

hours <- sort(unique(d$hour))
blocks <- seq(min(d$block), max(d$block))
z <- matrix(NA_real_, nrow = length(hours), ncol = length(blocks))
z[cbind(match(d$hour, hours), match(d$block, blocks))] <- d$difference_c
limits <- range(d$difference_c)
palette <- hcl.colors(256, "viridis")
missing <- setdiff(blocks, d$block)
ink <- "#33414E"

draw <- function() {
  layout(matrix(1:2, ncol = 2L), widths = c(6, 1.1))
  par(mar = c(4.4, 4.4, 3.4, 0.6), las = 1, col.axis = ink, col.lab = ink, fg = ink)
  image(hours, blocks, z, col = palette, zlim = limits, axes = FALSE, xlab = "Hour of day", ylab = "Block")
  axis(1, at = seq(0, 21, 3))
  axis(2, at = blocks, cex.axis = 0.68)
  box()
  title(main = "Canopy minus air temperature by block and hour", cex.main = 1, font.main = 1, col.main = ink, line = 1.9)
  mtext(sprintf("Simulated loggers, %d blocks over three days%s", length(unique(d$block)),
                if (length(missing)) sprintf("; block %s has no rows", paste(missing, collapse = ", ")) else ""),
        side = 3, line = 0.5, cex = 0.78, col = ink)
  par(mar = c(4.4, 0.6, 3.4, 3.6))
  levels <- seq(limits[1L], limits[2L], length.out = length(palette))
  image(1, levels, matrix(levels, nrow = 1L), col = palette, axes = FALSE, xlab = "", ylab = "")
  axis(4, cex.axis = 0.8)
  mtext("Difference (degrees C)", side = 4, line = 2.4, cex = 0.8, las = 0, col = ink)
  box()
}
pdf(file.path(output, "season_difference.pdf"), width = 7.5, height = 6, useDingbats = FALSE)
draw(); invisible(dev.off())
svg(file.path(output, "season_difference.svg"), width = 7.5, height = 6)
draw(); invisible(dev.off())
cat(sprintf("Read %d rows (%d blocks by %d hours). Wrote season_difference.pdf and season_difference.svg.\n",
            nrow(d), length(unique(d$block)), length(hours)))
