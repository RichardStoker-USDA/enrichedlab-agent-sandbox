//! Admission for model-written R that fills one block of a reviewed template.
//!
//! This is the first of two checks. It works on tokens, before anything is stored,
//! and admits positively: every name the block refers to must be one it binds itself,
//! a column of the staged dataset or a name on the allowlist below, and no call may
//! be made on a computed value, so a function cannot be assembled from strings or
//! language objects. Numbers written in the code may set parameters of a computation
//! on the data, but may not become result data themselves: none inside a data
//! constructor, and no vector of them assigned to a name. It also refuses `:::`,
//! hidden internals, superassignment, paths, URLs and secret-like strings. The
//! template repeats the check on the parsed expression and evaluates the block in an
//! environment that holds only the allowlisted names, then reruns it on a changed copy
//! of the input and refuses tables that do not change. None of this shows that a
//! result is scientifically valid, and code can still compute a value that does not
//! come from the data; results stay unvalidated until a person reviews them.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const ADMISSION_SCHEMA: &str = "enrichedlab.generated-code-admission.v1";
pub const MAX_BLOCK_BYTES: usize = 16 * 1024;
const MAX_LINES: usize = 400;
const MAX_NUMBERS: usize = 64;
const MAX_NUMBERS_IN_VECTOR: usize = 16;
const MAX_STRINGS: usize = 64;
const MAX_STRING_CHARS: usize = 120;
const MAX_REASONS: usize = 32;
/// At most this many refused names return to the lead with a refusal.
const MAX_REFUSED_NAMES: usize = 12;

/// Functions a block may not reach. The template supplies the data and owns every
/// file, device and path.
const FORBIDDEN: &[&str] = &[
    // processes, shells and native code
    "system",
    "system2",
    "shell",
    "shell.exec",
    "pipe",
    "fifo",
    "dyn.load",
    "dyn.unload",
    "library.dynam",
    "Sys.setenv",
    "Sys.unsetenv",
    "Sys.getenv",
    "Sys.setlocale",
    "Sys.chmod",
    "Sys.umask",
    "Sys.readlink",
    "Sys.info",
    "Sys.getpid",
    "Sys.sleep",
    "Sys.which",
    "R.home",
    "commandArgs",
    "quit",
    "setTimeLimit",
    "setSessionTimeLimit",
    // files, connections and network
    "file",
    "url",
    "gzfile",
    "bzfile",
    "xzfile",
    "unz",
    "gzcon",
    "socketConnection",
    "socketAccept",
    "serverSocket",
    "make.socket",
    "read.socket",
    "write.socket",
    "download.file",
    "curlGetHeaders",
    "readLines",
    "readline",
    "readRDS",
    "load",
    "source",
    "sys.source",
    "scan",
    "read.table",
    "read.csv",
    "read.csv2",
    "read.delim",
    "read.delim2",
    "read.fwf",
    "read.dcf",
    "readBin",
    "readChar",
    "writeLines",
    "writeBin",
    "writeChar",
    "write",
    "write.table",
    "write.csv",
    "write.csv2",
    "saveRDS",
    "save",
    "save.image",
    "sink",
    "capture.output",
    "cat",
    "dput",
    "dget",
    "dump",
    "setwd",
    "getwd",
    "unlink",
    "file.remove",
    "file.rename",
    "file.copy",
    "file.create",
    "file.append",
    "file.symlink",
    "file.link",
    "file.exists",
    "file.info",
    "file.access",
    "file.path",
    "dir.create",
    "dir.exists",
    "list.files",
    "list.dirs",
    "dir",
    "normalizePath",
    "path.expand",
    "tempfile",
    "tempdir",
    "Sys.glob",
    "open",
    "close",
    "textConnection",
    "rawConnection",
    "stdin",
    "stdout",
    "stderr",
    "showConnections",
    "getConnection",
    "closeAllConnections",
    // evaluation and environments
    "eval",
    "evalq",
    "eval.parent",
    "parse",
    "str2lang",
    "str2expression",
    "get",
    "get0",
    "mget",
    "assign",
    "delayedAssign",
    "makeActiveBinding",
    "environment",
    "sys.function",
    "sys.call",
    "sys.frame",
    "sys.frames",
    "sys.calls",
    "parent.frame",
    "parent.env",
    "globalenv",
    "emptyenv",
    "topenv",
    "baseenv",
    "as.environment",
    "list2env",
    "new.env",
    "local",
    "attach",
    "detach",
    "library",
    "require",
    "requireNamespace",
    "loadNamespace",
    "attachNamespace",
    "getNamespace",
    "asNamespace",
    "getExportedValue",
    "do.call",
    "match.fun",
    "body",
    "formals",
    "trace",
    "untrace",
    "browser",
    "debug",
    "debugonce",
    "undebug",
    "options",
    "reg.finalizer",
    "invokeRestart",
    "setClass",
    "setMethod",
    "setGeneric",
    "setRefClass",
    "setValidity",
    "lockBinding",
    "unlockBinding",
    "rm",
    "remove",
    "getFromNamespace",
    "getAnywhere",
    "argsAnywhere",
    "assignInNamespace",
    "assignInMyNamespace",
    "fixInNamespace",
    "getS3method",
    "registerS3method",
    "pos.to.env",
    "sys.on.exit",
    "sys.status",
    "sys.parent",
    "sys.parents",
    "sys.nframe",
    "dump.frames",
    "serialize",
    "unserialize",
    "loadhistory",
    "savehistory",
    // system information, archives, help and the network
    "system.file",
    "readRenviron",
    "Sys.setFileTime",
    "Sys.setlanguage",
    "nsl",
    "socketSelect",
    "zip",
    "unzip",
    "tar",
    "untar",
    "browseURL",
    "help",
    "example",
    "demo",
    "vignette",
    "RSiteSearch",
    "download.packages",
    "file.show",
    "file.edit",
    "edit",
    "fix",
    "View",
    // packages
    "install.packages",
    "remove.packages",
    "update.packages",
    "installed.packages",
    // devices: the template opens and closes every device
    "pdf",
    "svg",
    "png",
    "jpeg",
    "bmp",
    "tiff",
    "postscript",
    "cairo_pdf",
    "cairo_ps",
    "x11",
    "X11",
    "quartz",
    "windows",
    "dev.new",
    "dev.off",
    "dev.set",
    "dev.copy",
    "dev.print",
    "savePlot",
    "graphics.off",
    // building or rewriting calls and functions
    "as.call",
    "as.name",
    "as.symbol",
    "as.function",
    "call",
    "match.call",
    "quote",
    "substitute",
    "Recall",
    "Vectorize",
    "environmentName",
    "rapply",
    "within",
    "curve",
];

const NAMESPACES: &[&str] = &[
    "base",
    "stats",
    "graphics",
    "grDevices",
    "utils",
    "MASS",
    "boot",
    "splines",
    "nlme",
    "mgcv",
    "survival",
    "cluster",
    "lattice",
    "rpart",
    "nnet",
    "Matrix",
    "KernSmooth",
];

const RANDOM: &[&str] = &[
    "set.seed",
    "sample",
    "sample.int",
    "rnorm",
    "runif",
    "rbinom",
    "rpois",
    "rexp",
    "rgamma",
    "rbeta",
    "rt",
    "rchisq",
    "rmultinom",
    "rlnorm",
    "rweibull",
    "rcauchy",
    "rgeom",
    "rhyper",
    "rnbinom",
    "rlogis",
];

