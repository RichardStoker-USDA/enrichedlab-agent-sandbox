# EnrichedLab reviewed template: template://r-generated-analysis/v2
#
# Runs one model-written block that defines analyze(data). The template owns
# everything else: the staged dataset, the evaluation environment, plot devices,
# output files and the evidence record. The block is checked again here before it
# runs, and it runs in a separate R process whose environment holds only allowlisted
# names. This process never evaluates block code. These checks narrow what a block
# can do; the isolation boundary is the sandbox (no network, read-only root and
# input). Results are unvalidated until a scientist reviews them.
#
# Usage: Rscript --vanilla template.R <work dir> <block file> <output dir> <seed>
# <work dir> holds contract.dcf and the dataset it names. <output dir> must
# exist and be empty. Set ENRICHEDLAB_TEMPLATE_RERUN=local for a local rerun:
# the isolation probe is then recorded but not enforced.
# The template calls itself with --child to run the block; that mode is internal.

options(warn = 1, stringsAsFactors = FALSE, useFancyQuotes = FALSE)
TEMPLATE_REF <- "template://r-generated-analysis/v2"
args <- commandArgs(trailingOnly = TRUE)
utc <- function(t) format(t, "%Y-%m-%dT%H:%M:%OSZ", tz = "UTC")
or_empty <- function(x, empty) if (is.null(x)) empty else x

# ---- Minimal JSON writer (base R only). Length-1 atomics are scalars; use
# as.list() or I() for a one-element array. Named lists are objects.
json_string <- function(x) {
  x <- enc2utf8(as.character(x))
  x <- gsub("\\", "\\\\", x, fixed = TRUE)
  x <- gsub("\"", "\\\"", x, fixed = TRUE)
  x <- gsub("\n", "\\n", x, fixed = TRUE)
  x <- gsub("\r", "\\r", x, fixed = TRUE)
  x <- gsub("\t", "\\t", x, fixed = TRUE)
  x <- gsub("[[:cntrl:]]", " ", x)
  paste0("\"", x, "\"")
}
json_scalar <- function(x) {
  if (length(x) != 1L || is.na(x)) return("null")
  if (is.logical(x)) return(if (x) "true" else "false")
  if (is.numeric(x)) return(if (is.finite(x)) format(x, digits = 15, scientific = FALSE, trim = TRUE) else "null")
  json_string(x)
}
to_json <- function(x, indent = "") {
  inner <- paste0(indent, "  ")
  if (is.null(x)) return("null")
  if (is.list(x) && !is.null(names(x)) && length(x) > 0L) {
    parts <- vapply(names(x), function(n) paste0(inner, json_string(n), ": ", to_json(x[[n]], inner)), "")
    return(paste0("{\n", paste(parts, collapse = ",\n"), "\n", indent, "}"))
  }
  if (is.list(x) || inherits(x, "AsIs") || length(x) != 1L) {
    if (length(x) == 0L) return(if (is.list(x) && !is.null(names(x))) "{}" else "[]")
    items <- if (is.list(x)) lapply(x, to_json, indent = inner) else lapply(unclass(x), json_scalar)
    return(paste0("[\n", paste0(inner, unlist(items), collapse = ",\n"), "\n", indent, "]"))
  }
  json_scalar(x)
}
write_json <- function(x, path) writeLines(to_json(x), path, useBytes = TRUE)
sha256 <- function(path) {
  f <- get0("sha256sum", envir = asNamespace("tools"), inherits = FALSE)
  if (is.function(f)) paste0("sha256:", unname(f(path))) else NA_character_
}

