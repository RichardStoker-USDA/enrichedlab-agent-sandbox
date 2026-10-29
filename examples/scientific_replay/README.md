# Scientific replay reference

This candidate example produces a small, inspectable research package from R's public
`datasets::ToothGrowth` study. It exercises data custody and a descriptive method. It is
not a private laboratory analysis, a validated biological finding, or a multi-user agent service.

The operator supplies an immutable R image, stages these scripts read-only, and runs the
method in an isolated, non-root container with no network or API token. The runtime owns
paths, resource limits, storage and teardown. The method reads a declared input and writes
a new output directory. No model transports or creates measurement values.

```sh
Rscript scripts/prepare_reference.R .
Rscript scripts/analysis.R inputs/observations.csv outputs
python3 scripts/package.py build . --runtime-facts execution.json --output research_replay.zip
```

After extraction, open `index.html` to inspect the source, method, figure and execution
record. `outputs/figure.pdf` is the R-rendered publication master; the SVG is an R-rendered
preview. Run these commands from the extracted directory:

```sh
python3 scripts/package.py verify .
python3 scripts/package.py rerun .
```

Python 3.11 or later is required. Verification checks every inventoried file and refuses altered or extra files in the
input, method and original-result directories. Rerun uses the included CSV and exact
script in a new `rerun` directory, then compares group labels, counts and descriptive
statistics. Numeric comparison uses absolute and relative tolerances of `1e-12`; PDF/SVG
bytes are not asserted identical across graphics libraries. R must be installed locally.
Use the recorded image for the closest environment match. No package installation is required.
The PDF uses base R graphics. A local SVG preview additionally needs Cairo support;
`rerun/graphics.txt` states whether it was available. Missing Cairo does not prevent
the numeric comparison or PDF output and does not produce a substitute plot silently.

The manifest and RO-Crate metadata retain code, original data, runtime facts, outputs and
checksums. Checksums detect changes relative to this manifest; they are not a digital
signature or scientific validation. Execution facts must come from the operator's actual
workload receipt. Failed attempts keep separate directories and receipts. This example
does not implement the full agent event ledger or replace the existing state export.

The dataset is distributed with R under R's licensing terms. Keep its source reference and
original study documentation. Do not commit generated data, results or private runtime
facts to this reusable repository.

Art direction: **UC Davis research workbench**. The result opens first, beside its
validation status and concrete review actions. Tabs expose data/method, run history,
and reproduction instructions. File lineage opens the original data or exact code.
There is no promotional hero, green theme, decorative card grid, or invented product
claim. The interface uses [Aggie Blue and Aggie Gold](https://marketingtoolbox.ucdavis.edu/brand-guide/colors)
(`#022851`, `#FFBF00`) in both light and dark modes, with neutral reading surfaces.
The R figure uses the same palette with outlined points for contrast.

The portable page supports persisted light/dark preference, keyboard tab navigation
and small screens without external fonts, scripts or services. For a hosted preview,
serve the parent directory containing the extracted folder and its ZIP. The download
link resolves to that sibling ZIP. When opened directly as a local file, that link
is hidden because the package is already extracted. File links and rerun instructions
remain available offline.

## Retained broker reference tasks

`retained_review.py` extends the same UC Davis research workbench direction for paired
mean/median reference recipes downloaded from the broker. Supply the original archives,
authenticated download receipts, compute teardown receipt and local replay comparison.
The builder verifies those bindings, extracts the original files, previews each R PDF
with Poppler, and writes a portable HTML folder plus ZIP. It does not submit tasks or
turn a technical success into scientific approval. Generated output and private receipts
stay outside this repository.

```sh
python3 retained_review.py build /path/to/downloaded_artifacts --output /path/to/research_review
python3 /path/to/research_review/retained_review.py verify /path/to/research_review
python3 /path/to/research_review/retained_review.py rerun /path/to/research_review --output /path/to/local_replay
```

Python 3.12+, local R, and Poppler (`pdftoppm`, for building the preview only) are required.
The builder uses adjacent `review.html` styling; the delivered page and rerun utility
work offline without that template or Poppler. All authoritative numbers come directly
from R CSVs. The included immutable task archives remain unchanged. The replay compares
all bootstrap samples and interval/descriptive tables at absolute/relative tolerance
1e-12. Original R PDFs remain the figure masters; PNGs are explicitly derived previews.
This reference distinguishes operator-chosen reviewed recipes from future model tool
selection, generated-code tiers, long-horizon memory and general scaling.
