#!/usr/bin/env python3
"""Check, unpack and rerun the analyses in an EnrichedLab project evidence package.

Run it from the unpacked package folder (the one holding SHA256SUMS):

  python3 rerun/verify.py            check every checksum and unpack each analysis into extracted/
  python3 rerun/verify.py --rerun    also rerun each analysis and compare its tables with the retained ones

--rscript sets how R is started, for example inside a container:

  --rscript "docker run --rm --network none -e ENRICHEDLAB_TEMPLATE_RERUN=local -v {dir}:{dir} -w {dir} IMAGE Rscript"

where {dir} is replaced by each analysis folder. It reruns three package formats: an analysis written by a reviewed
template (run.json names the template and seed), a reviewed recipe that replays itself (run_parameters.json with
code/bootstrap_replay.R), and a render-service analysis whose spec.json has the exact template-analysis schema.
--formats selects a comma-separated subset. Any other or excluded package is reported as not rerun. With
--reviewed-only, as the Slurm job runs it, an analysis whose block the research agent wrote is rerun only when a named
person's newest review accepted it; the others are reported as not rerun, and when any package checksum fails no
analysis is rerun at all, since the review records themselves are then in doubt. --work names the folder the results go
to (extracted by default); the Slurm job gives each job its own, so two jobs from one package never share files. Python
3.8 or later, standard library only. The script prints counts and outcomes, never table or result values.

Exit status: 0 when every checksum matches and, with --rerun, every analysis was rerun and its results match; 1 when a
checksum or file is wrong; 2 when a rerun failed or a result is missing or differs beyond the tolerance; 3 when, with
--rerun, some analysis could not be rerun or had nothing to compare and nothing else is wrong. When several apply, the
lowest number is reported.
"""
import argparse
import csv
import hashlib
import json
import math
import os
import re
import shlex
import stat
import subprocess
import sys
import tarfile
import time
import unicodedata
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
RECIPE_SCHEMA = "sandbox.enrichedlab.org/reference-r-parameters/v1"
RENDER_TEMPLATE_SCHEMA = "enrichedlab.template-analysis-spec.v1"
RERUN_FORMATS = ("template", "recipe", "render-template")
R_SESSION_STRING_FIELDS = ("r_version", "platform", "running", "locale")
R_SESSION_LIBRARY_FIELDS = ("blas", "lapack")
R_SESSION_LIST_FIELDS = ("base_packages", "attached")
R_PACKAGE_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9.]{0,63}$")
R_PACKAGE_VERSION = re.compile(r"^[0-9][0-9A-Za-z.-]{0,31}$")
R_LIBRARY_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$")
MAX_R_SESSION_BYTES = 64 * 1024


def safe_name(name):
    if not isinstance(name, str):
        return False
    path = PurePosixPath(name)
    return (bool(path.parts) and path.as_posix() == name and not path.is_absolute() and ".." not in path.parts
            and "\\" not in name and not re.match(r"^[A-Za-z]:", name))