# ---- Names a block may call or refer to, besides the names it binds itself and the
# dataset's columns. The broker's admission holds the same list (core
# src/broker/codegen.rs, ALLOWED); both record the digest over the sorted names.
ALLOWED <- list(
  base = c("F", "Filter", "Find", "I", "LETTERS", "Map", "NCOL", "NROW", "Negate", "Position", "Reduce", "T",
           "abs", "all", "all.equal", "any", "anyNA", "aperm", "append", "apply", "array", "as.Date",
           "as.character", "as.data.frame", "as.double", "as.factor", "as.integer", "as.list", "as.logical",
           "as.matrix", "as.numeric", "as.table", "as.vector", "attr", "bquote", "by", "c", "cbind", "ceiling",
           "character", "choose", "class", "colMeans", "colSums", "colnames", "conditionMessage", "cos",
           "crossprod", "cummax", "cummin", "cumprod", "cumsum", "cut", "data.frame", "data.matrix", "det",
           "diag", "diff", "difftime", "dim", "dimnames", "double", "droplevels", "duplicated", "endsWith",
           "exp", "expand.grid", "expm1", "expression", "factor", "factorial", "findInterval", "floor",
           "format", "formatC", "gamma", "grep", "grepl", "gsub", "identical", "identity", "ifelse", "inherits",
           "integer", "interaction", "intersect", "inverse.rle", "invisible", "is.character", "is.data.frame",
           "is.double", "is.element", "is.factor", "is.finite", "is.infinite", "is.integer", "is.list",
           "is.logical", "is.matrix", "is.na", "is.nan", "is.null", "is.numeric", "is.unsorted", "is.vector",
           "isFALSE", "isTRUE", "jitter", "lapply", "lbeta", "lchoose", "length", "lengths", "letters",
           "levels", "lgamma", "list", "log", "log10", "log1p", "log2", "logical", "make.names", "make.unique",
           "mapply", "margin.table", "match", "matrix", "max", "mean", "merge", "message", "min", "missing",
           "month.abb", "month.name", "names", "nargs", "nchar", "ncol", "nlevels", "noquote", "nrow",
           "numeric", "on.exit", "order", "outer", "paste", "paste0", "pi", "plot", "pmax", "pmin", "pretty",
           "prettyNum", "print", "prod", "prop.table", "proportions", "range", "rank", "rbind", "regexpr",
           "regmatches", "rep", "rep_len", "replace", "return", "rev", "rle", "round", "row.names", "rowMeans",
           "rowSums", "rownames", "rowsum", "sample", "sample.int", "sapply", "scale", "seq", "seq.int",
           "seq_along", "seq_len", "set.seed", "setdiff", "sign", "signif", "sin", "solve", "sort", "split",
           "sprintf", "sqrt", "startsWith", "stop", "stopifnot", "strsplit", "structure", "sub", "subset",
           "substr", "substring", "sum", "summary", "suppressMessages", "suppressWarnings", "sweep", "switch",
           "t", "table", "tabulate", "tan", "tapply", "toString", "tolower", "toupper", "transform", "trimws",
           "trunc", "tryCatch", "typeof", "unclass", "union", "unique", "unlist", "unname", "unsplit", "vapply",
           "vector", "warning", "which", "which.max", "which.min", "with", "withCallingHandlers", "xor"),
  stats = c("AIC", "BIC", "Gamma", "IQR", "TukeyHSD", "add1", "addmargins", "aggregate", "anova", "aov",
            "approx", "as.formula", "ave", "bartlett.test", "binom.test", "binomial", "biplot", "chisq.test",
            "cmdscale", "coef", "coefficients", "complete.cases", "confint", "cor", "cor.test", "cov",
            "cov2cor", "cutree", "dbeta", "dbinom", "dchisq", "density", "deviance", "dexp", "df",
            "df.residual", "dgamma", "dist", "dlnorm", "dnorm", "dpois", "drop1", "dt", "dunif", "ecdf",
            "fisher.test", "fitted", "fitted.values", "fivenum", "fligner.test", "formula", "friedman.test",
            "ftable", "gaussian", "glm", "hclust", "integrate", "interaction.plot", "inverse.gaussian",
            "kmeans", "kruskal.test", "ks.test", "lm", "loess", "logLik", "lowess", "mad", "mahalanobis",
            "mantelhaen.test", "mcnemar.test", "median", "model.frame", "model.matrix", "model.response",
            "model.tables", "na.exclude", "na.fail", "na.omit", "na.pass", "nls", "nobs", "offset",
            "oneway.test", "optim", "optimize", "p.adjust", "pairwise.prop.test", "pairwise.t.test",
            "pairwise.wilcox.test", "pbeta", "pbinom", "pchisq", "pexp", "pf", "pgamma", "plnorm", "pnorm",
            "poisson", "poisson.test", "poly", "power.prop.test", "power.t.test", "ppoints", "ppois", "prcomp",
            "predict", "princomp", "prop.test", "pt", "ptukey", "punif", "qbeta", "qbinom", "qchisq", "qexp",
            "qf", "qgamma", "qlnorm", "qnorm", "qpois", "qqline", "qqnorm", "qqplot", "qt", "qtukey",
            "quantile", "quasi", "quasibinomial", "quasipoisson", "qunif", "rbeta", "rbinom", "rcauchy",
            "rchisq", "reformulate", "relevel", "reorder", "resid", "residuals", "rexp", "rgamma", "rgeom",
            "rhyper", "rlnorm", "rlogis", "rmultinom", "rnbinom", "rnorm", "rpois", "rt", "runif", "runmed",
            "rweibull", "sd", "setNames", "shapiro.test", "sigma", "smooth.spline", "spline", "step", "t.test",
            "terms", "uniroot", "update", "var", "var.test", "vcov", "weighted.mean", "weights", "wilcox.test",
            "xtabs"),
  graphics = c("abline", "arrows", "assocplot", "axis", "barplot", "box", "boxplot", "bxp", "cdplot", "contour",
               "coplot", "dotchart", "filled.contour", "fourfoldplot", "grid", "hist", "image", "layout",
               "legend", "lines", "matlines", "matplot", "matpoints", "mosaicplot", "mtext", "pairs", "par",
               "persp", "pie", "plot.new", "plot.window", "points", "polygon", "rect", "rug", "segments",
               "smoothScatter", "spineplot", "stripchart", "sunflowerplot", "symbols", "text", "title"),
  grDevices = c("adjustcolor", "boxplot.stats", "col2rgb", "colorRampPalette", "extendrange", "gray", "grey",
                "hcl", "hcl.colors", "heat.colors", "palette.colors", "rainbow", "rgb", "terrain.colors",
                "topo.colors"),
  utils = c("combn", "head", "modifyList", "stack", "str", "tail", "unstack"),
  MASS = c("boxcox", "fitdistr", "glm.nb", "lda", "mvrnorm", "polr", "qda", "rlm", "stepAIC"),
  nlme = c("VarCorr", "corAR1", "corCompSymm", "fixef", "gls", "intervals", "lme", "ranef", "varIdent"),
  mgcv = c("gam", "s", "te", "ti"),
  survival = c("Surv", "cox.zph", "coxph", "strata", "survdiff", "survfit"),
  cluster = c("agnes", "clara", "daisy", "diana", "fanny", "pam", "silhouette"),
  rpart = c("prune", "rpart"),
  boot = c("boot", "boot.ci"),
  splines = c("bs", "ns"),
  lattice = c("barchart", "bwplot", "densityplot", "dotplot", "histogram", "levelplot", "stripplot", "xyplot"),
  nnet = c("multinom"),
  KernSmooth = c("bkde", "dpik", "locpoly")
)
ALLOWED_NAMES <- sort(unique(unlist(ALLOWED, use.names = FALSE)), method = "radix")
allowlist_digest <- function() {
  path <- tempfile("allowlist-")
  on.exit(unlink(path))
  writeBin(charToRaw(paste(ALLOWED_NAMES, collapse = "\n")), path)
  sha256(path)
}

# The language every block needs. They are not names a block refers to; they are the
# functions R calls for its syntax (x[1], x$a <- v, if, for, operators).
SYNTAX <- c(
  "(", "{", "<-", "=", "if", "for", "while", "repeat", "break", "next", "function", "[", "[[", "$", "@",
  "[<-", "[[<-", "$<-", "+", "-", "*", "/", "^", "%%", "%/%", "%*%", "%o%", "%in%", "==", "!=", "<", ">",
  "<=", ">=", "!", "&", "&&", "|", "||", ":", "~", "::", "names<-", "colnames<-", "rownames<-",
  "row.names<-", "dimnames<-", "dim<-", "levels<-", "attr<-", "class<-", "is.na<-", "diag<-", "substr<-",
  "substring<-"
)
CONSTANTS <- c("TRUE", "FALSE", "T", "F", "NULL", "NA", "NA_integer_", "NA_real_", "NA_character_", "Inf",
               "NaN", "pi", "LETTERS", "letters", "month.abb", "month.name")
# Calls that build data from their arguments; a written number in one is invented
# result data unless it only sets the shape of the result.
CONSTRUCTORS <- c("data.frame", "list", "matrix", "array", "cbind", "rbind", "structure", "setNames",
                  "expand.grid", "transform", "append", "replace", "table", "as.data.frame", "as.matrix")
SHAPE_ARGUMENTS <- c("nrow", "ncol", "byrow", "dim", "stringsAsFactors", "check.names", "deparse.level",
                     "make.row.names", "fix.empty.names", "KEEP.OUT.ATTRS", "row.names", "responseName",
                     "useNA", "dnn", "after", "exclude")
