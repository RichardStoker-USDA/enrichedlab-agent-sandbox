#!/usr/bin/env python3
"""Measure replaceable typed decisions against public, explicitly labelled cases."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import stat
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, build_opener, HTTPRedirectHandler


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def sha(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def probability(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 <= value <= 1:
        raise ValueError("invalid probability")
    return value


def validate_answers(questions, response):
    if not isinstance(response.get("model"), str) or not response["model"]:
        raise ValueError("resolved model identity missing")
    answers = response["answers"]
    if set(answers) != set(questions):
        raise ValueError("answer identifiers differ")
    for name, q in questions.items():
        a = answers[name]
        if a["type"] != q["type"]:
            raise ValueError("answer type differs")
        if q["type"] == "noul":
            probability(a["noul"])
            continue
        p = a["probabilities"]
        labels = set(q["criteria"]) if q["type"] == "choice" else {str(i) for i in range(len(q["criteria"]))}
        if set(p) != labels or not math.isclose(sum(probability(v) for v in p.values()), 1, abs_tol=0.001):
            raise ValueError("probability distribution differs from rubric")
        probability(a["confidence"])
        if q["type"] == "choice":
            if a["choice"] not in labels or p[a["choice"]] < max(p.values()):
                raise ValueError("choice differs from distribution")
        elif q["type"] == "score":
            weighted = sum(int(k) * v for k, v in p.items())
            if not math.isclose(a["score"], weighted, abs_tol=0.001):
                raise ValueError("score differs from distribution")
        else:
            raise ValueError("unsupported answer type")
    return answers


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class SystemOne:
    """Provider boundary: structured state + rubric -> checked typed answers.

    An on-prem service implementing this protocol needs only a config change.
    Other protocols implement evaluate() and return this same checked shape.
    """
    def __init__(self, config, key_file):
        self.config = config
        if config["protocol"] != "systemone-v1":
            raise ValueError("unsupported provider protocol")
        u = urlsplit(config["endpoint"])
        if u.username or u.password or u.query or u.fragment or not u.hostname:
            raise ValueError("invalid provider endpoint")
        if u.scheme != "https" and not (u.scheme == "http" and u.hostname in {"localhost", "127.0.0.1", "::1"}):
            raise ValueError("TLS required outside loopback")
        if key_file:
            s = key_file.lstat()
            if not stat.S_ISREG(s.st_mode) or stat.S_IMODE(s.st_mode) != 0o600:
                raise ValueError("key must be a regular mode-600 file")
        self.key_file = key_file
        self.opener = build_opener(NoRedirect)

    def evaluate(self, state, questions):
        body = canonical({"state": state, "questions": questions, "model": self.config["model"]})
        if len(body) > 64 * 1024:
            raise ValueError("candidate request byte limit exceeded")
        headers = {"Content-Type": "application/json"}
        if self.key_file:
            headers["Authorization"] = "Bearer " + self.key_file.read_text().strip()
        started = time.monotonic()
        for attempt in range(3):
            try:
                request = Request(self.config["endpoint"], data=body, headers=headers, method="POST")
                with self.opener.open(request, timeout=15) as r:
                    raw = r.read(1024 * 1024 + 1)
                    if len(raw) > 1024 * 1024:
                        raise ValueError("provider response exceeds byte limit")
                response = json.loads(raw)
                answers = validate_answers(questions, response)
                return {"answers": answers, "model": response["model"], "usage": response.get("usage", {}),
                        "elapsed_ms": round((time.monotonic() - started) * 1000, 3), "attempts": attempt + 1}
            except HTTPError as e:
                code = e.code
                retry = e.headers.get("Retry-After", "")
                e.close()
                if code not in {429, 529} or attempt == 2:
                    raise ValueError(f"provider HTTP {code}") from None
                delay = min(10, max(1, float(retry))) if retry.isdigit() else 2 ** attempt
                time.sleep(delay)
            except (URLError, TimeoutError):
                raise ValueError("provider transport failed") from None


def evaluate_cases(config, cases, key_file, output):
    if output.exists():
        raise ValueError("preserve the existing measurement")
    if cases["data_class"] != "public_synthetic_examples":
        raise ValueError("this candidate runner accepts public synthetic examples only")
    client = SystemOne(config, key_file)
    output.mkdir(parents=True)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=Path(__file__).parent, text=True).strip()
    measurements = []
    for case in cases["cases"]:
        questions = cases["rubrics"][case["rubric"]]
        record = {"schema": "typed-decision-measurement/v1", "case_id": case["id"], "rubric_id": case["rubric"],
                  "rubric_sha256": sha(questions), "state_sha256": sha(case["state"]),
                  "state_ref": f"fixture://{cases['version']}/{case['id']}", "provider": config["name"],
                  "provider_config_sha256": sha(config), "source_revision": revision,
                  "at": datetime.now(timezone.utc).isoformat(), "authority": "advisory", "calibration": "pilot_only"}
        try:
            record.update(client.evaluate(case["state"], questions))
            record["matches"] = {}
            record["squared_errors"] = {}
            for key, expected in case["expected"].items():
                a = record["answers"][key]
                actual = a.get("choice") if a["type"] == "choice" else a.get("noul") if a["type"] == "noul" else a["score"]
                match = actual == expected if isinstance(expected, str) else abs(actual - expected) <= 0.2
                record["matches"][key] = match
                if a["type"] == "choice":
                    record["squared_errors"][key] = sum((p - int(label == expected)) ** 2 for label, p in a["probabilities"].items())
                elif a["type"] == "noul":
                    record["squared_errors"][key] = (actual - expected) ** 2
            record["status"] = "measured"
        except (ValueError, KeyError, TypeError, json.JSONDecodeError) as e:
            # Keep only error class, never a provider body, request, or credential.
            record.update(status="unavailable", error_class=type(e).__name__)
        (output / f"{case['id']}.json").write_text(json.dumps(record, indent=2) + "\n")
        measurements.append(record)
    successful = [x for x in measurements if x["status"] == "measured"]
    scores = [v for x in successful for v in x["matches"].values()]
    latencies = sorted(x["elapsed_ms"] for x in successful)
    summary = {"cases": len(measurements), "responses": len(successful), "matched_decisions": sum(scores),
               "labelled_decisions": len(scores), "mismatches": [{"case_id": x["case_id"], "question": k} for x in successful for k, v in x["matches"].items() if not v],
               "median_ms": latencies[len(latencies) // 2] if latencies else None,
               "max_ms": max(latencies) if latencies else None,
               "models": sorted({x["model"] for x in successful}),
               "calibration": "Small development pilot, not a calibration or scientific validation claim",
               "authority": "advisory; no actions or scientific values originate from these answers"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--config", required=True, type=Path)
    p.add_argument("--cases", required=True, type=Path)
    p.add_argument("--key-file", type=Path)
    p.add_argument("--output", required=True, type=Path)
    a = p.parse_args()
    evaluate_cases(json.loads(a.config.read_text()), json.loads(a.cases.read_text()), a.key_file, a.output)


if __name__ == "__main__":
    main()