/// Names a block may call or refer to, besides the names it binds itself and the
/// columns of the staged dataset. Base R, stats, graphics, grDevices and utils, and a
/// few functions from the recommended packages. Keep in step with `ALLOWED` in the
/// template; both record `allowlist_digest()` so a difference shows in the evidence.
pub const ALLOWED: &[&str] = &[
    "AIC",
    "BIC",
    "F",
    "Filter",
    "Find",
    "Gamma",
    "I",
    "IQR",
    "LETTERS",
    "Map",
    "NCOL",
    "NROW",
    "Negate",
    "Position",
    "Reduce",
    "Surv",
    "T",
    "TukeyHSD",
    "VarCorr",
    "abline",
    "abs",
    "add1",
    "addmargins",
    "adjustcolor",
    "aggregate",
    "agnes",
    "all",
    "all.equal",
    "anova",
    "any",
    "anyNA",
    "aov",
    "aperm",
    "append",
    "apply",
    "approx",
    "array",
    "arrows",
    "as.Date",
    "as.character",
    "as.data.frame",
    "as.double",
    "as.factor",
    "as.formula",
    "as.integer",
    "as.list",
    "as.logical",
    "as.matrix",
    "as.numeric",
    "as.table",
    "as.vector",
    "assocplot",
    "attr",
    "ave",
    "axis",
    "barchart",
    "barplot",
    "bartlett.test",
    "binom.test",
    "binomial",
    "biplot",
    "bkde",
    "boot",
    "boot.ci",
    "box",
    "boxcox",
    "boxplot",
    "boxplot.stats",
    "bquote",
    "bs",
    "bwplot",
    "bxp",
    "by",
    "c",
    "cbind",
    "cdplot",
    "ceiling",
    "character",
    "chisq.test",
    "choose",
    "clara",
    "class",
    "cmdscale",
    "coef",
    "coefficients",
    "col2rgb",
    "colMeans",
    "colSums",
    "colnames",
    "colorRampPalette",
    "combn",
    "complete.cases",
    "conditionMessage",
    "confint",
    "contour",
    "coplot",
    "cor",
    "cor.test",
    "corAR1",
    "corCompSymm",
    "cos",
    "cov",
    "cov2cor",
    "cox.zph",
    "coxph",
    "crossprod",
    "cummax",
    "cummin",
    "cumprod",
    "cumsum",
    "cut",
    "cutree",
    "daisy",
    "data.frame",
    "data.matrix",
    "dbeta",
    "dbinom",
    "dchisq",
    "density",
    "densityplot",
    "det",
    "deviance",
    "dexp",
    "df",
    "df.residual",
    "dgamma",
    "diag",
    "diana",
    "diff",
    "difftime",
    "dim",
    "dimnames",
    "dist",
    "dlnorm",
    "dnorm",
    "dotchart",
    "dotplot",
    "double",
    "dpik",
    "dpois",
    "drop1",
    "droplevels",
    "dt",
    "dunif",
    "duplicated",
    "ecdf",
    "endsWith",
    "exp",
    "expand.grid",
    "expm1",
    "expression",
    "extendrange",
    "factor",
    "factorial",
    "fanny",
    "filled.contour",
    "findInterval",
    "fisher.test",
    "fitdistr",
    "fitted",
    "fitted.values",
    "fivenum",
    "fixef",
    "fligner.test",
    "floor",
    "format",
    "formatC",
    "formula",
    "fourfoldplot",
    "friedman.test",
    "ftable",
    "gam",
    "gamma",
    "gaussian",
    "glm",
    "glm.nb",
    "gls",
    "gray",
    "grep",
    "grepl",
    "grey",
    "grid",
    "gsub",
    "hcl",
    "hcl.colors",
    "hclust",
    "head",
    "heat.colors",
    "hist",
    "histogram",
    "identical",
    "identity",
    "ifelse",
    "image",
    "inherits",
    "integer",
    "integrate",
    "interaction",
    "interaction.plot",
    "intersect",
    "intervals",
    "inverse.gaussian",
    "inverse.rle",
    "invisible",
    "is.character",
    "is.data.frame",
    "is.double",
    "is.element",
    "is.factor",
    "is.finite",
    "is.infinite",
    "is.integer",
    "is.list",
    "is.logical",
    "is.matrix",
    "is.na",
    "is.nan",
    "is.null",
    "is.numeric",
    "is.unsorted",
    "is.vector",
    "isFALSE",
    "isTRUE",
    "jitter",
    "kmeans",
    "kruskal.test",
    "ks.test",
    "lapply",
    "layout",
    "lbeta",
    "lchoose",
    "lda",
    "legend",
    "length",
    "lengths",
    "letters",
    "levelplot",
    "levels",
    "lgamma",
    "lines",
    "list",
    "lm",
    "lme",
    "locpoly",
    "loess",
    "log",
    "log10",
    "log1p",
    "log2",
    "logLik",
    "logical",
    "lowess",
    "mad",
    "mahalanobis",
    "make.names",
    "make.unique",
    "mantelhaen.test",
    "mapply",
    "margin.table",
    "match",
    "matlines",
    "matplot",
    "matpoints",
    "matrix",
    "max",
    "mcnemar.test",
    "mean",
    "median",
    "merge",
    "message",
    "min",
    "missing",
    "model.frame",
    "model.matrix",
    "model.response",
    "model.tables",
    "modifyList",
    "month.abb",
    "month.name",
    "mosaicplot",
    "mtext",
    "multinom",
    "mvrnorm",
    "na.exclude",
    "na.fail",
    "na.omit",
    "na.pass",
    "names",
    "nargs",
    "nchar",
    "ncol",
    "nlevels",
    "nls",
    "nobs",
    "noquote",
    "nrow",
    "ns",
    "numeric",
    "offset",
    "on.exit",
    "oneway.test",
    "optim",
    "optimize",
    "order",
    "outer",
    "p.adjust",
    "pairs",
    "pairwise.prop.test",
    "pairwise.t.test",
    "pairwise.wilcox.test",
    "palette.colors",
    "pam",
    "par",
    "paste",
    "paste0",
    "pbeta",
    "pbinom",
    "pchisq",
    "persp",
    "pexp",
    "pf",
    "pgamma",
    "pi",
    "pie",
    "plnorm",
    "plot",
    "plot.new",
    "plot.window",
    "pmax",
    "pmin",
    "pnorm",
    "points",
    "poisson",
    "poisson.test",
    "polr",
    "poly",
    "polygon",
    "power.prop.test",
    "power.t.test",
    "ppoints",
    "ppois",
    "prcomp",
    "predict",
    "pretty",
    "prettyNum",
    "princomp",
    "print",
    "prod",
    "prop.table",
    "prop.test",
    "proportions",
    "prune",
    "pt",
    "ptukey",
    "punif",
    "qbeta",
    "qbinom",
    "qchisq",
    "qda",
    "qexp",
    "qf",
    "qgamma",
    "qlnorm",
    "qnorm",
    "qpois",
    "qqline",
    "qqnorm",
    "qqplot",
    "qt",
    "qtukey",
    "quantile",
    "quasi",
    "quasibinomial",
    "quasipoisson",
    "qunif",
    "rainbow",
    "ranef",
    "range",
    "rank",
    "rbeta",
    "rbind",
    "rbinom",
    "rcauchy",
    "rchisq",
    "rect",
    "reformulate",
    "regexpr",
    "regmatches",
    "relevel",
    "reorder",
    "rep",
    "rep_len",
    "replace",
    "resid",
    "residuals",
    "return",
    "rev",
    "rexp",
    "rgamma",
    "rgb",
    "rgeom",
    "rhyper",
    "rle",
    "rlm",
    "rlnorm",
    "rlogis",
    "rmultinom",
    "rnbinom",
    "rnorm",
    "round",
    "row.names",
    "rowMeans",
    "rowSums",
    "rownames",
    "rowsum",
    "rpart",
    "rpois",
    "rt",
    "rug",
    "runif",
    "runmed",
    "rweibull",
    "s",
    "sample",
    "sample.int",
    "sapply",
    "scale",
    "sd",
    "segments",
    "seq",
    "seq.int",
    "seq_along",
    "seq_len",
    "set.seed",
    "setNames",
    "setdiff",
    "shapiro.test",
    "sigma",
    "sign",
    "signif",
    "silhouette",
    "sin",
    "smooth.spline",
    "smoothScatter",
    "solve",
    "sort",
    "spineplot",
    "spline",
    "split",
    "sprintf",
    "sqrt",
    "stack",
    "startsWith",
    "step",
    "stepAIC",
    "stop",
    "stopifnot",
    "str",
    "strata",
    "stripchart",
    "stripplot",
    "strsplit",
    "structure",
    "sub",
    "subset",
    "substr",
    "substring",
    "sum",
    "summary",
    "sunflowerplot",
    "suppressMessages",
    "suppressWarnings",
    "survdiff",
    "survfit",
    "sweep",
    "switch",
    "symbols",
    "t",
    "t.test",
    "table",
    "tabulate",
    "tail",
    "tan",
    "tapply",
    "te",
    "terms",
    "terrain.colors",
    "text",
    "ti",
    "title",
    "toString",
    "tolower",
    "topo.colors",
    "toupper",
    "transform",
    "trimws",
    "trunc",
    "tryCatch",
    "typeof",
    "unclass",
    "union",
    "unique",
    "uniroot",
    "unlist",
    "unname",
    "unsplit",
    "unstack",
    "update",
    "vapply",
    "var",
    "var.test",
    "varIdent",
    "vcov",
    "vector",
    "warning",
    "weighted.mean",
    "weights",
    "which",
    "which.max",
    "which.min",
    "wilcox.test",
    "with",
    "withCallingHandlers",
    "xor",
    "xtabs",
    "xyplot",
];