# Kept for their specific reason codes; none of them is on the allowlist.
FORBIDDEN <- c(
  "system", "system2", "shell", "shell.exec", "pipe", "fifo", "dyn.load", "dyn.unload",
  "library.dynam", "Sys.setenv", "Sys.unsetenv", "Sys.getenv", "Sys.setlocale", "Sys.chmod",
  "Sys.umask", "Sys.readlink", "Sys.info", "Sys.getpid", "Sys.sleep", "Sys.which", "R.home",
  "commandArgs", "quit", "setTimeLimit", "setSessionTimeLimit",
  "file", "url", "gzfile", "bzfile", "xzfile", "unz", "gzcon", "socketConnection",
  "socketAccept", "serverSocket", "make.socket", "read.socket", "write.socket",
  "download.file", "curlGetHeaders", "readLines", "readline", "readRDS", "load", "source",
  "sys.source", "scan", "read.table", "read.csv", "read.csv2", "read.delim", "read.delim2",
  "read.fwf", "read.dcf", "readBin", "readChar", "writeLines", "writeBin", "writeChar",
  "write", "write.table", "write.csv", "write.csv2", "saveRDS", "save", "save.image", "sink",
  "capture.output", "cat", "dput", "dget", "dump", "setwd", "getwd", "unlink", "file.remove",
  "file.rename", "file.copy", "file.create", "file.append", "file.symlink", "file.link",
  "file.exists", "file.info", "file.access", "file.path", "dir.create", "dir.exists",
  "list.files", "list.dirs", "dir", "normalizePath", "path.expand", "tempfile", "tempdir",
  "Sys.glob", "open", "close", "textConnection", "rawConnection", "stdin", "stdout",
  "stderr", "showConnections", "getConnection", "closeAllConnections",
  "eval", "evalq", "eval.parent", "parse", "str2lang", "str2expression", "get", "get0",
  "mget", "assign", "delayedAssign", "makeActiveBinding", "environment", "sys.function",
  "sys.call", "sys.frame", "sys.frames", "sys.calls", "parent.frame", "parent.env",
  "globalenv", "emptyenv", "topenv", "baseenv", "as.environment", "list2env", "new.env",
  "local", "attach", "detach", "library", "require", "requireNamespace", "loadNamespace",
  "attachNamespace", "getNamespace", "asNamespace", "getExportedValue", "do.call",
  "match.fun", "body", "formals", "trace", "untrace", "browser", "debug", "debugonce",
  "undebug", "options", "reg.finalizer", "invokeRestart", "setClass", "setMethod",
  "setGeneric", "setRefClass", "setValidity", "lockBinding", "unlockBinding", "rm", "remove",
  "getFromNamespace", "getAnywhere", "argsAnywhere", "assignInNamespace", "assignInMyNamespace",
  "fixInNamespace", "getS3method", "registerS3method", "pos.to.env", "sys.on.exit",
  "sys.status", "sys.parent", "sys.parents", "sys.nframe", "dump.frames", "serialize",
  "unserialize", "loadhistory", "savehistory", "system.file", "readRenviron",
  "Sys.setFileTime", "Sys.setlanguage", "nsl", "socketSelect", "zip", "unzip", "tar", "untar",
  "browseURL", "help", "example", "demo", "vignette", "RSiteSearch", "download.packages",
  "file.show", "file.edit", "edit", "fix", "View", "install.packages", "remove.packages",
  "update.packages", "installed.packages", "pdf", "svg", "png", "jpeg", "bmp", "tiff",
  "postscript", "cairo_pdf", "cairo_ps", "x11", "X11", "quartz", "windows", "dev.new",
  "dev.off", "dev.set", "dev.copy", "dev.print", "savePlot", "graphics.off",
  "as.call", "as.name", "as.symbol", "as.function", "call", "match.call", "quote", "substitute",
  "Recall", "Vectorize", "environmentName", "sys.source", "rapply", "within", "curve"
)
NAMESPACES <- c("base", "stats", "graphics", "grDevices", "utils", "MASS", "boot", "splines",
                "nlme", "mgcv", "survival", "cluster", "lattice", "rpart", "nnet", "Matrix",
                "KernSmooth")

# ---- The environment a block runs in: only the allowlisted names and the syntax,
# rooted at the empty environment. A name assembled from strings at run time (for
# example through match.fun) finds nothing else. Package functions load on first use.
allowed_environment <- function() {
  env <- new.env(parent = emptyenv())
  base <- baseenv()
  for (name in SYNTAX) if (name != "::") assign(name, get(name, envir = base), envir = env)
  for (pkg in names(ALLOWED)) {
    for (name in ALLOWED[[pkg]]) {
      if (pkg %in% c("base", "stats", "graphics", "grDevices", "utils")) {
        value <- if (pkg == "base") get(name, envir = base) else getExportedValue(pkg, name)
        assign(name, value, envir = env)
      } else {
        makeActiveBinding(name, local({ p <- pkg; n <- name; function() getExportedValue(p, n) }), env)
      }
    }
  }
  # pkg::name for an allowlisted name gives the same object as the bare name. Both
  # admission checks refuse any other pkg::name in block code, so the wider set here
  # serves only calls that R's own functions build and evaluate in the block's frame
  # (boxplot's formula method calls stats::model.frame.default, for example).
  assign("::", function(pkg, name) {
    pkg <- as.character(substitute(pkg))
    name <- as.character(substitute(name))
    if (name %in% ALLOWED_NAMES && pkg %in% NAMESPACES) return(get(name, envir = env, inherits = FALSE))
    if (!(pkg %in% NAMESPACES) || name %in% FORBIDDEN || startsWith(name, ".")) stop(sprintf("not allowed: %s::%s", pkg, name), call. = FALSE)
    getExportedValue(pkg, name)
  }, envir = env)
  lockEnvironment(env, bindings = TRUE)
  env
}