def open_regular(base, name):
    """Open one confined regular file without following a link in any path component."""
    if not safe_name(name):
        raise OSError("unsafe path")
    root = Path(base).resolve(strict=True)
    target = root.joinpath(*PurePosixPath(name).parts)
    try:
        target.resolve(strict=True).relative_to(root)
    except (OSError, ValueError):
        raise OSError("path leaves package root")
    current = root
    parts = PurePosixPath(name).parts
    target_stat = None
    for index, part in enumerate(parts):
        current = current / part
        target_stat = os.lstat(current)
        if stat.S_ISLNK(target_stat.st_mode):
            raise OSError("linked path")
        if index < len(parts) - 1 and not stat.S_ISDIR(target_stat.st_mode):
            raise OSError("non-directory path component")
    if target_stat is None or not stat.S_ISREG(target_stat.st_mode):
        raise OSError("not a regular file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(str(target), flags)
    try:
        opened = os.fstat(descriptor)
        if (not stat.S_ISREG(opened.st_mode)
                or (opened.st_dev, opened.st_ino) != (target_stat.st_dev, target_stat.st_ino)):
            raise OSError("file changed while opening")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def read_regular(base, name):
    with open_regular(base, name) as source:
        data = source.read()
        metadata = os.fstat(source.fileno())
    return data, metadata


def sha256_file(source):
    h = hashlib.sha256()
    for block in iter(lambda: source.read(1 << 20), b""):
        h.update(block)
    return h.hexdigest()


def file_sha256(base, name):
    with open_regular(base, name) as source:
        return sha256_file(source)


def manifest_digests(base, name="SHA256SUMS"):
    try:
        data, _ = read_regular(base, name)
        lines = data.decode("utf-8").splitlines()
    except (OSError, UnicodeError):
        return {}
    digests = {}
    for line in lines:
        if not line.strip():
            continue
        digest, separator, path = line.partition("  ")
        if separator:
            digests[path] = digest.strip().lower()
    return digests


def check_sums(base, sums):
    """Every line of a SHA256SUMS file as (path, problem or None)."""
    try:
        sums_name = sums.relative_to(base).as_posix()
        data, _ = read_regular(base, sums_name)
        lines = data.decode("utf-8").splitlines()
    except (OSError, UnicodeError, ValueError):
        return [(sums.name, "missing")]
    results = []
    for line in lines:
        if not line.strip():
            continue
        digest, _, name = line.partition("  ")
        if not safe_name(name):
            results.append((name, "missing"))
            continue
        try:
            actual = file_sha256(base, name)
        except OSError:
            results.append((name, "missing"))
        else:
            if actual != digest.strip().lower():
                results.append((name, "checksum differs"))
            else:
                results.append((name, None))
    return results


def ensure_directory(root, parts=()):
    """Create ordinary directories below root, refusing every existing link or non-directory component."""
    if os.path.lexists(str(root)):
        root_stat = os.lstat(root)
        if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
            raise ValueError("linked or non-directory extraction root")
    else:
        root.mkdir(parents=True)
    current = root
    for part in parts:
        current = current / part
        if os.path.lexists(str(current)):
            current_stat = os.lstat(current)
            if stat.S_ISLNK(current_stat.st_mode) or not stat.S_ISDIR(current_stat.st_mode):
                raise ValueError("linked or non-directory extraction path")
        else:
            current.mkdir()
    return current


def extract(archive, dest, archive_name):
    """Unpack regular files and folders only; refuse links, devices and paths outside dest."""
    count = 0
    with tarfile.open(fileobj=archive, mode="r:gz") as tar:
        for member in tar.getmembers():
            name = member.name[2:] if member.name.startswith("./") else member.name
            if not name or name == ".":
                continue
            if not safe_name(name):
                raise ValueError("unsafe path in " + archive_name)
            parts = PurePosixPath(name).parts
            if member.isdir():
                ensure_directory(dest, parts)
            elif member.isfile():
                parent = ensure_directory(dest, parts[:-1])
                target = parent / parts[-1]
                source = tar.extractfile(member)
                target_stat = os.lstat(target) if os.path.lexists(str(target)) else None
                if (source is None or (target_stat is not None
                        and (stat.S_ISLNK(target_stat.st_mode) or not stat.S_ISREG(target_stat.st_mode)))):
                    raise ValueError("unreadable file in " + archive_name)
                flags = (os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_BINARY", 0)
                         | getattr(os, "O_NOFOLLOW", 0))
                descriptor = os.open(str(target), flags, 0o600)
                with os.fdopen(descriptor, "wb") as out:
                    if not stat.S_ISREG(os.fstat(out.fileno()).st_mode):
                        raise ValueError("non-regular target in " + archive_name)
                    out.write(source.read())
                count += 1
            else:
                raise ValueError("link or special file in " + archive_name)
    return count


def copy_verified(base, name, target, expected):
    """Copy one confined file and verify the bytes copied, without following source or target links."""
    digest = hashlib.sha256()
    try:
        with open_regular(base, name) as source, open(target, "xb") as output:
            for block in iter(lambda: source.read(1 << 20), b""):
                digest.update(block)
                output.write(block)
        if digest.hexdigest() != expected.lower():
            raise OSError("checksum differs")
    except Exception:
        try:
            target.unlink()
        except OSError:
            pass
        raise


def load_json(path):
    """A JSON object from a package file, or an empty one when it is absent or unreadable."""
    try:
        value = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}
    except (ValueError, OSError):
        return {}
    return value if isinstance(value, dict) else {}


def agent_written(analysis, kind):
    """True when the agent wrote the analysis's block: a template run with no configured settings. A configured
    method's block is rendered by the platform from its settings, and a recipe carries no block."""
    return kind == "template" and not analysis.get("settings")


def person_accepted(analysis):
    """True when the newest review of the analysis is a named person's acceptance."""
    reviews = [r for r in analysis.get("reviews") or [] if isinstance(r, dict)]
    if not reviews:
        return False
    newest = max(reviews, key=lambda r: str(r.get("recorded_at") or ""))
    return newest.get("decision") == "accepted" and newest.get("reviewer_kind") == "person"


def package_kind(dest):
    """How an unpacked analysis package can be rerun: its format and the parameters the rerun needs."""
    spec = load_json(dest / "spec.json")
    if spec.get("schema") == RENDER_TEMPLATE_SCHEMA:
        return "render-template", spec
    run = load_json(dest / "run.json")
    if str(run.get("template_ref", "")).startswith("template://") and run.get("seed") is not None:
        return "template", run
    params = load_json(dest / "run_parameters.json")
    if (params.get("schemaVersion") == RECIPE_SCHEMA and params.get("statistic") in ("mean", "median")
            and isinstance(params.get("seed"), int) and (dest / "code" / "bootstrap_replay.R").is_file()
            and (dest / "input" / "source.zip").is_file()):
        return "recipe", params
    return "other", run


def recorded_r_session(run):
    """The broker-validated R session in a render-template spec, checked again before it is shown or queried: the ten
    fields render-service 0.7.1 writes, with its rules (text at most 400 characters without control characters and
    possibly empty, BLAS and LAPACK a file name or empty, LAPACK's version a version or empty)."""
    service = run.get("service")
    session = service.get("r_session") if isinstance(service, dict) else None
    fields = (R_SESSION_STRING_FIELDS + R_SESSION_LIBRARY_FIELDS + ("lapack_version",) + R_SESSION_LIST_FIELDS
              + ("packages",))
    if not isinstance(session, dict) or set(session) != set(fields):
        return None
    try:
        if len(json.dumps(session, ensure_ascii=False, separators=(",", ":")).encode("utf-8")) > MAX_R_SESSION_BYTES:
            return None
    except (TypeError, ValueError):
        return None
    if any(not isinstance(session.get(name), str) or len(session[name]) > 400
           or any(unicodedata.category(character) == "Cc" for character in session[name])
           for name in R_SESSION_STRING_FIELDS):
        return None
    if any(not isinstance(session.get(name), str) or (session[name] and not R_LIBRARY_NAME.fullmatch(session[name]))
           for name in R_SESSION_LIBRARY_FIELDS):
        return None
    version = session.get("lapack_version")
    if not isinstance(version, str) or (version and not R_PACKAGE_VERSION.fullmatch(version)):
        return None
    for field in R_SESSION_LIST_FIELDS:
        names = session.get(field)
        if (not isinstance(names, list) or len(names) > 512
                or any(not isinstance(name, str) or not R_PACKAGE_NAME.fullmatch(name) for name in names)):
            return None
    packages = session.get("packages")
    if (not isinstance(packages, dict) or not packages or len(packages) > 512
            or any(not isinstance(name, str) or not R_PACKAGE_NAME.fullmatch(name)
                   or not isinstance(version, str) or not R_PACKAGE_VERSION.fullmatch(version)
                   for name, version in packages.items())):
        return None
    return session