/// Names that are values written in the code, not references to anything.
const CONSTANTS: &[&str] = &[
    "TRUE",
    "FALSE",
    "T",
    "F",
    "NULL",
    "NA",
    "NA_integer_",
    "NA_real_",
    "NA_character_",
    "Inf",
    "NaN",
    "pi",
    "LETTERS",
    "letters",
    "month.abb",
    "month.name",
];

const KEYWORDS: &[&str] = &[
    "function", "if", "else", "for", "in", "while", "repeat", "break", "next",
];

/// Calls that build data from their arguments. A number written straight into one
/// becomes result data the block invented, unless it only sets the result's shape.
const CONSTRUCTORS: &[&str] = &[
    "data.frame",
    "list",
    "matrix",
    "array",
    "cbind",
    "rbind",
    "structure",
    "setNames",
    "expand.grid",
    "transform",
    "append",
    "replace",
    "table",
    "as.data.frame",
    "as.matrix",
];

/// Arguments of a constructor that set the shape of the result, not its values.
const SHAPE_ARGUMENTS: &[&str] = &[
    "nrow",
    "ncol",
    "byrow",
    "dim",
    "stringsAsFactors",
    "check.names",
    "deparse.level",
    "make.row.names",
    "fix.empty.names",
    "KEEP.OUT.ATTRS",
    "row.names",
    "responseName",
    "useNA",
    "dnn",
    "after",
    "exclude",
];

/// SHA-256 over the allowlist, one name per line in byte order. Recorded with every
/// admission so the list that decided it can be compared with the template's.
pub fn allowlist_digest() -> String {
    let mut names: Vec<&str> = ALLOWED.to_vec();
    names.sort_unstable();
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(names.join("\n").as_bytes()))
    )
}

const SECRET_MARKERS: &[&str] = &[
    "-----begin",
    "akia",
    "ghp_",
    "gho_",
    "xoxb",
    "xoxp",
    "password",
    "passwd",
    "token",
    "secret",
    "bearer",
    "api_key",
    "apikey",
    "private key",
    "credential",
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Ident(String),
    Number,
    Str(String),
    Op(String),
    /// A line break; it ends a statement at the top level or inside braces.
    Newline,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AdmissionReport {
    pub schema: &'static str,
    pub result: &'static str,
    pub template_ref: String,
    pub block_sha256: String,
    pub size_bytes: usize,
    pub lines: usize,
    pub reasons: Vec<String>,
    pub functions_used: Vec<String>,
    pub namespaces: Vec<String>,
    pub numeric_literals: usize,
    pub string_literals: usize,
    pub uses_random_numbers: bool,
    pub allowlist: String,
    pub scientific_validation: &'static str,
    /// Names the block called that admission refused, for the lead's next attempt only.
    /// Never serialized, logged or recorded; `refused_name` bounds each one.
    #[serde(skip)]
    pub refused_names: Vec<String>,
}
impl AdmissionReport {
    pub fn accepted(&self) -> bool {
        self.result == "accepted"
    }
    /// A fixed code safe for logs and run events.
    pub fn first_reason(&self) -> &str {
        self.reasons
            .first()
            .map(String::as_str)
            .unwrap_or("code_rejected")
    }
}

/// A static code for run events: the admission reason without a function name.
pub fn event_code(reason: &str) -> &'static str {
    const CODES: &[&str] = &[
        "code_size_invalid",
        "code_too_many_lines",
        "code_control_character",
        "code_string_multiline",
        "code_unterminated_string",
        "code_backtick_name",
        "code_raw_string",
        "code_operator_invalid",
        "code_non_ascii_outside_strings",
        "code_hidden_identifier",
        "code_namespace_not_allowed",
        "code_internal_namespace",
        "code_superassignment",
        "code_unbalanced",
        "code_missing_entry_point",
        "code_too_many_numbers",
        "code_too_many_strings",
        "code_embedded_data",
        "code_string_too_long",
        "code_path_or_url",
        "code_secret_like_string",
        "code_name_not_allowed",
        "code_dynamic_call",
        "code_literal_data",
        "code_forbidden_function",
    ];
    if reason.starts_with("code_forbidden_function_") {
        return "code_forbidden_function";
    }
    CODES
        .iter()
        .find(|code| **code == reason)
        .copied()
        .unwrap_or("code_rejected")
}

pub fn digest(block: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(block.as_bytes())))
}