# ---- Child mode: run the block on one dataset and write what it returns. This
# process is the only one that evaluates block code.
run_child <- function(block_file, data_file, result_dir, seed) {
  data <- readRDS(data_file)
  limits <- readRDS(file.path(dirname(data_file), "limits.rds"))
  for (d in c("results", "figures")) dir.create(file.path(result_dir, d), mode = "0700")
  status <- "succeeded"
  error_text <- NA_character_
  warnings_seen <- character()
  tables_out <- list()
  figures_out <- list()
  notes_out <- character()
  env <- allowed_environment()
  # Base and stats code resolve a function named by a string through match.fun from
  # their own frames, which see all of base. While block code runs, that resolves only
  # the allowlist and the syntax; the template's own device and file calls run with the
  # original between those windows.
  restricted_match_fun <- function(FUN, descend = TRUE) {
    if (is.function(FUN)) return(FUN)
    if ((is.character(FUN) && length(FUN) == 1L) || is.name(FUN)) {
      name <- as.character(FUN)
      if (name %in% c(ALLOWED_NAMES, SYNTAX)) return(get(name, envir = env, inherits = FALSE))
      stop(sprintf("a function named by a string must be on the allowlist: %s", name), call. = FALSE)
    }
    stop("match.fun needs a function or the name of an allowlisted one", call. = FALSE)
  }
  original_match_fun <- get("match.fun", envir = baseenv())
  resolve <- function(f) {
    unlockBinding("match.fun", baseenv())
    assign("match.fun", f, envir = baseenv())
    lockBinding("match.fun", baseenv())
  }
  as_block <- function(code) {
    resolve(restricted_match_fun)
    on.exit(resolve(original_match_fun))
    code
  }
  block_env <- new.env(parent = env)
  exprs <- parse(block_file, keep.source = FALSE)
  set.seed(seed)
  output_con <- file(file.path(result_dir, "block_output.txt"), open = "wt")
  sink(output_con)
  sink(output_con, type = "message")
  result <- tryCatch(withCallingHandlers(as_block({
    for (expression in as.list(exprs)) eval(expression, envir = block_env)
    analyze <- get("analyze", envir = block_env, mode = "function", inherits = FALSE)
    analyze(data)
  }), warning = function(w) {
    warnings_seen <<- c(warnings_seen, conditionMessage(w))
    invokeRestart("muffleWarning")
  }), error = function(e) e)
  sink(type = "message")
  sink()
  close(output_con)
  valid_name <- function(x) length(x) > 0L && all(grepl("^[a-z][a-z0-9_]{0,40}$", x)) && !anyDuplicated(x)
  if (inherits(result, "error")) {
    status <- "block_failed"
    error_text <- substr(conditionMessage(result), 1L, 500L)
  } else if (!is.list(result) || !all(names(result) %in% c("tables", "figures", "notes")) ||
             !is.list(or_empty(result$tables, list())) || !is.list(or_empty(result$figures, list()))) {
    status <- "result_invalid"
    error_text <- "analyze(data) must return list(tables = , figures = , notes = )."
  } else {
    tables <- or_empty(result$tables, list())
    figures <- or_empty(result$figures, list())
    notes_out <- as.character(or_empty(result$notes, character()))
    problems <- c(
      if (length(tables) > limits$tables) "too many tables",
      if (length(tables) && !valid_name(names(tables))) "table names must be lower_snake_case and unique",
      if (!all(vapply(tables, is.data.frame, FALSE))) "every table must be a data frame",
      if (any(vapply(tables, function(t) is.data.frame(t) && (nrow(t) > limits$rows || ncol(t) > 100L), FALSE))) "a table is too large",
      if (length(figures) > limits$figures) "too many figures",
      if (length(figures) && !valid_name(names(figures))) "figure names must be lower_snake_case and unique",
      if (!all(vapply(figures, is.function, FALSE))) "every figure must be a function that draws one plot",
      if (length(notes_out) > limits$notes || any(nchar(notes_out) > 500L)) "too many or too long notes"
    )
    if (length(problems)) {
      status <- "result_invalid"
      error_text <- paste(problems, collapse = "; ")
    } else {
      for (name in names(tables)) {
        t <- tables[[name]]
        if (!identical(rownames(t), as.character(seq_len(nrow(t))))) t <- cbind(row_name = rownames(t), t)
        path <- file.path("results", paste0(name, ".csv"))
        utils::write.csv(t, file.path(result_dir, path), row.names = FALSE, na = "")
        tables_out[[length(tables_out) + 1L]] <- list(name = name, file = path, rows = nrow(t), columns = ncol(t))
      }
      # A device that cannot open is the environment's problem, not the block's: the
      # figure keeps the formats that did open.
      draw <- function(open_device, path, fn) {
        before <- grDevices::dev.cur()
        opened <- tryCatch({ open_device(file.path(result_dir, path)); grDevices::dev.cur() != before },
                           error = function(e) FALSE, warning = function(w) FALSE)
        if (!opened) {
          if (grDevices::dev.cur() != before) grDevices::dev.off()
          return(list(drawn = FALSE, error = NA_character_))
        }
        failed <- tryCatch(as_block({ fn(); NA_character_ }), error = function(e) substr(conditionMessage(e), 1L, 300L))
        grDevices::dev.off()
        list(drawn = TRUE, error = failed)
      }
      for (name in names(figures)) {
        pdf_path <- file.path("figures", paste0(name, ".pdf"))
        pdf <- draw(function(f) grDevices::pdf(f, width = 7, height = 5, useDingbats = FALSE), pdf_path, figures[[name]])
        files <- if (pdf$drawn) list(pdf_path) else list()
        figure_error <- if (!pdf$drawn) "the PDF device could not be opened" else pdf$error
        if (is.na(figure_error) && isTRUE(capabilities("cairo"))) {
          svg_path <- file.path("figures", paste0(name, ".svg"))
          svg <- draw(function(f) grDevices::svg(f, width = 7, height = 5), svg_path, figures[[name]])
          if (svg$drawn) { files <- c(files, list(svg_path)); figure_error <- svg$error }
        }
        figures_out[[length(figures_out) + 1L]] <- list(name = name, files = files, error = figure_error)
        if (!is.na(figure_error)) {
          status <- "block_failed"
          error_text <- paste0("Figure ", name, " failed: ", figure_error)
        }
      }
    }
  }
  if (length(warnings_seen)) writeLines(warnings_seen, file.path(result_dir, "warnings.txt"), useBytes = TRUE)
  # One field per line, tab separated; values carry no tab or line break.
  flat <- function(x) gsub("[\t\r\n]", " ", as.character(x))
  writeLines(c(paste0("status\t", status), paste0("error\t", if (is.na(error_text)) "" else flat(error_text)),
               paste0("warnings\t", length(warnings_seen)),
               vapply(tables_out, function(t) paste0("table\t", t$name), ""),
               vapply(figures_out, function(f) paste0("figure\t", f$name), ""),
               vapply(notes_out, function(n) paste0("note\t", flat(n)), "")),
             file.path(result_dir, "report.tsv"), useBytes = TRUE)
}

if (length(args) >= 1L && identical(args[[1L]], "--child")) {
  if (length(args) != 5L) stop("usage: template.R --child <block file> <data file> <result dir> <seed>")
  run_child(args[[2L]], args[[3L]], args[[4L]], as.integer(args[[5L]]))
  quit(status = 0L)
}

