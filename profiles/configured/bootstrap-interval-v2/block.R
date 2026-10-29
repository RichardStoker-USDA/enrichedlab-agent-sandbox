# Configured recipe bootstrap-interval-v2: a percentile bootstrap interval of the {{statistic}} tooth
# length within each group of {{groups_words}}. The agent chose these settings; it wrote none of this code.
analyze <- function(data) {
  groups <- split(data$len, {{groups}}, drop = TRUE)
  measure <- function(values) {{statistic}}(values)
  rows <- lapply(names(groups), function(name) {
    values <- groups[[name]]
    draws <- vapply(seq_len({{resamples}}), function(i) measure(sample(values, length(values), replace = TRUE)),
                    numeric(1))
    bounds <- quantile(draws, probs = c((1 - {{level}}) / 2, (1 + {{level}}) / 2), names = FALSE)
    data.frame(group = name, observations = length(values), estimate = measure(values),
               lower = min(bounds), upper = max(bounds))
  })
  intervals <- Reduce(rbind, rows)
  list(
    tables = list(bootstrap_intervals = intervals),
    figures = list(bootstrap_intervals = function() {
      positions <- seq_len(nrow(intervals))
      plot(positions, intervals$estimate, ylim = c(min(intervals$lower), max(intervals$upper)), xaxt = "n",
           xlab = {{groups_label}}, ylab = "Tooth length", pch = 21, bg = "#FFBF00", col = "#022851")
      arrows(positions, intervals$lower, positions, intervals$upper, angle = 90, code = 3, length = 0.05,
             col = "#022851")
      axis(1, at = positions, labels = intervals$group)
    }),
    notes = c("Percentile bootstrap of the {{statistic}} within each group: {{resamples}} resamples, level {{level}}.")
  )
}
