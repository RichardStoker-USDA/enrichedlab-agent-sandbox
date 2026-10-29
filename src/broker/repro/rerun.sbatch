#!/bin/bash
# Rerun every analysis in this EnrichedLab package on a Slurm cluster with Apptainer, then compare each result table
# with the retained one. From the unpacked package folder:
#
#   sbatch -A <account> rerun/slurm/rerun.sbatch
#
# Leave out -A where your cluster gives you a default account. The first time, the job pulls the R image these analyses
# ran with, by its digest, into the package folder; that needs internet access on the compute node, which SCINet's
# compute nodes have. Where they do not, pull it once on a node that does, from the package folder:
#
#   apptainer pull r-base-4.6.1.sif docker://docker.io/library/r-base@sha256:4e6fb1eebb29725c9c524851fb9a4bd2bd152254bcc3778cb44fd1c3d7fe9d1b
#
# or set IMAGE to an image file you already have. The job reruns an analysis whose code the research agent wrote only
# when a named person on the project has accepted it (verify.py --reviewed-only); the report names the ones it skipped.
# The package folder must be writable: each job writes its results to its own extracted-<job id> folder there.
# rerun/README.md has the steps for SCINet Atlas and Ceres.
#SBATCH --job-name=enrichedlab-rerun
#SBATCH --cpus-per-task=2
#SBATCH --mem=8G
#SBATCH --time=02:00:00
#SBATCH --output=rerun-%j.log
set -euo pipefail
cd "${SLURM_SUBMIT_DIR:-$PWD}"
test -f SHA256SUMS -a -f rerun/verify.py || { echo "Submit this from the unpacked package folder." >&2; exit 1; }
if ! command -v apptainer >/dev/null 2>&1; then module load apptainer 2>/dev/null || true; fi
command -v apptainer >/dev/null 2>&1 \
  || { echo "Apptainer is not available: load its module (module spider apptainer) and submit again." >&2; exit 1; }
python3 -c 'import sys; sys.exit(sys.version_info < (3, 8))' 2>/dev/null \
  || { echo "This needs Python 3.8 or later: load a newer Python module (module spider python) and submit again." >&2; exit 1; }
# Apptainer's cache and build files go to the job's node-local scratch when it has one, never the home folder's quota.
export APPTAINER_CACHEDIR="${APPTAINER_CACHEDIR:-${TMPDIR:-$PWD/.apptainer}}"
export APPTAINER_TMPDIR="${APPTAINER_TMPDIR:-${TMPDIR:-$PWD/.apptainer}}"
mkdir -p "$APPTAINER_CACHEDIR" "$APPTAINER_TMPDIR"
IMAGE="${IMAGE:-$PWD/r-base-4.6.1.sif}"
if [ ! -f "$IMAGE" ]; then
  # Pull into a file of this job's own, then rename it into place, so two first runs never meet a half-written image;
  # both pull the same digest, so whichever rename lands last leaves the same bytes.
  part="$(dirname "$IMAGE")/.r-base-4.6.1.${SLURM_JOB_ID:-$$}.sif"
  if apptainer pull "$part" docker://docker.io/library/r-base@sha256:4e6fb1eebb29725c9c524851fb9a4bd2bd152254bcc3778cb44fd1c3d7fe9d1b; then
    mv -f "$part" "$IMAGE"
  else
    rm -f "$part"
  fi
  test -f "$IMAGE" \
    || { echo "The R image could not be pulled here: pull it on a node with internet access (see the top of this file) or set IMAGE." >&2; exit 1; }
fi
# The verifier splits --rscript like a shell, so the image path is quoted for a folder name with spaces.
QUOTED_IMAGE="$(python3 -c 'import shlex, sys; print(shlex.quote(sys.argv[1]))' "$IMAGE")"
# This pinned base-R image does not carry the render-service templates' ggplot2, ragg, svglite, scales and jsonlite
# packages. Keep those analyses visible as not rerun until the kit pins an image that can run them.
python3 rerun/verify.py --rerun --reviewed-only --work "extracted-${SLURM_JOB_ID:-$$}" \
  --formats template,recipe \
  --rscript "apptainer exec --cleanenv --env ENRICHEDLAB_TEMPLATE_RERUN=local --bind {dir} --pwd {dir} $QUOTED_IMAGE Rscript"