# ---- Parent mode from here on: trusted code only.
if (length(args) != 4L) stop("usage: template.R <work dir> <block file> <output dir> <seed>")
work <- normalizePath(args[[1L]], mustWork = TRUE)
block_file <- normalizePath(args[[2L]], mustWork = TRUE)
out <- normalizePath(args[[3L]], mustWork = TRUE)
seed <- suppressWarnings(as.integer(args[[4L]]))
if (is.na(seed)) stop("seed must be an integer")
if (length(list.files(out, all.files = TRUE, no.. = TRUE)) > 0L) stop("output dir must be empty")
local_rerun <- identical(Sys.getenv("ENRICHEDLAB_TEMPLATE_RERUN"), "local")
started <- Sys.time()
for (d in c("code", "input/data", "results", "figures", "logs")) {
  dir.create(file.path(out, d), recursive = TRUE, mode = "0700")
}
log_file <- file.path(out, "logs", "template.log")
note <- function(...) {
  cat(format(Sys.time(), "%Y-%m-%dT%H:%M:%OSZ", tz = "UTC"), " ", ..., "\n",
      sep = "", file = log_file, append = TRUE)
}

# ---- Contract: dataset, column types and limits.
contract <- read.dcf(file.path(work, "contract.dcf"), all = TRUE)
if (nrow(contract) != 1L || !identical(contract$Template, TEMPLATE_REF)) stop("contract does not name this template")
limit <- function(field) as.integer(contract[[field]])
columns <- strsplit(strsplit(contract$Columns, ",[[:space:]]*")[[1L]], ":", fixed = TRUE)
types <- setNames(vapply(columns, `[[`, "", 2L), vapply(columns, `[[`, "", 1L))
if (!all(types %in% c("numeric", "integer", "factor", "character"))) stop("contract column type invalid")
dataset_path <- file.path(work, contract$Dataset)
dataset <- utils::read.csv(dataset_path, colClasses = ifelse(types == "factor", "character", types),
                           check.names = FALSE, na.strings = "")
if (!identical(names(dataset), names(types))) stop("dataset columns differ from the contract")
for (column in names(types)[types == "factor"]) dataset[[column]] <- factor(dataset[[column]])
invisible(file.copy(file.path(work, "contract.dcf"), file.path(out, "input", "contract.dcf")))
invisible(file.copy(dataset_path, file.path(out, "input", "data", basename(dataset_path))))
block_text <- readChar(block_file, file.info(block_file)$size, useBytes = TRUE)
invisible(file.copy(block_file, file.path(out, "code", "block.R")))
template_path <- sub("^--file=", "", grep("^--file=", commandArgs(FALSE), value = TRUE)[1L])
invisible(file.copy(template_path, file.path(out, "code", "template.R")))
note("template ", TEMPLATE_REF, " started; seed ", seed, if (local_rerun) "; local rerun" else "")