/// Admit or reject one block. `columns` are the staged dataset's column names from
/// the profile's contract; a block may refer to them in formulas and the like.
/// Reasons are fixed codes, optionally suffixed with a name from the fixed forbidden
/// list; no text from the block enters them.
pub fn admit(block: &str, template_ref: &str, columns: &[String]) -> AdmissionReport {
    let mut reasons = BTreeSet::new();
    let mut report = AdmissionReport {
        schema: ADMISSION_SCHEMA,
        result: "rejected",
        template_ref: template_ref.to_owned(),
        block_sha256: digest(block),
        size_bytes: block.len(),
        lines: block.lines().count(),
        reasons: vec![],
        functions_used: vec![],
        namespaces: vec![],
        numeric_literals: 0,
        string_literals: 0,
        uses_random_numbers: false,
        allowlist: allowlist_digest(),
        scientific_validation: "not_performed",
        refused_names: vec![],
    };
    if block.is_empty() || block.len() > MAX_BLOCK_BYTES {
        reasons.insert("code_size_invalid".to_owned());
    }
    if report.lines > MAX_LINES {
        reasons.insert("code_too_many_lines".to_owned());
    }
    if block
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        reasons.insert("code_control_character".to_owned());
    }
    let tokens = match tokenize(block) {
        Ok(tokens) => tokens,
        Err(code) => {
            reasons.insert(code.to_owned());
            return finish(report, reasons);
        }
    };
    let plain: Vec<Token> = tokens
        .iter()
        .filter(|t| !matches!(t, Token::Newline))
        .cloned()
        .collect();
    let mut refused = BTreeSet::new();
    check(&plain, &mut report, &mut reasons, &mut refused);
    let columns: BTreeSet<String> = columns.iter().cloned().collect();
    check_names_and_literals(&tokens, &columns, &mut reasons, &mut refused);
    report.refused_names = refused
        .into_iter()
        .filter(|name| refused_name(name))
        .take(MAX_REFUSED_NAMES)
        .collect();
    finish(report, reasons)
}

/// A refused name goes back to the lead only when it is a plain R name of bounded length.
fn refused_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 48
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '.')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

fn finish(mut report: AdmissionReport, reasons: BTreeSet<String>) -> AdmissionReport {
    report.reasons = reasons.into_iter().take(MAX_REASONS).collect();
    if report.reasons.is_empty() {
        report.result = "accepted";
    }
    report
}

fn tokenize(source: &str) -> Result<Vec<Token>, &'static str> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' => i += 1,
            b'\n' => {
                tokens.push(Token::Newline);
                i += 1;
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let quote = b;
                let start = i + 1;
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        c if c == quote => {
                            closed = true;
                            break;
                        }
                        b'\n' => return Err("code_string_multiline"),
                        _ => i += 1,
                    }
                }
                if !closed || i > bytes.len() {
                    return Err("code_unterminated_string");
                }
                tokens.push(Token::Str(source[start..i].to_owned()));
                i += 1;
            }
            b'`' => return Err("code_backtick_name"),
            b'0'..=b'9' => i = number(bytes, i, &mut tokens),
            b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i = number(bytes, i, &mut tokens)
            }
            c if c.is_ascii_alphabetic() || c == b'.' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
                {
                    i += 1;
                }
                let name = &source[start..i];
                if matches!(name, "r" | "R") && matches!(bytes.get(i), Some(b'"' | b'\'')) {
                    return Err("code_raw_string");
                }
                tokens.push(Token::Ident(name.to_owned()));
            }
            b'%' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'%' && bytes[i] != b'\n' {
                    i += 1;
                }
                if i >= bytes.len() || bytes[i] != b'%' {
                    return Err("code_operator_invalid");
                }
                i += 1;
                tokens.push(Token::Op(source[start..i].to_owned()));
            }
            b'\\' if bytes.get(i + 1) == Some(&b'(') => {
                tokens.push(Token::Op("\\".into()));
                i += 1;
            }
            c if c.is_ascii() => {
                let rest = &source[i..];
                let op = [
                    ":::", "<<-", "->>", "::", "<-", "->", "<=", ">=", "==", "!=", "&&", "||", "|>",
                ]
                .into_iter()
                .find(|op| rest.starts_with(op))
                .unwrap_or(&rest[..1]);
                if !"+-*/^<>=!&|~$@:,;()[]{}".contains(op.chars().next().unwrap_or(' '))
                    && op.len() == 1
                {
                    return Err("code_operator_invalid");
                }
                tokens.push(Token::Op(op.to_owned()));
                i += op.len();
            }
            _ => return Err("code_non_ascii_outside_strings"),
        }
    }
    Ok(tokens)
}

fn number(bytes: &[u8], mut i: usize, tokens: &mut Vec<Token>) -> usize {
    if bytes[i] == b'0' && matches!(bytes.get(i + 1), Some(b'x' | b'X')) {
        i += 2;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
    } else {
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    if matches!(bytes.get(i), Some(b'L' | b'i')) {
        i += 1;
    }
    tokens.push(Token::Number);
    i
}

fn check(
    tokens: &[Token],
    report: &mut AdmissionReport,
    reasons: &mut BTreeSet<String>,
    refused: &mut BTreeSet<String>,
) {
    let forbidden: BTreeSet<&str> = FORBIDDEN.iter().copied().collect();
    let mut functions = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    let mut depth: i64 = 0;
    // Paren depth of each open `c(`; the numbers counted inside it.
    let mut vectors: Vec<(i64, usize)> = vec![];
    let mut entry_point = false;
    for (index, token) in tokens.iter().enumerate() {
        let previous = index.checked_sub(1).map(|p| &tokens[p]);
        let next = tokens.get(index + 1);
        match token {
            Token::Ident(name) => {
                let member = matches!(previous, Some(Token::Op(op)) if op == "$" || op == "@");
                let qualified = matches!(previous, Some(Token::Op(op)) if op == "::");
                // An assignment target or argument name is not a reference.
                let named_argument = matches!(next, Some(Token::Op(op)) if op == "=" || op == "<-");
                if name.starts_with('.') && name != "." && !member {
                    reasons.insert("code_hidden_identifier".into());
                }
                if !member && !named_argument && forbidden.contains(name.as_str()) {
                    reasons.insert(format!("code_forbidden_function_{}", code_name(name)));
                    refused.insert(name.clone());
                }
                if matches!(next, Some(Token::Op(op)) if op == "::" || op == ":::") {
                    namespaces.insert(name.clone());
                    if !NAMESPACES.contains(&name.as_str()) {
                        reasons.insert("code_namespace_not_allowed".into());
                    }
                }
                let keyword = matches!(
                    name.as_str(),
                    "function" | "if" | "for" | "while" | "repeat"
                );
                if !member && !keyword && matches!(next, Some(Token::Op(op)) if op == "(") {
                    functions.insert(name.clone());
                    if RANDOM.contains(&name.as_str()) {
                        report.uses_random_numbers = true;
                    }
                    if name == "c" {
                        vectors.push((depth + 1, 0));
                    }
                }
                if name == "analyze"
                    && !qualified
                    && matches!(next, Some(Token::Op(op)) if op == "<-" || op == "=")
                    && matches!(tokens.get(index + 2), Some(Token::Ident(f)) if f == "function")
                    && matches!(tokens.get(index + 3), Some(Token::Op(op)) if op == "(")
                    && matches!(tokens.get(index + 4), Some(Token::Ident(d)) if d == "data")
                {
                    entry_point = true;
                }
            }
            Token::Number => {
                report.numeric_literals += 1;
                if let Some(open) = vectors.last_mut() {
                    open.1 += 1;
                    if open.1 > MAX_NUMBERS_IN_VECTOR {
                        reasons.insert("code_embedded_data".into());
                    }
                }
            }
            Token::Str(text) => {
                report.string_literals += 1;
                check_string(text, &forbidden, reasons, refused);
            }
            Token::Op(op) => match op.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    if vectors.last().is_some_and(|(open, _)| *open == depth) && op == ")" {
                        vectors.pop();
                    }
                    depth -= 1;
                    if depth < 0 {
                        reasons.insert("code_unbalanced".into());
                    }
                }
                ":::" => {
                    reasons.insert("code_internal_namespace".into());
                }
                "<<-" | "->>" => {
                    reasons.insert("code_superassignment".into());
                }
                _ => {}
            },
            Token::Newline => {}
        }
    }
    if depth != 0 {
        reasons.insert("code_unbalanced".into());
    }
    if !entry_point {
        reasons.insert("code_missing_entry_point".into());
    }
    if report.numeric_literals > MAX_NUMBERS {
        reasons.insert("code_too_many_numbers".into());
    }
    if report.string_literals > MAX_STRINGS {
        reasons.insert("code_too_many_strings".into());
    }
    report.functions_used = functions.into_iter().take(200).collect();
    report.namespaces = namespaces.into_iter().collect();
}