def local_r_expression(packages):
    """Base-R expression that prints the local R version and selected installed-package versions as JSON."""
    names = ",".join(json.dumps(name) for name in packages)
    return ("packages<-c(" + names + ");q<-function(x)encodeString(x,quote='\"',na.encode=FALSE);"
            "versions<-vapply(packages,function(p)if(requireNamespace(p,quietly=TRUE))"
            "q(as.character(utils::packageVersion(p)))else'null',character(1));"
            "items<-if(length(packages))paste(paste0(q(packages),':',versions),collapse=',')else'';"
            "cat(paste0('{\"r_version\":',q(R.version.string),',\"packages\":{',items,'}}'))")


def local_r_session(words, dest, env, packages, timeout):
    """Read the local R environment through the same launcher and startup boundary used for the rerun."""
    try:
        completed = subprocess.run(words + ["--vanilla", "-e", local_r_expression(packages)], cwd=dest, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        return None, "local R environment could not be read ({})".format(type(error).__name__)
    if completed.returncode != 0 or len(completed.stdout) > MAX_R_SESSION_BYTES:
        return None, "local R environment could not be read"
    try:
        value = json.loads(completed.stdout.decode("utf-8"))
    except (UnicodeError, ValueError):
        return None, "local R environment could not be read"
    versions = value.get("packages") if isinstance(value, dict) else None
    if not isinstance(value.get("r_version") if isinstance(value, dict) else None, str) or not isinstance(versions, dict):
        return None, "local R environment could not be read"
    selected = {}
    for name in packages:
        version = versions.get(name)
        if version is not None and (not isinstance(version, str) or not R_PACKAGE_VERSION.fullmatch(version)):
            return None, "local R environment could not be read"
        selected[name] = version
    return {"r_version": value["r_version"], "packages": selected}, None


def r_environment_comparison(recorded, local, problem):
    """Recorded and local R facts plus every informational difference; none of these affects result comparison."""
    kept = {"r_version": recorded["r_version"], "packages": dict(sorted(recorded["packages"].items()))}
    if problem:
        return {"recorded": kept, "local": None, "differences": [problem]}
    differences = []
    if recorded["r_version"] != local["r_version"]:
        differences.append("R version: recorded {!r}, local {!r}".format(
            recorded["r_version"], local["r_version"]))
    for name, version in sorted(recorded["packages"].items()):
        local_version = local["packages"].get(name)
        if local_version != version:
            differences.append("package {}: recorded {}, local {}".format(
                name, version, local_version if local_version is not None else "not installed"))
    return {"recorded": kept, "local": local, "differences": differences}


def csv_shape(path):
    """Rows (after the header) and columns of a CSV table; its cells are read, never printed."""
    with open(path, newline="", encoding="utf-8") as f:
        reader = csv.reader(f)
        header = next(reader, [])
        return sum(1 for _ in reader), len(header)


def result_tables(dest, kind, run, digests):
    """Each result table as name, file, rows and columns. A template lists its tables in run.json; every other package
    is read from the CSV files under results/ that its own SHA256SUMS names."""
    if kind == "template":
        return [{"name": t.get("name"), "file": t.get("file"), "rows": t.get("rows"), "columns": t.get("columns")}
                for t in run.get("tables") or []]
    tables = []
    for name in sorted(digests):
        if name.startswith("results/") and name.endswith(".csv") and safe_name(name) and (dest / name).is_file():
            rows, columns = csv_shape(dest / name)
            tables.append({"name": PurePosixPath(name).stem, "file": name, "rows": rows, "columns": columns})
    return tables


def rerun_words(kind, params):
    """The rerun command a person types in the analysis folder, as METHOD.md shows it."""
    if kind == "template":
        return "ENRICHEDLAB_TEMPLATE_RERUN=local Rscript --vanilla code/template.R input code/block.R rerun {}".format(params["seed"])
    if kind == "recipe":
        return ("Rscript --vanilla code/bootstrap_replay.R input/source.zip rerun.tar.gz {} {}, then unpack rerun.tar.gz "
                "into a folder named rerun".format(params["statistic"], params["seed"]))
    if kind == "render-template":
        code = params.get("code")
        template_record = code.get("template") if isinstance(code, dict) else None
        template = template_record.get("path") if isinstance(template_record, dict) else None
        return "Rscript --vanilla {} rerun".format(shlex.quote(template)) if isinstance(template, str) and template else ""
    return ""


R_STARTUP_VARIABLES = ("R_PROFILE", "R_PROFILE_USER", "R_ENVIRON", "R_ENVIRON_USER", "R_LIBS_USER",
                       "R_DEFAULT_PACKAGES", "R_SCRIPT_DEFAULT_PACKAGES", "R_TESTS")


def rerun_arguments(kind, params, folder):
    if kind == "template":
        return ["--vanilla", "code/template.R", "input", "code/block.R", folder, str(params["seed"])]
    if kind == "recipe":
        return ["--vanilla", "code/bootstrap_replay.R", "input/source.zip", folder + ".tar.gz",
                params["statistic"], str(params["seed"])]
    return ["--vanilla", params["code"]["template"]["path"], folder]


def selected_formats(text):
    """A validated comma-separated set of rerunnable package formats."""
    formats = [part.strip() for part in text.split(",")]
    if not formats or any(not part or part not in RERUN_FORMATS for part in formats):
        raise argparse.ArgumentTypeError("choose a comma list of template, recipe and render-template")
    return set(formats)


def relative_tolerance(text):
    try:
        value = float(text)
    except ValueError:
        raise argparse.ArgumentTypeError("tolerance must be a finite, non-negative number")
    if not math.isfinite(value) or value < 0:
        raise argparse.ArgumentTypeError("tolerance must be a finite, non-negative number")
    return value


def slug(text):
    return re.sub(r"[^a-z0-9]+", "-", (text or "").lower()).strip("-")[:40] or "analysis"


def sas_name(text):
    name = re.sub(r"[^A-Za-z0-9_]", "_", text)
    return (name if re.match(r"[A-Za-z_]", name) else "t_" + name)[:32]


def number(text):
    try:
        value = float(text)
    except ValueError:
        return None
    return value if math.isfinite(value) else None


def compare(retained, rerun, tolerance):
    """Compare two CSV tables. Returns outcome, cells compared, cells different and the largest relative difference."""
    if retained.read_bytes() == rerun.read_bytes():
        return "identical", None, 0, 0.0
    with open(retained, newline="", encoding="utf-8") as a, open(rerun, newline="", encoding="utf-8") as b:
        left, right = list(csv.reader(a)), list(csv.reader(b))
    if len(left) != len(right) or any(len(x) != len(y) for x, y in zip(left, right)) or (left and left[0] != right[0]):
        return "different shape or header", None, None, None
    cells, different, largest = 0, 0, 0.0
    for row_a, row_b in zip(left[1:], right[1:]):
        for x, y in zip(row_a, row_b):
            cells += 1
            if x == y:
                continue
            nx, ny = number(x), number(y)
            if nx is None or ny is None:
                different += 1
                continue
            scale = max(abs(nx), abs(ny))
            relative = abs(nx - ny) / scale if scale else 0.0
            largest = max(largest, relative)
            if relative > tolerance:
                different += 1
    return ("within tolerance" if different == 0 else "different"), cells, different, largest


def parse_json_value(data):
    """A finite JSON value from bytes, with an error label but never its contents."""
    try:
        value = json.loads(data.decode("utf-8"))
    except (UnicodeError, ValueError):
        return None, "missing or unreadable"

    def finite(item):
        if isinstance(item, float):
            return math.isfinite(item)
        if isinstance(item, list):
            return all(finite(child) for child in item)
        if isinstance(item, dict):
            return all(finite(child) for child in item.values())
        return True

    return (value, None) if finite(value) else (None, "missing or unreadable")


def json_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def same_json_shape(left, right):
    """Shapes include object keys, array lengths and JSON scalar types; all JSON numbers share one type."""
    if json_number(left) or json_number(right):
        return json_number(left) and json_number(right)
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(same_json_shape(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(same_json_shape(x, y) for x, y in zip(left, right))
    return True


def compare_json(left, right, tolerance):
    """Compare JSON without returning values: outcome, numbers compared/different and largest relative difference."""
    if not same_json_shape(left, right):
        return "different shape", 0, 0, 0.0
    compared = different = 0
    largest = 0.0
    exact = True
    other_different = False

    def walk(a, b):
        nonlocal compared, different, exact, largest, other_different
        if json_number(a):
            compared += 1
            if a == b:
                return
            exact = False
            scale = max(abs(a), abs(b))
            relative = abs(a / scale - b / scale) if scale else 0.0
            largest = max(largest, relative)
            if relative > tolerance:
                different += 1
            return
        if isinstance(a, dict):
            for key in a:
                walk(a[key], b[key])
        elif isinstance(a, list):
            for x, y in zip(a, b):
                walk(x, y)
        elif a != b:
            exact = False
            other_different = True

    walk(left, right)
    if exact:
        outcome = "identical"
    elif different == 0 and not other_different:
        outcome = "within tolerance"
    else:
        outcome = "different"
    return outcome, compared, different, largest


def render_code_files(dest, spec, digests):
    """Verified render-template code paths, or a safe reason the analysis must not run."""
    code = spec.get("code")
    if not isinstance(code, dict):
        return None, "spec.json does not name all three code files"
    paths = {}
    for role in ("template", "shared", "settings"):
        record = code.get(role)
        path = record.get("path") if isinstance(record, dict) else None
        expected = record.get("sha256") if isinstance(record, dict) else None
        if (not isinstance(path, str) or not safe_name(path) or PurePosixPath(path).parts[0] != "code"
                or not isinstance(expected, str)):
            return None, "spec.json has an invalid {} code record".format(role)
        if role == "template" and not path.endswith(".R"):
            return None, "spec.json has an invalid template code record"
        if role == "settings" and path != "code/params.json":
            return None, "spec.json has an invalid settings code record"
        if path not in digests:
            return None, "the {} code file is not listed in the package manifest".format(role)
        try:
            matches = (digests[path].lower() == expected.lower()
                       and file_sha256(dest, path) == expected.lower())
        except OSError:
            matches = False
        if not matches:
            return None, "the {} code file is missing or its SHA-256 differs".format(role)
        paths[role] = {"path": path, "sha256": expected.lower()}
    return paths, None


def render_record(dest, spec, digests):
    """Read and authenticate the retained render record before any child starts."""
    files = spec.get("files")
    records = [item for item in files if isinstance(item, dict) and item.get("role") == "record"] \
        if isinstance(files, list) else []
    if len(records) != 1:
        return None, "spec.json does not name exactly one retained record"
    path, expected = records[0].get("path"), records[0].get("sha256")
    if path != "results/record.json" or not safe_name(path) or not isinstance(expected, str):
        return None, "spec.json has an invalid retained record"
    if path not in digests:
        return None, "the retained record is not listed in the package manifest"
    try:
        data, metadata = read_regular(dest, path)
    except OSError:
        return None, "the retained record is not a regular file inside its package"
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected.lower() or actual != digests[path].lower():
        return None, "the retained record is missing or its SHA-256 differs"
    value, problem = parse_json_value(data)
    if problem:
        return None, "the retained record is missing or unreadable"
    signature = (metadata.st_dev, metadata.st_ino, metadata.st_size,
                 metadata.st_mtime_ns, metadata.st_ctime_ns)
    return {"path": path, "value": value, "sha256": actual, "signature": signature,
            "identity": (metadata.st_dev, metadata.st_ino)}, None


def retained_record_unchanged(dest, record):
    try:
        data, metadata = read_regular(dest, record["path"])
    except OSError:
        return False
    signature = (metadata.st_dev, metadata.st_ino, metadata.st_size,
                 metadata.st_mtime_ns, metadata.st_ctime_ns)
    return hashlib.sha256(data).hexdigest() == record["sha256"] and signature == record["signature"]


def new_render_result(dest, folder, started_ns, retained_identity):
    """Read a new regular render result without following links or accepting the retained record itself."""
    name = folder + "/results_exact.json"
    target = dest / name
    try:
        target_stat = os.lstat(target)
    except OSError:
        return None, "results_exact.json missing or unreadable"
    if stat.S_ISLNK(target_stat.st_mode) or not stat.S_ISREG(target_stat.st_mode):
        return None, "refused: results_exact.json is not a new regular file"
    try:
        data, metadata = read_regular(dest, name)
    except OSError:
        return None, "refused: results_exact.json is not a new regular file"
    if ((metadata.st_dev, metadata.st_ino) == retained_identity
            or metadata.st_ctime_ns < started_ns):
        return None, "refused: results_exact.json is not a new regular file created by this rerun"
    value, problem = parse_json_value(data)
    return (None, "results_exact.json missing or unreadable") if problem else (value, None)


def method_sheet(analysis, kind, run, tables, digests, command):
    lines = ["# " + (analysis.get("title") or analysis["task_id"]), ""]
    lines.append("Analysis `{}`, method `{}`.".format(analysis["task_id"], analysis.get("method") or "unknown"))
    if analysis.get("objective"):
        lines.append("")
        lines.append("What was asked: " + analysis["objective"])
    if analysis.get("settings"):
        lines.append("")
        lines.append("Settings: " + json.dumps(analysis["settings"], sort_keys=True))
    lines += ["", "## How it ran", ""]
    if kind == "render-template":
        lines.append("- Feature: `{}`".format(run.get("feature")))
        lines.append("- Method label: `{}`".format(run.get("method")))
        service = run.get("service")
        service = service if isinstance(service, dict) else {}
        lines.append("- Service version: `{}`".format(service.get("version")))
        lines.append("- Service image digest: `{}`".format(service.get("image_digest")))
        r_session = recorded_r_session(run)
        if r_session:
            for label, key in (("R version", "r_version"), ("Platform", "platform"), ("OS", "running")):
                if r_session[key]:
                    lines.append("- {}: `{}`".format(label, r_session[key]))
            lines.append("- R packages:")
            for name, version in sorted(r_session["packages"].items()):
                lines.append("  - `{} {}`".format(name, version))
        else:
            lines.append("- R environment: not recorded")
        lines.append("- Settings used by the template: `{}`".format(
            json.dumps(run.get("settings_used"), sort_keys=True, separators=(",", ":"))))
        source = run.get("source")
        source = source if isinstance(source, dict) else {}
        lines.append("- Source analysis: `{}`".format(source.get("task_id")))
        lines.append("- Source table: `{}`, {} rows, SHA-256 `{}`".format(
            source.get("table_path"), source.get("rows"), source.get("table_sha256")))
        code = run.get("code")
        code = code if isinstance(code, dict) else {}
        for role, label in (("template", "Template code"), ("shared", "Shared code"),
                            ("settings", "Settings file")):
            record = code.get(role)
            record = record if isinstance(record, dict) else {}
            lines.append("- {}: `{}`, SHA-256 `{}`".format(label, record.get("path"), record.get("sha256")))
        if command:
            lines.append("- Rerun command: `{}`".format(command))
    else:
        for key, label in (("r_version", "R"), ("seed", "Seed"), ("statistic", "Statistic"),
                           ("recipe", "Recipe script"), ("template_ref", "Template"),
                           ("template_sha256", "Template SHA-256"), ("block_sha256", "Code block SHA-256"),
                           ("started_at", "Started"), ("finished_at", "Finished"), ("status", "Status")):
            if run.get(key) is not None:
                lines.append("- {}: `{}`".format(label, run[key]))
        dataset = run.get("dataset") or {}
        if dataset:
            lines.append("- Input data: `{}`, {} rows, columns {}, SHA-256 `{}`".format(
                dataset.get("file"), dataset.get("rows"), ", ".join(dataset.get("columns") or []), dataset.get("sha256")))
        check = run.get("input_dependence") or {}
        if check:
            lines.append("- Input-dependence check, as the template recorded it: `{}`".format(check.get("outcome")))
    lines += ["", "## Results", ""]
    for table in tables:
        lines.append("- Table `{}`: {} rows, {} columns, SHA-256 `{}`".format(
            table.get("file"), table.get("rows"), table.get("columns"), digests.get(table.get("file"), "not listed")))
    for figure in run.get("figures") or []:
        for f in figure.get("files") or []:
            lines.append("- Figure `{}`, SHA-256 `{}`".format(f, digests.get(f, "not listed")))
    for note in run.get("notes") or []:
        lines.append("- Note: " + note)
    lines += ["", "## Reviews", ""]
    reviews = analysis.get("reviews") or []
    if not reviews:
        lines.append("No person had reviewed this analysis when the package was made.")
    for r in reviews:
        who = r.get("reviewer") or "a person"
        if r.get("reviewer_kind") == "service_account":
            who += " (a service account, not a person's review)"
        lines.append("- {} by {} on {}{}".format(r.get("decision"), who, r.get("recorded_at"),
                                                ": " + r["note"] if r.get("note") else ""))
    lines += ["", "## Rerun", ""]
    if command:
        lines += ["From this folder:", "", "    " + command, ""]
        if kind == "render-template":
            lines.append("The rerun folder's `results_exact.json` should match `results/record.json`; "
                         "`rerun/verify.py --rerun` compares them.")
        else:
            lines.append("Tables in the rerun folder's `results/` should match `results/`; "
                         "`rerun/verify.py --rerun` compares them.")
    else:
        lines.append("This package is in a format the kit cannot rerun; its own README says how it was made.")
    lines += ["", "Matching checksums and a matching rerun show the files are intact and the code reproduces them. "
              "They are not statistical validation, which stays with the research team.", ""]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rerun", action="store_true", help="rerun each analysis and compare its results")
    parser.add_argument("--rscript", default="Rscript", help="how to start R; {dir} becomes the analysis folder")
    parser.add_argument("--tolerance", type=relative_tolerance, default=1e-9,
                        help="largest relative difference allowed in numbers")
    parser.add_argument("--timeout", type=int, default=3600, help="seconds allowed for each rerun")
    parser.add_argument("--formats", type=selected_formats, default=set(RERUN_FORMATS),
                        help="comma list of template, recipe and render-template (default: all three)")
    parser.add_argument("--reviewed-only", action="store_true",
                        help="rerun an analysis whose block the agent wrote only when a person accepted it")
    parser.add_argument("--work", default="extracted",
                        help="the folder in the package the results go to; a name other than extracted must be new")
    args = parser.parse_args()
    damaged = failed = incomplete = False
    print("Package:", ROOT.name)
    results = check_sums(ROOT, ROOT / "SHA256SUMS")
    outer_digests = manifest_digests(ROOT)
    bad = [(n, p) for n, p in results if p]
    print("Package checksums: {} of {} files match".format(len(results) - len(bad), len(results)))
    for name, problem in bad:
        print("  {}: {}".format(name, problem))
        damaged = True
    analyses = json.loads((ROOT / "rerun" / "analyses.json").read_text(encoding="utf-8"))["analyses"]
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", args.work):
        sys.exit("--work takes a plain folder name, such as extracted-12345")
    out = ROOT / args.work
    try:
        # A job's own folder must be new, so two jobs never write into one another's results.
        out.mkdir(exist_ok=args.work == "extracted")
        ensure_directory(out)
    except (FileExistsError, ValueError):
        sys.exit("{} already exists: give this run a folder of its own".format(args.work))
    # Under --reviewed-only the review records decide what runs, so a package whose checksums fail runs nothing.
    trusted = not (args.reviewed_only and bad)
    catalog, sas, jsl, report = [], [], [], []
    counts = {"analyses": len(analyses), "matched": 0, "failed_or_different": 0, "not_rerun": 0}
    posix_root = ROOT.as_posix().rstrip("/") + "/"

    # Unpack every retained package before processing any rerun. A render-template analysis can then resolve its source
    # by task id even if a hand-built analyses.json did not put the source first.
    unpacked = []
    for index, analysis in enumerate(analyses, 1):
        label = "{:02d}-{}".format(index, slug(analysis.get("title") or analysis["task_id"]))
        package = analysis.get("package")
        expected = analysis.get("package_sha256")
        listed = outer_digests.get(package) if isinstance(package, str) else None
        if (not safe_name(package) or not isinstance(expected, str) or listed != expected.lower()):
            print("{}: the retained package is missing or changed".format(label))
            damaged = True
            unpacked.append(None)
            continue
        dest = out / label
        try:
            with open_regular(ROOT, package) as archive:
                actual = sha256_file(archive)
                if actual != expected.lower():
                    raise ValueError("package checksum differs")
                archive.seek(0)
                files = extract(archive, dest, package)
        except (OSError, ValueError, tarfile.TarError):
            print("{}: the retained package is missing or changed".format(label))
            damaged = True
            unpacked.append(None)
            continue
        inner = check_sums(dest, dest / "SHA256SUMS")
        inner_bad = [n for n, p in inner if p]
        print("{}: {} files unpacked; {} of {} package checksums match".format(label, files, len(inner) - len(inner_bad), len(inner)))
        if inner_bad or not inner:
            damaged = True
        digests = manifest_digests(dest)
        kind, params = package_kind(dest)
        tables = result_tables(dest, kind, params, digests)
        unpacked.append({"label": label, "dest": dest, "digests": digests, "kind": kind, "params": params,
                         "tables": tables})

    def source_table(current, spec):
        source = spec.get("source")
        if not isinstance(source, dict):
            return None, "spec.json does not name a source analysis and table"
        task_id, path, expected = source.get("task_id"), source.get("table_path"), source.get("table_sha256")
        if not isinstance(task_id, str) or task_id == analyses[current].get("task_id"):
            return None, "the source task is not another analysis in rerun/analyses.json"
        candidates = [i for i, item in enumerate(analyses) if item.get("task_id") == task_id and i != current]
        if not candidates:
            return None, "the source analysis is absent from rerun/analyses.json"
        if not isinstance(path, str) or not safe_name(path) or not isinstance(expected, str):
            return None, "spec.json has an invalid source table record"
        found = listed = False
        for i in candidates:
            info = unpacked[i]
            if info is None:
                continue
            if path not in info["digests"]:
                continue
            listed = True
            if info["digests"][path].lower() != expected.lower():
                found = True
                continue
            try:
                actual = file_sha256(info["dest"], path)
            except OSError:
                continue
            found = True
            if actual == expected.lower():
                return {"base": info["dest"], "path": path, "sha256": expected.lower()}, None
        if found:
            return None, "the source table's SHA-256 differs from spec.json"
        if not listed and any(unpacked[i] is not None for i in candidates):
            return None, "the source table is not listed in its package manifest"
        if not any(unpacked[i] is not None for i in candidates):
            return None, "the source analysis package was not unpacked"
        return None, "the source table is missing or unreadable"

    for index, analysis in enumerate(analyses, 1):
        label = "{:02d}-{}".format(index, slug(analysis.get("title") or analysis["task_id"]))
        entry = {"analysis": label, "task_id": analysis["task_id"]}
        info = unpacked[index - 1]
        if info is None:
            entry["package"] = "missing or changed"
            if args.rerun:
                entry["rerun"] = "not rerun: the package is missing or changed"
                counts["not_rerun"] += 1
            report.append(entry)
            continue
        dest, digests, kind, params, tables = (info["dest"], info["digests"], info["kind"], info["params"],
                                               info["tables"])
        entry["format"] = kind
        if kind in ("template", "render-template"):
            sheet_run = params
        elif kind == "recipe":
            sheet_run = dict(load_json(dest / "run.json"), seed=params["seed"], statistic=params["statistic"],
                             recipe="code/bootstrap_replay.R")
        else:
            sheet_run = load_json(dest / "run.json")
        command = rerun_words(kind, params)
        (dest / "METHOD.md").write_text(method_sheet(analysis, kind, sheet_run, tables, digests, command),
                                        encoding="utf-8")
        for table in tables:
            path = "{}/{}/{}".format(args.work, label, table.get("file"))
            catalog.append([label, table.get("name"), path, table.get("rows"), table.get("columns"), digests.get(table.get("file"), "")])
            sas.append('proc import datafile="&root.{}" out=work.{} dbms=csv replace;\n  guessingrows=max;\nrun;'.format(
                path, sas_name("a{:02d}_{}".format(index, table.get("name") or "table"))))
            jsl.append('Open( root || "{}" );'.format(path))
        if not args.rerun:
            report.append(entry)
            continue
        if kind == "other":
            entry["rerun"] = "not rerun: no supported template run or reviewed recipe this kit can replay"
            counts["not_rerun"] += 1
            incomplete = True
            print("{}: not rerun; its package is in a format this kit cannot replay (see its README)".format(label))
            report.append(entry)
            continue
        if kind not in args.formats:
            entry["rerun"] = "not rerun: format {} was excluded by --formats".format(kind)
            counts["not_rerun"] += 1
            incomplete = True
            print("{}: not rerun; format {} was excluded by --formats".format(label, kind))
            report.append(entry)
            continue
        if not trusted:
            entry["rerun"] = "not rerun: the package's checksums do not match, so its review records cannot be trusted"
            counts["not_rerun"] += 1
            incomplete = True
            print("{}: not rerun; the package's checksums do not match".format(label))
            report.append(entry)
            continue
        if args.reviewed_only and agent_written(analysis, kind) and not person_accepted(analysis):
            entry["rerun"] = "not rerun: the agent wrote its code and no person has accepted it"
            counts["not_rerun"] += 1
            incomplete = True
            print("{}: not rerun; the agent wrote its code and no person on the project has accepted it".format(label))
            report.append(entry)
            continue
        render_paths = source = retained_record = None
        if kind == "render-template":
            render_paths, problem = render_code_files(dest, params, digests)
            if problem is None:
                source, problem = source_table(index - 1, params)
            if problem is None:
                retained_record, problem = render_record(dest, params, digests)
            if problem is not None:
                entry["rerun"] = "not rerun: " + problem
                counts["not_rerun"] += 1
                incomplete = True
                print("{}: not rerun; {}".format(label, problem))
                report.append(entry)
                continue
        folder, n = "rerun", 2
        while (dest / folder).exists() or (dest / (folder + ".tar.gz")).exists():
            folder, n = "rerun-{}".format(n), n + 1
        (dest / folder).mkdir()
        if kind == "render-template":
            try:
                copy_verified(source["base"], source["path"], dest / folder / "data.csv", source["sha256"])
                settings = render_paths["settings"]
                copy_verified(dest, settings["path"], dest / folder / "params.json", settings["sha256"])
            except (OSError, ValueError):
                entry["rerun"] = "not rerun: the source table or settings file could not be staged"
                counts["not_rerun"] += 1
                incomplete = True
                print("{}: not rerun; the source table or settings file could not be staged".format(label))
                report.append(entry)
                continue
        # Split first, then name the folder, so a folder path with spaces stays one argument.
        words = [w.replace("{dir}", str(dest)) for w in shlex.split(args.rscript, posix=os.name != "nt")]
        env = dict(os.environ)
        if kind == "template":
            env["ENRICHEDLAB_TEMPLATE_RERUN"] = "local"
        elif kind == "render-template":
            # Every variable that makes R read startup code or attach a package before the template: profile and
            # environment files, the default package lists and the test file R sources when R_TESTS is set. The
            # library paths R_LIBS and R_LIBS_SITE stay; they locate the packages the template itself loads.
            for name in R_STARTUP_VARIABLES:
                env.pop(name, None)
            recorded_session = recorded_r_session(params)
            if recorded_session:
                local_session, environment_problem = local_r_session(
                    words, dest, env, sorted(recorded_session["packages"]), args.timeout)
                environment = r_environment_comparison(recorded_session, local_session, environment_problem)
                entry["r_environment"] = environment
                for difference in environment["differences"]:
                    print("{}: R environment information: {}".format(label, difference))
        started_ns = time.time_ns()
        with open(dest / (folder + ".log"), "wb") as log:
            try:
                code = subprocess.run(words + rerun_arguments(kind, params, folder), cwd=dest, env=env, stdout=log,
                                      stderr=subprocess.STDOUT, timeout=args.timeout).returncode
            except (OSError, subprocess.TimeoutExpired) as error:
                code = "not run: " + type(error).__name__
        if kind == "recipe" and code == 0:
            try:
                archive_name = folder + ".tar.gz"
                with open_regular(dest, archive_name) as archive:
                    extract(archive, dest / folder, archive_name)
                replay = (dest / folder / "exit_code.txt").read_text(encoding="utf-8").strip()
                if replay != "0":
                    code = "recipe recorded exit " + replay[:16]
            except (OSError, ValueError, tarfile.TarError):
                code = "no readable output archive"
        if kind == "render-template":
            entry.update({"rerun_folder": folder, "exit_code": code})
            ran = code == 0
            if not ran:
                outcome = ("not compared: the rerun failed", 0, 0, 0.0)
            elif not retained_record_unchanged(dest, retained_record):
                outcome = ("refused: retained record changed during rerun", 0, 0, 0.0)
            else:
                again, again_problem = new_render_result(
                    dest, folder, started_ns, retained_record["identity"])
                if again_problem:
                    outcome = (again_problem, 0, 0, 0.0)
                else:
                    outcome = compare_json(retained_record["value"], again, args.tolerance)
            entry["record"] = {"retained": "results/record.json", "rerun": folder + "/results_exact.json",
                               "outcome": outcome[0], "numbers_compared": outcome[1],
                               "numbers_different": outcome[2], "largest_relative_difference": outcome[3]}
            matched = outcome[0] in ("identical", "within tolerance")
            refused = outcome[0].startswith("refused:")
            unreadable = "missing or unreadable" in outcome[0]
            if not ran or unreadable or refused:
                entry["rerun"] = "failed"
                counts["failed_or_different"] += 1
                failed = True
            elif not matched:
                entry["rerun"] = "differs"
                counts["failed_or_different"] += 1
                failed = True
            else:
                entry["rerun"] = "matches"
                counts["matched"] += 1
            print("{}: rerun exit {}; {}; results_exact.json {}".format(label, code, entry["rerun"], outcome[0]))
            report.append(entry)
            continue

        entry.update({"rerun_folder": folder, "exit_code": code, "tables": []})
        # A failed run fails the analysis whatever tables it left, and it is checked before any table.
        ran = code == 0
        for table in tables:
            retained, again = dest / table["file"], dest / folder / table["file"]
            if not ran:
                outcome = ("not compared: the rerun failed", None, None, None)
            elif not again.is_file():
                outcome = ("missing", None, None, None)
            else:
                outcome = compare(retained, again, args.tolerance)
            entry["tables"].append({"table": table.get("file"), "outcome": outcome[0], "cells_compared": outcome[1],
                                    "cells_different": outcome[2], "largest_relative_difference": outcome[3]})
        matched = all(t["outcome"] in ("identical", "within tolerance") for t in entry["tables"])
        if not ran or not matched:
            entry["rerun"] = "failed" if not ran else "differs"
            counts["failed_or_different"] += 1
            failed = True
        elif not tables:
            entry["rerun"] = "rerun, but no table to compare"
            counts["not_rerun"] += 1
            incomplete = True
        else:
            entry["rerun"] = "matches"
            counts["matched"] += 1
        summary = ", ".join("{} {}".format(t["table"], t["outcome"]) for t in entry["tables"]) or "no tables"
        print("{}: rerun exit {}; {}; {}".format(label, code, entry["rerun"], summary))
        report.append(entry)
    with open(out / "tables.csv", "w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["analysis", "table", "path", "rows", "columns", "sha256"])
        writer.writerows(catalog)
    (out / "import_all.sas").write_text(
        "/* Generated by rerun/verify.py: imports every retained result table into WORK. */\n"
        "/* If you move the package folder, change root to its new location (with a trailing slash). */\n"
        '%let root = {};\n\n'.format(posix_root) + "\n\n".join(sas) + "\n", encoding="utf-8")
    (out / "open_all.jsl").write_text(
        "// Generated by rerun/verify.py: opens every retained result table in JMP.\n"
        "// If you move the package folder, change root to its new location (with a trailing slash).\n"
        'root = "{}";\n'.format(posix_root) + "\n".join(jsl) + "\n", encoding="utf-8")
    if args.rerun:
        (out / "rerun_report.json").write_text(json.dumps({"tolerance": args.tolerance, "counts": counts, "analyses": report},
                                                          indent=2) + "\n", encoding="utf-8")
        print("Reruns: {matched} of {analyses} analyses rerun with matching tables; {failed_or_different} failed or differ; "
              "{not_rerun} not rerun or with nothing to compare".format(**counts))
    print("Wrote {0}/tables.csv, {0}/import_all.sas and {0}/open_all.jsl".format(args.work) +
          (" and {}/rerun_report.json".format(args.work) if args.rerun else ""))
    print("Checksums and a matching rerun are not statistical validation; that stays with the research team.")
    return 1 if damaged else 2 if failed else 3 if incomplete else 0


if __name__ == "__main__":
    sys.exit(main())