# ---- Second admission, on the parsed expressions. The same positive rules as the
# broker: every name is bound by the block, a dataset column, a constant or on the
# allowlist; no call on a computed value; no written numbers as result data.
admit <- function(text, column_names) {
  reasons <- character()
  add <- function(code) reasons <<- union(reasons, code)
  exprs <- tryCatch(parse(text = text, keep.source = FALSE), error = function(e) e)
  if (inherits(exprs, "error")) return(list(result = "rejected", reasons = I("code_parse_error"), exprs = NULL))
  numbers <- 0L
  strings <- 0L
  children <- function(e) {
    items <- as.list(e)
    keep <- vapply(seq_along(items), function(i) !identical(items[[i]], quote(expr = )) && !is.null(items[[i]]), FALSE)
    items[keep]
  }
  # Names the block binds: assignment targets, function parameters and loop variables.
  bound <- character()
  collect <- function(e) {
    if (is.pairlist(e)) { for (item in children(e)) collect(item); return(invisible()) }
    if (!is.call(e)) return(invisible())
    head <- e[[1L]]
    name <- if (is.symbol(head)) as.character(head) else ""
    if (name %in% c("<-", "=", "<<-") && length(e) == 3L) {
      target <- e[[2L]]
      if (is.symbol(target) || is.character(target)) bound <<- union(bound, as.character(target))
    }
    if (name == "function" && !is.null(e[[2L]])) bound <<- union(bound, names(e[[2L]]))
    if (name == "for" && is.symbol(e[[2L]])) bound <<- union(bound, as.character(e[[2L]]))
    for (item in children(e)) collect(item)
    invisible()
  }
  for (x in as.list(exprs)) collect(x)
  known <- function(n) n %in% ALLOWED_NAMES || n %in% bound || n %in% column_names || n %in% CONSTANTS || n == "."
  refuse_name <- function(n) {
    if (n %in% FORBIDDEN) add(paste0("code_forbidden_function_", gsub("[^a-z0-9]", "_", tolower(n))))
    else add("code_name_not_allowed")
  }
  # Inside a formula or a column scope's argument (with, subset, transform), a bare
  # name refers to a data column, possibly one the block added. Called names must be
  # known everywhere.
  visit_leaf <- function(e, columns_ok = FALSE) {
    if (is.symbol(e)) {
      n <- as.character(e)
      if (startsWith(n, ".") && n != ".") add("code_hidden_identifier")
      if (nzchar(n) && !known(n) && !(columns_ok && !(n %in% FORBIDDEN))) refuse_name(n)
    } else if (is.character(e)) {
      strings <<- strings + length(e)
      for (s in e) {
        if (nchar(s) > 120L) add("code_string_too_long")
        if (startsWith(s, "/") || startsWith(s, "~") || grepl("\\", s, fixed = TRUE) ||
            grepl("://", s, fixed = TRUE) || grepl("..", s, fixed = TRUE)) add("code_path_or_url")
        if (trimws(s) %in% FORBIDDEN) add(paste0("code_forbidden_function_", gsub("[^a-z0-9]", "_", tolower(trimws(s)))))
        if (grepl("password|passwd|token|secret|bearer|api_key|apikey|credential|-----begin|akia|ghp_", tolower(s)) ||
            grepl("[A-Za-z0-9+/=_-]{32,}", s)) add("code_secret_like_string")
      }
    } else if (is.numeric(e)) {
      numbers <<- numbers + length(e)
    }
  }
  # What a value carries: numbers written in the code, and whether anything in it
  # refers to data or to a value computed from it.
  summarize <- function(e) {
    if (is.numeric(e)) return(list(numbers = length(e), data = FALSE))
    if (is.symbol(e)) return(list(numbers = 0L, data = !(as.character(e) %in% CONSTANTS)))
    if (!is.call(e)) return(list(numbers = 0L, data = FALSE))
    head <- e[[1L]]
    if (!is.symbol(head)) return(list(numbers = 0L, data = TRUE))
    name <- as.character(head)
    items <- children(e)[-1L]
    if (name %in% c("function", "$", "@", "::", ":::", "for", "while", "repeat")) return(list(numbers = 0L, data = TRUE))
    if (name %in% c("[", "[[")) return(if (length(items)) summarize(items[[1L]]) else list(numbers = 0L, data = FALSE))
    if (name == "{") return(if (length(items)) summarize(items[[length(items)]]) else list(numbers = 0L, data = FALSE))
    if (name %in% c("<-", "=")) return(if (length(items) == 2L) summarize(items[[2L]]) else list(numbers = 0L, data = TRUE))
    parts <- lapply(items, summarize)
    data <- any(vapply(parts, `[[`, FALSE, "data"))
    total <- sum(vapply(parts, `[[`, 0L, "numbers"))
    list(numbers = if (data && !(name %in% c("c", "(", "if")) && !(name %in% SYNTAX)) 0L else total, data = data)
  }
  literal_value <- function(e) {
    s <- summarize(e)
    if (!s$data && s$numbers >= 2L) add("code_literal_data")
  }
  walk <- function(e, columns_ok = FALSE) {
    if (is.pairlist(e)) { for (item in children(e)) walk(item, columns_ok); return(invisible()) }
    if (!is.call(e)) { visit_leaf(e, columns_ok); return(invisible()) }
    head <- e[[1L]]
    if (is.call(head)) {
      qualified <- is.symbol(head[[1L]]) && as.character(head[[1L]]) %in% c("::", ":::")
      if (!qualified) add("code_dynamic_call")
      walk(head, columns_ok)
      for (item in children(e)[-1L]) walk(item, columns_ok)
      return(invisible())
    }
    name <- if (is.symbol(head)) as.character(head) else ""
    if (name %in% c("::", ":::")) {
      if (name == ":::") add("code_internal_namespace")
      if (!(as.character(e[[2L]]) %in% NAMESPACES)) add("code_namespace_not_allowed")
      target <- as.character(e[[3L]])
      if (!(target %in% ALLOWED_NAMES)) refuse_name(target)
      return(invisible())
    }
    if (name == "<<-") add("code_superassignment")
    if (name == "c" && sum(vapply(children(e)[-1L], function(x) if (is.numeric(x)) length(x) else 0L, 0L)) > 16L) add("code_embedded_data")
    if (name %in% c("$", "@")) { walk(e[[2L]], columns_ok); return(invisible()) }
    if (name %in% c("<-", "=") && length(e) == 3L) {
      literal_value(e[[3L]])
      if (!is.symbol(e[[2L]])) walk(e[[2L]], columns_ok)
      walk(e[[3L]], columns_ok)
      return(invisible())
    }
    if (name == "function") {
      if (!is.null(e[[2L]])) for (default in children(e[[2L]])) { literal_value(default); walk(default, columns_ok) }
      walk(e[[3L]], columns_ok)
      return(invisible())
    }
    if (name == "for") { walk(e[[3L]], columns_ok); walk(e[[4L]], columns_ok); return(invisible()) }
    if (nzchar(name) && !(name %in% SYNTAX) && !known(name)) refuse_name(name)
    if (startsWith(name, ".") && name != ".") add("code_hidden_identifier")
    if (name %in% CONSTRUCTORS) {
      arguments <- as.list(e)[-1L]
      argument_names <- or_empty(names(arguments), rep("", length(arguments)))
      for (i in seq_along(arguments)) {
        if (identical(arguments[[i]], quote(expr = )) || is.null(arguments[[i]])) next
        s <- summarize(arguments[[i]])
        if (!s$data && s$numbers > 0L && !(argument_names[[i]] %in% SHAPE_ARGUMENTS)) add("code_literal_data")
      }
    }
    # A named argument's name is not a reference; only its value is walked.
    items <- children(e)[-1L]
    scope <- name %in% c("with", "subset", "transform")
    for (i in seq_along(items)) walk(items[[i]], columns_ok || name == "~" || (scope && i > 1L))
    invisible()
  }
  for (x in as.list(exprs)) walk(x)
  entry <- any(vapply(as.list(exprs), function(x) {
    is.call(x) && as.character(x[[1L]]) %in% c("<-", "=") && identical(x[[2L]], as.name("analyze")) &&
      is.call(x[[3L]]) && identical(x[[3L]][[1L]], as.name("function")) &&
      identical(names(x[[3L]][[2L]])[1L], "data")
  }, FALSE))
  if (!entry) add("code_missing_entry_point")
  if (numbers > 64L) add("code_too_many_numbers")
  if (strings > 64L) add("code_too_many_strings")
  list(result = if (length(reasons)) "rejected" else "accepted", reasons = I(sort(reasons)),
       numeric_literals = numbers, string_literals = strings, exprs = exprs)
}

# ---- Isolation probe, before any block code runs.
probe <- function() {
  connect <- function(host, port) {
    r <- tryCatch({
      con <- suppressWarnings(socketConnection(host, port, blocking = TRUE, open = "r+b", timeout = 3))
      close(con)
      "connected"
    }, error = function(e) "refused", warning = function(w) "refused")
    r
  }
  writable <- function(dir) {
    target <- file.path(dir, paste0(".enrichedlab-probe-", Sys.getpid()))
    ok <- suppressWarnings(tryCatch(file.create(target), error = function(e) FALSE))
    if (isTRUE(ok)) unlink(target)
    isTRUE(ok)
  }
  resolved <- tryCatch(!is.null(suppressWarnings(utils::nsl("example.org"))), error = function(e) FALSE)
  cgroup <- function(path) if (file.exists(path)) trimws(readLines(path, n = 1L, warn = FALSE)) else NA_character_
  checks <- list(
    external_tcp_1_1_1_1_443 = connect("1.1.1.1", 443L),
    cluster_api_10_43_0_1_443 = connect("10.43.0.1", 443L),
    dns_example_org = if (resolved) "resolved" else "unresolved",
    service_account_token = if (file.exists("/var/run/secrets/kubernetes.io/serviceaccount/token")) "present" else "absent",
    input_writable = writable("/input"),
    root_writable = writable("/"),
    tmp_writable = writable("/tmp"),
    home_writable = writable(Sys.getenv("HOME", "/root"))
  )
  enforced <- !local_rerun
  ok <- identical(checks$external_tcp_1_1_1_1_443, "refused") &&
    identical(checks$cluster_api_10_43_0_1_443, "refused") &&
    identical(checks$dns_example_org, "unresolved") &&
    identical(checks$service_account_token, "absent") &&
    !checks$input_writable && !checks$root_writable
  list(schema = "enrichedlab.template-boundary-probe.v1", enforced = enforced, isolated = ok,
       checks = checks,
       limits = list(cpus_visible = parallel::detectCores(),
                     cgroup_cpu_max = cgroup("/sys/fs/cgroup/cpu.max"),
                     cgroup_memory_max = cgroup("/sys/fs/cgroup/memory.max")))
}