fn check_string(
    text: &str,
    forbidden: &BTreeSet<&str>,
    reasons: &mut BTreeSet<String>,
    refused: &mut BTreeSet<String>,
) {
    if text.chars().count() > MAX_STRING_CHARS {
        reasons.insert("code_string_too_long".into());
    }
    let lower = text.to_ascii_lowercase();
    if text.starts_with('/')
        || text.starts_with('~')
        || text.contains('\\')
        || text.contains("://")
        || text.contains("..")
    {
        reasons.insert("code_path_or_url".into());
    }
    if SECRET_MARKERS.iter().any(|m| lower.contains(m))
        || text
            .split(|c: char| !(c.is_ascii_alphanumeric() || "+/=_-".contains(c)))
            .any(|run| run.len() >= 32)
    {
        reasons.insert("code_secret_like_string".into());
    }
    // `match.fun` resolves strings, so a string may not name a forbidden function.
    if forbidden.contains(text.trim()) {
        reasons.insert(format!(
            "code_forbidden_function_{}",
            code_name(text.trim())
        ));
        refused.insert(text.trim().to_owned());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Top,
    Brace,
    Call,
    /// The parameter list of `function`.
    Function,
    /// The parenthesised head of `if`, `for` or `while`.
    Condition,
    Group,
    Index,
}

/// What one argument or statement holds: numbers written in the code, and whether
/// anything in it refers to data or to a value computed from it. Unknown names wait
/// in `pending` until the part is known to be a formula, where they name columns.
#[derive(Clone, Debug, Default)]
struct Part {
    numbers: usize,
    data: bool,
    name: Option<String>,
    tokens: usize,
    pending: usize,
    formula: bool,
}

/// Calls whose arguments after the first are evaluated among the data's columns.
const COLUMN_SCOPES: &[&str] = &["with", "subset", "transform"];

struct Frame {
    kind: Kind,
    name: String,
    parts: Vec<Part>,
    current: Part,
    /// Statement contexts: the current statement assigns its value.
    assignment: bool,
    /// Statement contexts: the last finished statement, whose value the braces take.
    last: Part,
    /// Names here refer to data columns: inside a formula or a column scope's argument.
    lenient: bool,
}

impl Frame {
    fn new(kind: Kind, name: &str) -> Self {
        Frame {
            kind,
            name: name.to_owned(),
            parts: vec![],
            current: Part::default(),
            assignment: false,
            last: Part::default(),
            lenient: false,
        }
    }
    /// True when the current argument is evaluated among the data's columns.
    fn column_scope(&self) -> bool {
        self.lenient
            || self.current.formula
            || (self.kind == Kind::Call
                && COLUMN_SCOPES.contains(&self.name.as_str())
                && !self.parts.is_empty())
    }
    fn statements(&self) -> bool {
        matches!(self.kind, Kind::Top | Kind::Brace)
    }
}

fn neighbours(tokens: &[Token], index: usize) -> (Option<&Token>, Option<&Token>) {
    let previous = tokens[..index]
        .iter()
        .rev()
        .find(|t| !matches!(t, Token::Newline));
    let next = tokens[index + 1..]
        .iter()
        .find(|t| !matches!(t, Token::Newline));
    (previous, next)
}

fn is_op(token: Option<&Token>, ops: &[&str]) -> bool {
    matches!(token, Some(Token::Op(op)) if ops.contains(&op.as_str()))
}

/// The kind of a `(` from the token before it and the frame that token closed.
/// The second value is true for a call on a computed value.
fn opened(previous: Option<&Token>, closed: Option<Kind>, new_statement: bool) -> (Kind, bool) {
    if new_statement {
        return (Kind::Group, false);
    }
    match previous {
        Some(Token::Ident(n)) if n == "function" => (Kind::Function, false),
        Some(Token::Ident(n)) if matches!(n.as_str(), "if" | "for" | "while") => {
            (Kind::Condition, false)
        }
        Some(Token::Ident(n)) if KEYWORDS.contains(&n.as_str()) => (Kind::Group, false),
        Some(Token::Ident(_)) | Some(Token::Str(_)) => (Kind::Call, false),
        Some(Token::Op(op)) if op == ")" => match closed {
            Some(Kind::Function | Kind::Condition) => (Kind::Group, false),
            _ => (Kind::Call, true),
        },
        Some(Token::Op(op)) if op == "]" || op == "}" => (Kind::Call, true),
        _ => (Kind::Group, false),
    }
}

/// Names the block binds itself: assignment targets, function parameters and loop
/// variables. Scope is the whole block; each is still a value the block made.
fn bound_names(tokens: &[Token]) -> BTreeSet<String> {
    let mut bound = BTreeSet::new();
    let mut stack = vec![Kind::Top];
    let mut closed: Option<Kind> = None;
    for (index, token) in tokens.iter().enumerate() {
        let (previous, next) = neighbours(tokens, index);
        let last_closed = if matches!(token, Token::Newline) {
            closed
        } else {
            closed.take()
        };
        match token {
            Token::Ident(name) | Token::Str(name) => {
                let inner = *stack.last().unwrap_or(&Kind::Top);
                // `x$name <- value` adds a column, not a name the block can use bare.
                let member = is_op(previous, &["$", "@"]);
                let assigns = !member
                    && (is_op(next, &["<-"])
                        || (is_op(next, &["="])
                            && matches!(inner, Kind::Top | Kind::Brace | Kind::Group)));
                let parameter = inner == Kind::Function
                    && is_op(next, &[",", ")", "="])
                    && is_op(previous, &["(", ","]);
                let target = is_op(previous, &["->"]);
                let looped = inner == Kind::Condition
                    && is_op(previous, &["("])
                    && matches!(next, Some(Token::Ident(w)) if w == "in");
                if assigns || parameter || target || looped {
                    bound.insert(name.clone());
                }
            }
            Token::Op(op) => match op.as_str() {
                "(" => stack.push(opened(previous, last_closed, false).0),
                "[" => stack.push(Kind::Index),
                "{" => stack.push(Kind::Brace),
                ")" | "]" | "}" => {
                    if stack.len() > 1 {
                        closed = stack.pop();
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    bound
}

fn literal_value(part: &Part, reasons: &mut BTreeSet<String>) {
    if !part.data && part.numbers >= 2 {
        reasons.insert("code_literal_data".into());
    }
}

/// Unknown names are refused unless the part turned out to be a formula or a column
/// scope; there they name columns, possibly ones the block added.
fn settle(part: &Part, lenient: bool, reasons: &mut BTreeSet<String>) {
    if part.pending > 0 && !part.formula && !lenient {
        reasons.insert("code_name_not_allowed".into());
    }
}

fn end_statement(frame: &mut Frame, reasons: &mut BTreeSet<String>) {
    let part = std::mem::take(&mut frame.current);
    settle(&part, frame.lenient, reasons);
    if frame.assignment {
        literal_value(&part, reasons);
    }
    frame.assignment = false;
    if part.tokens > 0 {
        frame.last = part;
    }
}

/// Close a frame and return what its value carries into the enclosing part.
fn close(frame: &mut Frame, reasons: &mut BTreeSet<String>) -> Option<Part> {
    match frame.kind {
        Kind::Top | Kind::Brace => {
            end_statement(frame, reasons);
            Some(frame.last.clone())
        }
        Kind::Group => {
            let mut part = std::mem::take(&mut frame.current);
            if frame.assignment {
                literal_value(&part, reasons);
            }
            if part.formula || frame.lenient {
                part.pending = 0;
            }
            Some(part)
        }
        Kind::Index | Kind::Condition => {
            let current = std::mem::take(&mut frame.current);
            let pending = frame.parts.iter().map(|p| p.pending).sum::<usize>() + current.pending;
            Some(Part {
                pending: if frame.lenient || current.formula {
                    0
                } else {
                    pending
                },
                ..Part::default()
            })
        }
        Kind::Function => {
            let current = std::mem::take(&mut frame.current);
            if current.tokens > 0 {
                frame.parts.push(current);
            }
            // A vector written as a default value is data the caller never passed.
            for part in &frame.parts {
                literal_value(part, reasons);
                settle(part, frame.lenient, reasons);
            }
            None
        }
        Kind::Call => {
            let current = std::mem::take(&mut frame.current);
            if current.tokens > 0 || !frame.parts.is_empty() {
                frame.parts.push(current);
            }
            let scope = COLUMN_SCOPES.contains(&frame.name.as_str());
            let pending: usize = frame
                .parts
                .iter()
                .enumerate()
                .filter(|(i, p)| !(p.formula || frame.lenient || (scope && *i > 0)))
                .map(|(_, p)| p.pending)
                .sum();
            let data = frame.parts.iter().any(|p| p.data);
            if CONSTRUCTORS.contains(&frame.name.as_str()) {
                for part in &frame.parts {
                    let shape = part
                        .name
                        .as_deref()
                        .is_some_and(|n| SHAPE_ARGUMENTS.contains(&n));
                    if !part.data && part.numbers > 0 && !shape {
                        reasons.insert("code_literal_data".into());
                    }
                }
            }
            // A call on the data is computed; written numbers are its parameters. A
            // call on written values only is itself written data.
            Some(Part {
                numbers: if data {
                    0
                } else {
                    frame.parts.iter().map(|p| p.numbers).sum()
                },
                data,
                name: None,
                tokens: 1,
                pending,
                formula: false,
            })
        }
    }
}

/// True when a line break does not end the statement: the line ends in an operator
/// or a separator that needs more.
fn continues(previous: Option<&Token>) -> bool {
    match previous {
        Some(Token::Op(op)) => !matches!(op.as_str(), ")" | "]" | "}"),
        Some(Token::Ident(word)) => word == "else",
        _ => false,
    }
}

/// Positive admission. Every name the block refers to is one it binds, a column of
/// the staged dataset or an allowlisted name; no call is made on a computed value;
/// and no number written in the code becomes result data.
fn check_names_and_literals(
    tokens: &[Token],
    columns: &BTreeSet<String>,
    reasons: &mut BTreeSet<String>,
    refused: &mut BTreeSet<String>,
) {
    let allowed: BTreeSet<&str> = ALLOWED.iter().copied().collect();
    let forbidden: BTreeSet<&str> = FORBIDDEN.iter().copied().collect();
    let bound = bound_names(tokens);
    let mut stack = vec![Frame::new(Kind::Top, "")];
    let mut closed: Option<Kind> = None;
    for (index, token) in tokens.iter().enumerate() {
        let (previous, next) = neighbours(tokens, index);
        let last_closed = if matches!(token, Token::Newline) {
            closed
        } else {
            closed.take()
        };
        let newline_before = index > 0 && matches!(tokens[index - 1], Token::Newline);
        let Some(frame) = stack.last_mut() else {
            return;
        };
        match token {
            Token::Newline => {
                if frame.statements() && !continues(previous) {
                    end_statement(frame, reasons);
                }
            }
            Token::Number => {
                frame.current.numbers += 1;
                frame.current.tokens += 1;
            }
            Token::Str(_) => frame.current.tokens += 1,
            Token::Ident(name) => {
                frame.current.tokens += 1;
                if is_op(previous, &["$", "@"]) {
                    frame.current.data = true;
                    continue;
                }
                if is_op(next, &["::", ":::"]) {
                    continue;
                }
                if KEYWORDS.contains(&name.as_str()) {
                    if name == "function" {
                        frame.current.data = true;
                    }
                    continue;
                }
                if is_op(next, &["="])
                    && matches!(frame.kind, Kind::Call | Kind::Function | Kind::Index)
                {
                    frame.current.name = Some(name.clone());
                    continue;
                }
                if CONSTANTS.contains(&name.as_str()) {
                    continue;
                }
                let qualified = is_op(previous, &["::", ":::"]);
                let known = allowed.contains(name.as_str())
                    || (!qualified
                        && (bound.contains(name) || columns.contains(name) || name == "."));
                let call = is_op(next, &["("]);
                if !known && !forbidden.contains(name.as_str()) {
                    // A called name must be known everywhere; a bare one may be a
                    // column the block added, used in a formula or a column scope.
                    if call || qualified {
                        reasons.insert("code_name_not_allowed".into());
                        refused.insert(name.clone());
                    } else {
                        frame.current.pending += 1;
                    }
                }
                if !call {
                    frame.current.data = true;
                }
            }
            Token::Op(op) => match op.as_str() {
                "(" => {
                    let statement_start = newline_before && frame.statements();
                    let (kind, dynamic) = opened(previous, last_closed, statement_start);
                    let before = index
                        .checked_sub(1)
                        .map(|i| neighbours(tokens, i).0)
                        .unwrap_or(None);
                    let member_call = kind == Kind::Call
                        && matches!(previous, Some(Token::Ident(_)))
                        && is_op(before, &["$", "@"]);
                    if dynamic || member_call {
                        reasons.insert("code_dynamic_call".into());
                    }
                    let name = match previous {
                        Some(Token::Ident(n)) if kind == Kind::Call => n.clone(),
                        Some(Token::Str(n)) if kind == Kind::Call => {
                            // R calls a function named by a string in call position.
                            if !(allowed.contains(n.as_str()) || bound.contains(n)) {
                                reasons.insert("code_name_not_allowed".into());
                                refused.insert(n.clone());
                            }
                            n.clone()
                        }
                        _ => String::new(),
                    };
                    let lenient = frame.column_scope();
                    let mut opened_frame = Frame::new(kind, &name);
                    opened_frame.lenient = lenient;
                    stack.push(opened_frame);
                }
                "[" | "{" => {
                    let lenient = frame.column_scope();
                    let kind = if op == "[" { Kind::Index } else { Kind::Brace };
                    let mut opened_frame = Frame::new(kind, "");
                    opened_frame.lenient = lenient;
                    stack.push(opened_frame);
                }
                "~" => {
                    frame.current.formula = true;
                    frame.current.tokens += 1;
                }
                "," => {
                    let part = std::mem::take(&mut frame.current);
                    frame.parts.push(part);
                }
                ")" | "]" | "}" => {
                    if stack.len() == 1 {
                        continue;
                    }
                    let mut done = stack.pop().unwrap_or_else(|| Frame::new(Kind::Group, ""));
                    closed = Some(done.kind);
                    let summary = close(&mut done, reasons);
                    if let Some(parent) = stack.last_mut() {
                        parent.current.tokens += 1;
                        if let Some(value) = summary {
                            parent.current.numbers += value.numbers;
                            parent.current.data |= value.data;
                            parent.current.pending += value.pending;
                        }
                    }
                }
                "<-" | "=" if matches!(frame.kind, Kind::Top | Kind::Brace | Kind::Group) => {
                    frame.assignment = true;
                    let pending = frame.current.pending;
                    frame.current = Part::default();
                    frame.current.pending = pending;
                }
                "->" if matches!(frame.kind, Kind::Top | Kind::Brace | Kind::Group) => {
                    literal_value(&frame.current, reasons);
                    frame.current = Part::default();
                }
                ";" if frame.statements() => end_statement(frame, reasons),
                _ => frame.current.tokens += 1,
            },
        }
    }
    if let Some(top) = stack.first_mut() {
        end_statement(top, reasons);
    }
}

/// Fixed-vocabulary form of a forbidden name for a reason code.
fn code_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEMPLATE: &str = "template://r-generated-analysis/v2";
    const GOOD: &str = r##"# Two-way ANOVA of tooth length by supplement and dose.
analyze <- function(data) {
  data$dose <- factor(data$dose)
  fit <- stats::aov(len ~ supp * dose, data = data)
  summary_table <- as.data.frame(summary(fit)[[1]])
  summary_table$term <- trimws(rownames(summary_table))
  tukey <- as.data.frame(TukeyHSD(fit, "dose")$dose)
  tukey$comparison <- rownames(tukey)
  list(
    tables = list(anova = summary_table, dose_comparisons = tukey),
    figures = list(
      length_by_dose = function() {
        boxplot(len ~ supp + dose, data = data, col = c("#FFBF00", "#022851"),
                xlab = "Supplement and dose (mg/day)", ylab = "Tooth length")
      }
    ),
    notes = c("Two-way ANOVA with interaction; Tukey HSD across doses.")
  )
}
"##;

    fn columns() -> Vec<String> {
        ["len", "supp", "dose"].map(String::from).to_vec()
    }

    fn reasons(block: &str) -> Vec<String> {
        admit(block, TEMPLATE, &columns()).reasons
    }

    fn entry(body: &str) -> String {
        format!(
            "analyze <- function(data) {{\n{body}\n  list(tables = list(), figures = list())\n}}\n"
        )
    }

    #[test]
    fn accepts_a_typical_analysis_block() {
        let report = admit(GOOD, TEMPLATE, &columns());
        assert_eq!(report.reasons, Vec::<String>::new());
        assert!(report.accepted());
        assert!(report.functions_used.contains(&"TukeyHSD".to_owned()));
        assert_eq!(report.namespaces, vec!["stats".to_owned()]);
        assert!(!report.uses_random_numbers);
        assert_eq!(report.block_sha256, digest(GOOD));
    }

    #[test]
    fn rejects_process_file_and_network_calls() {
        for (block, code) in [
            ("system('ls')", "code_forbidden_function_system"),
            (
                "x <- readLines('data.csv')",
                "code_forbidden_function_readlines",
            ),
            (
                "download.file('x', 'y')",
                "code_forbidden_function_download_file",
            ),
            (
                "con <- socketConnection('h', 80)",
                "code_forbidden_function_socketconnection",
            ),
            ("Sys.getenv('HOME')", "code_forbidden_function_sys_getenv"),
            (
                "f <- utils::getFromNamespace(paste0('sys', 'tem'), 'base')",
                "code_forbidden_function_getfromnamespace",
            ),
            ("utils::nsl('example.org')", "code_forbidden_function_nsl"),
            ("f <- system", "code_forbidden_function_system"),
        ] {
            assert!(reasons(block).contains(&code.to_owned()), "{block}");
        }
    }

    #[test]
    fn rejects_string_named_functions_and_evaluation() {
        for (block, code) in [
            ("sapply(1, 'system')", "code_forbidden_function_system"),
            ("Reduce(\"eval\", list())", "code_forbidden_function_eval"),
            ("do.call(mean, list(1))", "code_forbidden_function_do_call"),
            ("eval(parse(text = 'x'))", "code_forbidden_function_parse"),
            ("get('x')", "code_forbidden_function_get"),
        ] {
            assert!(reasons(block).contains(&code.to_owned()), "{block}");
        }
    }

    #[test]
    fn names_refused_calls_for_the_next_attempt_only() {
        let block = entry(
            "  draws <- replicate(10, mean(sample(data$len, replace = TRUE)))\n  parts <- do.call(rbind, list(data))\n  x <- stats::fancy_fit(len ~ dose, data = data)",
        );
        let report = admit(&block, TEMPLATE, &columns());
        assert!(!report.accepted());
        assert_eq!(report.refused_names, ["do.call", "fancy_fit", "replicate"]);
        // The list stays out of the serialized report and out of every run event code.
        let recorded = serde_json::to_value(&report).unwrap();
        assert!(recorded.get("refused_names").is_none());
        assert_eq!(event_code(report.first_reason()), "code_forbidden_function");
        assert!(admit(GOOD, TEMPLATE, &columns()).refused_names.is_empty());
        // A string in call position is refused, but only a plain bounded name returns.
        let odd = admit("'my fn'(1)", TEMPLATE, &columns());
        assert!(odd.reasons.contains(&"code_name_not_allowed".to_owned()));
        assert!(odd.refused_names.is_empty());
        let many = (0..20)
            .map(|i| format!("unknown_{i:02}(1)"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(admit(&many, TEMPLATE, &columns()).refused_names.len(), 12);
        assert!(refused_name("vapply") && refused_name("do.call") && !refused_name("a b"));
        assert!(!refused_name(&"x".repeat(49)) && !refused_name("_x") && !refused_name("1x"));
    }

    #[test]
    fn event_codes_map_once() {
        for reason in [
            "code_forbidden_function_do_call",
            "code_forbidden_function",
            "code_name_not_allowed",
            "code_literal_data",
            "code_anything_else",
        ] {
            let once = event_code(reason);
            assert_eq!(event_code(once), once, "{reason}");
        }
        assert_eq!(
            event_code("code_forbidden_function"),
            "code_forbidden_function"
        );
    }

    #[test]
    fn rejects_internals_namespaces_and_superassignment() {
        assert!(reasons("stats:::x").contains(&"code_internal_namespace".to_owned()));
        assert!(reasons(".Internal(x)").contains(&"code_hidden_identifier".to_owned()));
        assert!(reasons("jsonlite::toJSON(1)").contains(&"code_namespace_not_allowed".to_owned()));
        assert!(reasons("x <<- 1").contains(&"code_superassignment".to_owned()));
        assert!(reasons("`sys` <- 1").contains(&"code_backtick_name".to_owned()));
        assert!(reasons("x <- r\"(a)\"").contains(&"code_raw_string".to_owned()));
    }

    #[test]
    fn rejects_paths_urls_and_secret_like_strings() {
        assert!(reasons("x <- '/etc/passwd'").contains(&"code_path_or_url".to_owned()));
        assert!(reasons("x <- 'https://example.org'").contains(&"code_path_or_url".to_owned()));
        assert!(reasons("x <- '../data'").contains(&"code_path_or_url".to_owned()));
        assert!(reasons("x <- 'my token'").contains(&"code_secret_like_string".to_owned()));
        assert!(
            reasons("x <- 'QUtJQUlPU0ZPRE5ON0VYQU1QTEVrZXl2YWx1ZQ'")
                .contains(&"code_secret_like_string".to_owned())
        );
    }

    #[test]
    fn rejects_embedded_data() {
        let values = (0..20)
            .map(|v| format!("{v}.5"))
            .collect::<Vec<_>>()
            .join(", ");
        let block = format!("analyze <- function(data) {{ y <- c({values}); list() }}");
        assert!(reasons(&block).contains(&"code_embedded_data".to_owned()));
        let many = (0..70)
            .map(|v| format!("x{v} <- {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(reasons(&many).contains(&"code_too_many_numbers".to_owned()));
    }

    #[test]
    fn requires_the_entry_point_and_balanced_code() {
        assert!(reasons("f <- function(x) x").contains(&"code_missing_entry_point".to_owned()));
        assert!(reasons("analyze <- function(data) {").contains(&"code_unbalanced".to_owned()));
    }

    #[test]
    fn member_names_and_named_arguments_are_not_calls() {
        let block = "analyze <- function(data) { source <- data$source; q <- 2; list(tables = list(file = data), notes = c('Loaded from the source file')) }";
        assert_eq!(reasons(block), Vec::<String>::new());
    }

    #[test]
    fn the_preview_block_is_admitted_as_recorded() {
        let block =
            include_str!("../../clients/workspace/fixtures/r/generated/package/code/block.R");
        let recorded: serde_json::Value = serde_json::from_str(include_str!(
            "../../clients/workspace/fixtures/r/generated/admission.json"
        ))
        .unwrap();
        let columns = ["sample_id", "condition", "response"].map(String::from);
        assert_eq!(
            serde_json::to_value(admit(block, TEMPLATE, &columns)).unwrap(),
            recorded
        );
    }

    #[test]
    fn reports_random_number_use() {
        let block =
            "analyze <- function(data) { set.seed(1); list(tables = list(), figures = list()) }";
        let report = admit(block, TEMPLATE, &columns());
        assert!(report.accepted());
        assert!(report.uses_random_numbers);
    }

    #[test]
    fn the_allowlist_digest_matches_the_template() {
        // The template computes the same digest over its list; a change to either
        // list must change both.
        assert_eq!(
            allowlist_digest(),
            "sha256:869e5cadcf8ba4cc3a215b30e0164f045626de5779f9acbd03670fbd076402a1"
        );
        assert_eq!(
            admit(GOOD, TEMPLATE, &columns()).allowlist,
            allowlist_digest()
        );
    }

    #[test]
    fn refuses_rows_written_into_the_code() {
        // The block returned two invented rows.
        let invented = "analyze <- function(data) { list(tables = list(result = data.frame(value = c(91, 92)))) }";
        assert!(reasons(invented).contains(&"code_literal_data".to_owned()));
        for body in [
            "  x <- c(91, 92)",
            "  t <- data.frame(value = cumsum(c(91, 1)))",
            "  t <- data.frame(value = 91)",
            "  t <- cbind(data, one = 1)",
            "  data$len[1:2] <- c(91, 92)",
            "  c(91, 92) -> x",
            "  f <- function(d, v = c(1, 2)) v",
            "  t <- as.data.frame(seq(1, 10))",
        ] {
            assert!(
                reasons(&entry(body)).contains(&"code_literal_data".to_owned()),
                "{body}"
            );
        }
    }

    #[test]
    fn numbers_may_set_parameters_of_a_computation() {
        for body in [
            "  q <- quantile(data$len, c(0.25, 0.75))",
            "  m <- round(mean(data$len), 2)",
            "  t <- data.frame(m = round(mean(data$len), 2), q = quantile(data$len, 0.5))",
            "  m <- matrix(data$len, nrow = 2)",
            "  alpha <- 0.05",
            "  with(data, interaction.plot(dose, supp, len, pch = c(19, 17)))",
            "  s <- summary(stats::aov(len ~ supp, data = data))[[1]]",
            "  for (i in 1:3) print(i)",
        ] {
            assert_eq!(reasons(&entry(body)), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn refuses_functions_assembled_at_run_time() {
        // File.exists built from language objects.
        let constructed = r#"analyze <- function(data) {
 target <- paste0("", intToUtf8(47), "tmp", intToUtf8(47), "marker")
 ns <- as.call(list(as.name("::"), as.name("base"), as.name(paste0("fi", "le", ".exists"))))
 body_call <- as.call(list(ns, target))
 reader <- as.function(list(body_call))
 list(tables = list(result = data.frame(marker_exists = reader())))
}"#;
        assert!(reasons(constructed).contains(&"code_name_not_allowed".to_owned()));
        for (body, code) in [
            (
                "  x <- sapply(names(data), file.exists)",
                "code_forbidden_function_file_exists",
            ),
            ("  x <- sapply(1, Sys.time)", "code_name_not_allowed"),
            ("  x <- (function(v) v)(1)", "code_dynamic_call"),
            (
                "  f <- function() mean\n  x <- f()(data$len)",
                "code_dynamic_call",
            ),
            (
                "  fns <- list(mean)\n  x <- fns[[1]](data$len)",
                "code_dynamic_call",
            ),
            ("  x <- data$f(1)", "code_dynamic_call"),
            (
                "  x <- \"file.exists\"(\"marker\")",
                "code_name_not_allowed",
            ),
            ("  x <- baseenv()", "code_forbidden_function_baseenv"),
        ] {
            assert!(reasons(&entry(body)).contains(&code.to_owned()), "{body}");
        }
    }

    #[test]
    fn parentheses_after_a_head_or_a_line_break_are_not_calls() {
        for body in [
            "  x <- if (TRUE) (1)",
            "  f <- function(v) (v + 1)",
            "  y <- mean(data$len)\n  (y)",
        ] {
            assert_eq!(reasons(&entry(body)), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn names_are_bound_columns_or_allowlisted() {
        assert_eq!(
            reasons(&entry("  fit <- lm(len ~ dose, data = data)")),
            Vec::<String>::new()
        );
        assert!(
            reasons(&entry("  m <- mean(weight)")).contains(&"code_name_not_allowed".to_owned())
        );
        let later = "analyze <- function(data) {\n  helper(data)\n}\nhelper <- function(d) list(tables = list(means = aggregate(len ~ supp, data = d, FUN = mean)))\n";
        assert_eq!(reasons(later), Vec::<String>::new());
        // Outside a formula, a column is a known name only when the contract lists it.
        assert!(
            admit(&entry("  m <- mean(len)"), TEMPLATE, &[])
                .reasons
                .contains(&"code_name_not_allowed".to_owned())
        );
        assert_eq!(reasons(&entry("  m <- mean(len)")), Vec::<String>::new());
    }

    #[test]
    fn formulas_and_column_scopes_may_name_columns_the_block_added() {
        for body in [
            "  data$dose_group <- factor(data$dose)\n  k <- kruskal.test(len ~ dose_group, data = data)",
            "  data$dose_group <- factor(data$dose)\n  m <- with(data, tapply(len, dose_group, mean))",
            "  s <- subset(data, dose_group == \"a\")",
            "  fit <- lm(log(weight) ~ dose, data = data)",
            "  f <- ratio ~ dose\n  fit <- lm(f, data = data)",
        ] {
            assert_eq!(reasons(&entry(body)), Vec::<String>::new(), "{body}");
        }
        for (body, code) in [
            (
                "  data$dose_group <- factor(data$dose)\n  m <- mean(dose_group)",
                "code_name_not_allowed",
            ),
            (
                "  fit <- lm(foo(len) ~ dose, data = data)",
                "code_name_not_allowed",
            ),
            (
                "  s <- subset(data, file.exists(dose_group))",
                "code_forbidden_function_file_exists",
            ),
        ] {
            assert!(reasons(&entry(body)).contains(&code.to_owned()), "{body}");
        }
    }
}
