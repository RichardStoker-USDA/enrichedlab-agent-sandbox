"""The rerun kit's Slurm script (src/broker/repro/rerun.sbatch) with stubs for apptainer and python3.

No Slurm, R or network is used: the stubs record their arguments, and the pull stub writes the image file. Run with:
python3 -m unittest discover -s tests/repro
"""
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SBATCH = Path(__file__).resolve().parents[2] / "src" / "broker" / "repro" / "rerun.sbatch"
DIGEST = "sha256:4e6fb1eebb29725c9c524851fb9a4bd2bd152254bcc3778cb44fd1c3d7fe9d1b"

APPTAINER = textwrap.dedent('''\
    #!/bin/sh
    echo "apptainer $* cache=$APPTAINER_CACHEDIR tmp=$APPTAINER_TMPDIR" >> "$CALLS"
    if [ "$1" = pull ]; then
      [ "$PULL" = fail ] && exit 1
      : > "$2"
    fi
''')

PYTHON = textwrap.dedent('''\
    #!/bin/sh
    if [ "$1" = -c ]; then
      case "$2" in
        *version_info*) [ "$PY_OLD" = 1 ] && exit 1; exit 0 ;;
      esac
      exec "$REAL_PYTHON" "$@"
    fi
    echo "python3 $*" >> "$CALLS"
''')


class SlurmScript(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="sbatch-codetest-"))
        self.addCleanup(shutil.rmtree, self.root)
        self.package = self.root / "package"
        (self.package / "rerun").mkdir(parents=True)
        (self.package / "SHA256SUMS").write_text("")
        (self.package / "rerun" / "verify.py").write_text("")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, text in (("apptainer", APPTAINER), ("python3", PYTHON)):
            (self.bin / name).write_text(text)
            (self.bin / name).chmod(0o755)
        self.scratch = self.root / "scratch"
        self.scratch.mkdir()
        self.calls = self.root / "calls.txt"

    def run_script(self, **env):
        base = {"PATH": "{}:/usr/bin:/bin".format(self.bin), "HOME": str(self.root), "CALLS": str(self.calls),
                "SLURM_SUBMIT_DIR": str(self.package), "TMPDIR": str(self.scratch), "REAL_PYTHON": sys.executable}
        result = subprocess.run(["bash", str(SBATCH)], cwd=self.package, env={**base, **env}, capture_output=True,
                                text=True, timeout=60)
        calls = self.calls.read_text().splitlines() if self.calls.exists() else []
        return result, calls

    def test_pulls_the_pinned_image_once_then_reruns_reviewed_code_with_it(self):
        result, calls = self.run_script(SLURM_JOB_ID="4242")
        self.assertEqual(result.returncode, 0, result.stderr)
        image = self.package / "r-base-4.6.1.sif"
        self.assertTrue(image.is_file())
        # The pull lands in a file of the job's own and is renamed into place.
        self.assertEqual(calls[0], "apptainer pull {} docker://docker.io/library/r-base@{} cache={} tmp={}".format(
            self.package / ".r-base-4.6.1.4242.sif", DIGEST, self.scratch, self.scratch))
        self.assertFalse((self.package / ".r-base-4.6.1.4242.sif").exists())
        self.assertTrue(calls[1].startswith(
            "python3 rerun/verify.py --rerun --reviewed-only --work extracted-4242 --formats template,recipe "
            "--rscript apptainer exec --cleanenv"))
        self.assertIn("{} Rscript".format(image), calls[1])
        self.calls.write_text("")
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(c.startswith("apptainer pull") for c in calls))

    def test_keeps_the_image_cache_out_of_the_home_folder_without_scratch(self):
        env = {"PATH": "{}:/usr/bin:/bin".format(self.bin), "HOME": str(self.root), "CALLS": str(self.calls),
               "SLURM_SUBMIT_DIR": str(self.package), "REAL_PYTHON": sys.executable}
        result = subprocess.run(["bash", str(SBATCH)], cwd=self.package, env=env, capture_output=True, text=True,
                                timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        pull = self.calls.read_text().splitlines()[0]
        self.assertIn("cache={}".format(self.package / ".apptainer"), pull)

    def test_a_failed_pull_or_an_old_python_says_what_to_do(self):
        result, calls = self.run_script(PULL="fail")
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not be pulled here", result.stderr)
        self.assertFalse(any(c.startswith("python3 rerun/verify.py") for c in calls))
        result, _ = self.run_script(PY_OLD="1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("Python 3.8 or later", result.stderr)

    def test_an_image_path_with_spaces_stays_one_argument(self):
        folder = self.root / "R images"
        folder.mkdir()
        image = folder / "r-base-4.6.1.sif"
        image.write_bytes(b"")
        result, calls = self.run_script(IMAGE=str(image))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(c.startswith("apptainer pull") for c in calls))
        rscript = calls[0].split("--rscript ", 1)[1]
        self.assertEqual(shlex.split(rscript)[-2:], [str(image), "Rscript"])

    def test_refuses_to_start_outside_the_package_folder(self):
        os.remove(self.package / "SHA256SUMS")
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("unpacked package folder", result.stderr)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