# ---- A changed copy of the input. One row goes from each group of at least five
# (groups are combinations of the factor and text columns, leaving out identifier-like
# columns with more distinct values than half the rows), and continuous numeric
# columns get noise of 1 percent of their standard deviation. Coded columns with
# fewer than ten distinct values keep their values, so the block's grouping still works.
changed_input <- function(d, seed) {
  set.seed(seed + 1L)
  grouping <- names(d)[vapply(d, function(x) (is.factor(x) || is.character(x)) &&
                                length(unique(x)) <= nrow(d) / 2, FALSE)]
  groups <- if (length(grouping)) interaction(d[grouping], drop = TRUE) else factor(rep("all", nrow(d)))
  drop <- integer()
  for (g in levels(groups)) {
    rows <- which(groups == g)
    if (length(rows) >= 5L) drop <- c(drop, rows[sample.int(length(rows), 1L)])
  }
  if (!length(drop) && nrow(d) >= 6L) {
    largest <- names(which.max(table(groups)))
    rows <- which(groups == largest)
    if (length(rows) >= 3L) drop <- rows[sample.int(length(rows), 1L)]
  }
  changed <- d[setdiff(seq_len(nrow(d)), drop), , drop = FALSE]
  for (column in names(changed)) {
    x <- changed[[column]]
    if (is.numeric(x) && length(unique(x[!is.na(x)])) >= 10L) {
      spread <- stats::sd(x, na.rm = TRUE)
      if (is.finite(spread) && spread > 0) {
        moved <- x + stats::rnorm(length(x), 0, 0.01 * spread)
        changed[[column]] <- if (is.integer(x)) as.integer(round(moved)) else moved
      }
    }
  }
  rownames(changed) <- NULL
  list(data = changed, rows_removed = length(drop))
}

# ---- Run the block in its own R process on one dataset.
child <- function(label, data, seconds) {
  dir <- file.path(tempdir(), paste0("child-", label))
  dir.create(dir, mode = "0700")
  data_file <- file.path(dir, "data.rds")
  saveRDS(data, data_file)
  saveRDS(list(tables = limit("MaxTables"), rows = limit("MaxTableRows"), figures = limit("MaxFigures"),
               notes = limit("MaxNotes")), file.path(dir, "limits.rds"))
  result_dir <- file.path(dir, "out")
  dir.create(result_dir, mode = "0700")
  log <- file.path(out, "logs", paste0("child_", label, ".txt"))
  code <- suppressWarnings(system2(file.path(R.home("bin"), "Rscript"),
                                   c("--vanilla", shQuote(template_path), "--child", shQuote(block_file),
                                     shQuote(data_file), shQuote(result_dir), as.character(seed)),
                                   stdout = log, stderr = log, timeout = seconds))
  report_file <- file.path(result_dir, "report.tsv")
  if (!file.exists(report_file)) {
    reason <- if (identical(code, 124L)) sprintf("The block did not finish within %d seconds.", seconds) else "The block's R process stopped before it reported."
    return(list(status = "block_failed", error = reason, dir = result_dir, tables = character(), figures = character(),
                notes = character(), warnings = 0L))
  }
  lines <- readLines(report_file, warn = FALSE)
  key <- sub("\t.*$", "", lines)
  value <- sub("^[^\t]*\t?", "", lines)
  first <- function(k) if (any(key == k)) value[key == k][[1L]] else NA_character_
  statuses <- c("succeeded", "block_failed", "result_invalid")
  status <- first("status")
  error <- first("error")
  list(status = if (status %in% statuses) status else "block_failed",
       error = if (is.na(error) || !nzchar(error)) NA_character_ else substr(error, 1L, 500L),
       dir = result_dir, tables = value[key == "table"], figures = value[key == "figure"],
       notes = substr(value[key == "note"], 1L, 500L), warnings = suppressWarnings(as.integer(or_empty(first("warnings"), "0"))))
}

# ---- Compare the tables of the two runs. A table with numbers is input-dependent
# when at least one of its numbers changed with the input.
compare_tables <- function(real_dir, changed_dir, names) {
  lapply(names, function(name) {
    a_path <- file.path(real_dir, "results", paste0(name, ".csv"))
    b_path <- file.path(changed_dir, "results", paste0(name, ".csv"))
    a <- utils::read.csv(a_path, check.names = FALSE)
    numeric_columns <- names(a)[vapply(a, is.numeric, FALSE)]
    cells <- sum(vapply(numeric_columns, function(n) sum(!is.na(a[[n]])), 0L))
    if (!file.exists(b_path)) return(list(name = name, numeric_cells = cells, changed_cells = cells, outcome = "changed"))
    b <- utils::read.csv(b_path, check.names = FALSE)
    changed <- 0L
    unchanged_columns <- character()
    for (n in numeric_columns) {
      if (!(n %in% names(b)) || nrow(a) != nrow(b)) { changed <- changed + sum(!is.na(a[[n]])); next }
      same <- !is.na(a[[n]]) & !is.na(b[[n]]) & a[[n]] == b[[n]]
      differs <- sum(!is.na(a[[n]])) - sum(same)
      changed <- changed + differs
      if (differs == 0L && sum(!is.na(a[[n]])) > 0L) unchanged_columns <- c(unchanged_columns, n)
    }
    outcome <- if (cells == 0L) "no_numbers" else if (changed == 0L) "unchanged" else "changed"
    list(name = name, numeric_cells = cells, changed_cells = changed, unchanged_columns = I(unchanged_columns),
         outcome = outcome)
  })
}

status <- "succeeded"
error_text <- NA_character_
warnings_count <- 0L
tables_out <- list()
figures_out <- list()
notes_out <- character()
dependence <- list(checked = FALSE, outcome = "not_run")

boundary <- probe()
write_json(boundary[c("schema", "enforced", "isolated", "checks", "limits")], file.path(out, "boundary.json"))
note("isolation probe: ", if (boundary$isolated) "isolated" else "NOT isolated", if (boundary$enforced) " (enforced)" else " (recorded only)")
admission <- admit(block_text, names(types))
write_json(list(schema = "enrichedlab.generated-code-admission.v1", checker = "template-ast",
                template_ref = TEMPLATE_REF, block_sha256 = sha256(block_file),
                result = admission$result, reasons = admission$reasons,
                numeric_literals = admission$numeric_literals, string_literals = admission$string_literals,
                allowlist = allowlist_digest(), scientific_validation = "not_performed"),
           file.path(out, "admission.json"))

if (boundary$enforced && !boundary$isolated) {
  status <- "boundary_violation"
  error_text <- "The sandbox isolation probe found network or path access; the block did not run."
} else if (!identical(admission$result, "accepted")) {
  status <- "admission_rejected"
  error_text <- paste("The block failed the template's admission check:", paste(admission$reasons, collapse = ", "))
} else {
  seconds <- limit("TimeLimitSeconds")
  real <- child("input", dataset, seconds)
  status <- or_empty(real$status, "block_failed")
  error_text <- real$error
  warnings_count <- real$warnings
  if (file.exists(file.path(real$dir, "block_output.txt"))) invisible(file.copy(file.path(real$dir, "block_output.txt"), file.path(out, "logs", "block_output.txt")))
  if (file.exists(file.path(real$dir, "warnings.txt"))) invisible(file.copy(file.path(real$dir, "warnings.txt"), file.path(out, "logs", "warnings.txt")))
  valid <- function(x) length(x) == 0L || (all(grepl("^[a-z][a-z0-9_]{0,40}$", x)) && !anyDuplicated(x))
  if (identical(status, "succeeded") && (!valid(real$tables) || !valid(real$figures))) {
    status <- "result_invalid"
    error_text <- "The block's process reported names that are not lower_snake_case."
  }
  if (identical(status, "succeeded") && length(real$tables)) {
    moved <- changed_input(dataset, seed)
    again <- child("changed_input", moved$data, seconds)
    if (file.exists(file.path(again$dir, "block_output.txt"))) invisible(file.copy(file.path(again$dir, "block_output.txt"), file.path(out, "logs", "block_output_changed_input.txt")))
    if (identical(again$status, "succeeded")) {
      tables <- compare_tables(real$dir, again$dir, real$tables)
      unchanged <- vapply(tables, function(t) identical(t$outcome, "unchanged"), FALSE)
      dependence <- list(checked = TRUE, outcome = if (any(unchanged)) "failed" else "passed",
                         rows_removed = moved$rows_removed, noise = "1 percent of the standard deviation on continuous numeric columns",
                         tables = tables)
      if (any(unchanged)) {
        status <- "input_independent"
        error_text <- paste0("These tables did not change when the input changed, so their numbers were not computed from it: ",
                             paste(vapply(tables[unchanged], `[[`, "", "name"), collapse = ", "), ". The results were not kept.")
      }
    } else {
      dependence <- list(checked = FALSE, outcome = "not_checked", rows_removed = moved$rows_removed,
                         reason = "The block did not finish on the changed input, so whether its tables depend on the input is not known.")
    }
  }
  if (identical(status, "succeeded")) {
    for (name in real$tables) {
      path <- file.path("results", paste0(name, ".csv"))
      invisible(file.copy(file.path(real$dir, path), file.path(out, path)))
      t <- utils::read.csv(file.path(out, path), check.names = FALSE)
      tables_out[[length(tables_out) + 1L]] <- list(name = name, file = path, rows = nrow(t), columns = ncol(t))
    }
    for (name in real$figures) {
      files <- list()
      for (ext in c("pdf", "svg")) {
        path <- file.path("figures", paste0(name, ".", ext))
        if (file.exists(file.path(real$dir, path))) { invisible(file.copy(file.path(real$dir, path), file.path(out, path))); files <- c(files, list(path)) }
      }
      figures_out[[length(figures_out) + 1L]] <- list(name = name, files = files, error = NA_character_)
    }
    notes_out <- utils::head(real$notes, limit("MaxNotes"))
  }
}
finished <- Sys.time()
writeLines(capture.output(sessionInfo()), file.path(out, "session_info.txt"))
block_lines <- strsplit(block_text, "\n", fixed = TRUE)[[1L]]
writeLines(c("--- template://r-generated-analysis/v2 replaceable block (empty)",
             "+++ generated/block.R",
             sprintf("@@ -0,0 +1,%d @@", length(block_lines)),
             paste0("+", block_lines)),
           file.path(out, "code", "block.diff"), useBytes = TRUE)
run <- list(
  schema = "enrichedlab.template-run.v1", template_ref = TEMPLATE_REF, status = status,
  validation = "unvalidated", seed = seed, started_at = utc(started), finished_at = utc(finished),
  r_version = R.version.string, local_rerun = local_rerun,
  template_sha256 = sha256(file.path(out, "code", "template.R")), block_sha256 = sha256(block_file),
  evaluation = "separate R process; only allowlisted names", allowlist = allowlist_digest(),
  dataset = list(file = file.path("input", "data", basename(dataset_path)), rows = nrow(dataset),
                 columns = as.list(names(dataset)), sha256 = sha256(dataset_path)),
  input_dependence = dependence,
  tables = tables_out, figures = figures_out, notes = as.list(notes_out),
  warnings = warnings_count, error = error_text
)
write_json(run, file.path(out, "run.json"))
sandbox_line <- if (boundary$enforced && boundary$isolated) {
  "Before the block ran, the probe in `boundary.json` found no network access, no service-account token and a read-only root and input."
} else if (boundary$enforced) {
  "The probe in `boundary.json` found network or path access, so the block did not run."
} else {
  "This run was outside the sandbox; `boundary.json` records the probe without enforcing it."
}
writeLines(c(
  "# Model-written analysis (unvalidated)",
  "",
  sprintf("An R block written by the research model was given to the reviewed template `%s`.", TEMPLATE_REF),
  sprintf("Outcome: %s.", status),
  "",
  "The template supplied the data, opened every plot device and wrote every file. It runs",
  "a block in a separate R process where the block's own code can name only allowlisted R",
  "functions. That narrows what a block can do; the isolation boundary is the sandbox.",
  sandbox_line,
  "The template ran the block again on a changed copy of the input and refuses tables",
  "whose numbers do not change. Code can still compute a value that does not come from",
  "the data, so these results are unvalidated until a scientist reviews the code and the",
  "output.",
  "",
  "- `code/block.R`: the exact model-written block. `code/block.diff` is the only change",
  "  from the reviewed template, `code/template.R`.",
  "- `admission.json`: the template's check of the block before it ran.",
  "- `boundary.json`: the isolation probe the template ran before the block.",
  "- `results/` tables (CSV) and `figures/` plots (PDF, and SVG when available).",
  "- `run.json`: status, seed, R version, digests, the input-dependence check, tables and figures.",
  "- `input/`: the contract and the exact dataset the block received.",
  "- `logs/`: template log, the block's printed output and warnings.",
  "- `SHA256SUMS`: a checksum for every file in this package.",
  "",
  "## Rerun",
  "",
  sprintf("ENRICHEDLAB_TEMPLATE_RERUN=local Rscript --vanilla code/template.R input code/block.R rerun %d", seed),
  "",
  "Create an empty `rerun` directory first. Tables in `rerun/results` should match",
  "`results` exactly; figures are redrawn from the same code and data."
), file.path(out, "README.md"))
note("finished with status ", status)
quit(status = switch(status, succeeded = 0L, block_failed = 2L, result_invalid = 2L, input_independent = 2L,
                     boundary_violation = 3L, admission_rejected = 4L, 1L))
