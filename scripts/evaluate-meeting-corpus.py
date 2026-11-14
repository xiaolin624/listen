"""Freeze and evaluate the #26 real meeting corpus locally; never upload audio.

The corpus-wide denominator and edit-distance recurrence extend the existing
benchmark-meeting-corpus.ps1 evaluator. Missing, failed and timed-out samples
contribute deletions, including samples after a fatal harness error.
"""

import argparse
from decimal import Decimal, InvalidOperation
import hashlib
import json
from pathlib import Path
import re
import shutil
import sys
import unicodedata
import wave


BASELINE_COMMIT = "4b8a487e5e6cb5aaedf8bfe8084291cc24540545"
CATEGORIES = {"technical", "english", "number", "negation", "uncertainty"}
COVERAGE = {"mandarin-long", "mixed-technical", "products-projects", "numbers-units",
            "negation", "uncertainty"}


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n",
                    encoding="utf-8")


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def index_samples(samples):
    indexed = {}
    for sample in samples:
        identity = sample["id"]
        if identity in indexed:
            raise ValueError(f"Duplicate sample id: {identity}")
        indexed[identity] = sample
    return indexed


def definition_samples(path):
    definition = read_json(path)
    samples = definition["samples"]
    if len(samples) != 20 or len(index_samples(samples)) != 20:
        raise ValueError("The #26 definition must contain exactly 20 unique samples")
    return samples


def review_template(args):
    if args.output.exists():
        raise ValueError("Review already exists; choose a new output to preserve your edits")
    definition = definition_samples(args.definition)
    recordings = read_json(args.recordings) if args.recordings.exists() else {"samples": []}
    recorded = index_samples(recordings["samples"])
    rows = []
    for sample in definition:
        audio = recorded.get(sample["id"], {})
        rows.append({"id": sample["id"], "audio_sha256": audio.get("sha256"),
                     "reference": sample["reference"],
                     "human_recording_confirmed": False, "listened_to_audio": False,
                     "reference_verified": False, "reviewer": "", "reviewed_at": "",
                     "recording_conditions": "", "expressions": sample["expressions"],
                     "number_equivalences": []})
    write_json(args.output, {
        "definition_sha256": digest(args.definition),
        "recordings_manifest_sha256": digest(args.recordings) if args.recordings.exists() else None,
        "evidence_kind": "unverified", "samples": rows,
        "instructions": "Listen to each WAV. Verify the actual words, human provenance and recording conditions. Fill every review; do not copy recognizer output as reference. Number equivalences require verified=true and a reason. Tool fixtures must be labelled tooling-fixture and never qualify as quality evidence.",
    })


def freeze(args):
    definition = definition_samples(args.definition)
    recordings, review = read_json(args.recordings), read_json(args.review)
    recorded, reviewed = index_samples(recordings["samples"]), index_samples(review["samples"])
    identities = {sample["id"] for sample in definition}
    if (set(recorded) != identities or set(reviewed) != identities
            or any(not all(reviewed[identity].get(flag) is True for flag in (
                "human_recording_confirmed", "listened_to_audio", "reference_verified"))
                   for identity in identities)):
        raise ValueError("20 verified real samples and listened reference reviews are required; no frozen corpus was created")
    if (review["definition_sha256"] != digest(args.definition)
            or recordings.get("corpus_sha256") != digest(args.definition)
            or review["recordings_manifest_sha256"] != digest(args.recordings)):
        raise ValueError("Definition or recordings changed after reference review; regenerate and review")
    evidence_kind = review.get("evidence_kind")
    if evidence_kind not in ("real-human-speech", "tooling-fixture"):
        raise ValueError("Set evidence_kind to real-human-speech only after verifying the human source")
    if recordings.get("kind") != "real-meeting-evaluation" or recordings.get("generator"):
        raise ValueError("Synthetic/legacy manifests cannot become the #26 real corpus")
    coverage = {value for sample in definition for value in sample.get("coverage", [])}
    if not COVERAGE <= coverage:
        raise ValueError("The definition does not cover all required meeting cases")
    rows, hashes = [], set()
    for planned in definition:
        identity = planned["id"]
        audio, checked = recorded[identity], reviewed[identity]
        for field in ("reviewer", "reviewed_at", "recording_conditions", "reference"):
            if not isinstance(checked.get(field), str) or not checked[field].strip():
                raise ValueError(f"Missing {field}: {identity}")
        path = Path(audio.get("file") or audio.get("audio", ""))
        if not path.is_absolute():
            path = args.recordings.resolve().parent / path
        checksum = digest(path)
        if checksum != audio.get("sha256") or checksum != checked.get("audio_sha256"):
            raise ValueError(f"Audio changed since recording/review: {identity}")
        if checksum in hashes:
            raise ValueError(f"Duplicate audio cannot count as another real sample: {identity}")
        hashes.add(checksum)
        with wave.open(str(path)) as stream:
            if (stream.getnchannels(), stream.getsampwidth(), stream.getframerate()) != (1, 2, 16000):
                raise ValueError(f"Expected 16 kHz mono 16-bit PCM WAV: {identity}")
            duration = stream.getnframes() / 16000
            if duration < 1 or len(stream.readframes(stream.getnframes())) != stream.getnframes() * 2:
                raise ValueError(f"Empty/truncated audio: {identity}")
        for expression in checked["expressions"]:
            if expression["category"] not in CATEGORIES or expression["expected"] not in checked["reference"]:
                raise ValueError(f"Key expression is not in the verified reference: {identity}")
        for equivalence in checked.get("number_equivalences", []):
            if (equivalence.get("verified") is not True or not equivalence.get("reason")
                    or not number_pattern(equivalence["canonical"]).search(checked["reference"])
                    or not equivalence.get("variants")):
                raise ValueError(f"Number-format equivalence needs a verified meaning and reason: {identity}")
            validate_number_equivalence(equivalence)
        rows.append({"id": identity, "audio": str(path.resolve()), "sha256": checksum,
                     "duration_seconds": duration, "reference": checked["reference"],
                     "reference_sha256": hashlib.sha256(checked["reference"].encode("utf-8")).hexdigest(),
                     "coverage": planned["coverage"], "expressions": checked["expressions"],
                     "number_equivalences": checked.get("number_equivalences", []),
                     "recorded_at": audio.get("recorded_at"),
                     "endpoint_name": audio.get("endpoint_name"),
                     "capture_source": audio.get("capture_source"),
                     "recording_conditions": checked["recording_conditions"],
                     "reference_review": {key: checked[key] for key in (
                         "reviewer", "reviewed_at", "human_recording_confirmed",
                         "listened_to_audio", "reference_verified")}})
    if not CATEGORIES <= {item["category"] for row in rows for item in row["expressions"]}:
        raise ValueError("Key-expression reviews must cover all five categories")
    baseline = verify_baseline(args.baseline)
    if evidence_kind == "real-human-speech" and baseline.get("kind") != "local-baseline-build":
        raise ValueError("A real corpus requires an actual fixed local-baseline build receipt")
    if args.output.exists() and any(args.output.iterdir()):
        raise ValueError("Frozen output already exists; choose a new directory")
    args.output.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(args.baseline, args.output / "baseline.json")
    shutil.copyfile(baseline["artifacts"]["glossary"]["path"], args.output / "glossary.txt")
    write_json(args.output / "manifest.json", {
        "schema_version": 2, "kind": "frozen-meeting-evaluation",
        "evidence_kind": evidence_kind, "required_sample_count": 20,
        "recording_chain": recordings["recording_chain"],
        "definition_sha256": digest(args.definition),
        "recordings_manifest_sha256": digest(args.recordings), "reference_review_sha256": digest(args.review),
        "baseline": {"file": "baseline.json", "sha256": digest(args.output / "baseline.json"),
                     "git_commit": baseline["git_commit"]},
        "glossary": {"file": "glossary.txt", "sha256": digest(args.output / "glossary.txt")},
        "samples": rows,
    })
    print(f"Frozen {len(rows)} samples ({evidence_kind}); no recognition or upload was performed")


def verify_baseline(path):
    baseline = read_json(path)
    if baseline.get("git_commit") != BASELINE_COMMIT or baseline.get("engine") != "sensevoice":
        raise ValueError("Baseline must be the fixed current local SenseVoice implementation at " + BASELINE_COMMIT)
    if not baseline.get("build_succeeded"):
        raise ValueError("Baseline build did not succeed")
    for role, artifact in baseline["artifacts"].items():
        if digest(artifact["path"]) != artifact["sha256"]:
            raise ValueError(f"Baseline artifact changed: {role}")
    if baseline.get("kind") == "local-baseline-build":
        required = {"listen_binary", "glossary", "model", "tokens", "vad",
                    "sherpa", "onnxruntime", "libunwind", "build_log"}
        if not required <= baseline["artifacts"].keys() or not baseline.get("source_clean"):
            raise ValueError("Baseline receipt lacks the clean build/configuration artifacts")
    return baseline


def verify_frozen(path, manifest):
    if manifest.get("evidence_kind") == "real-human-speech":
        if (manifest.get("schema_version") != 2 or manifest.get("kind") != "frozen-meeting-evaluation"
                or len(index_samples(manifest["samples"])) != 20):
            raise ValueError("A real corpus must be frozen with exactly 20 unique samples")
        hashes = set()
        for sample in manifest["samples"]:
            review = sample.get("reference_review", {})
            if (not all(review.get(flag) is True for flag in (
                    "human_recording_confirmed", "listened_to_audio", "reference_verified"))
                    or not all(review.get(field) for field in ("reviewer", "reviewed_at"))
                    or not sample.get("recording_conditions") or not sample.get("reference_sha256")):
                raise ValueError(f"Reference verification is incomplete: {sample['id']}")
            if not sample.get("sha256") or sample["sha256"] in hashes:
                raise ValueError("Real audio identities/hashes must be complete and unique")
            hashes.add(sample["sha256"])
    for sample in manifest["samples"]:
        if sample.get("audio") and digest(sample["audio"]) != sample["sha256"]:
            raise ValueError(f"Audio changed: {sample['id']}")
        if sample.get("reference_sha256") and hashlib.sha256(
                sample["reference"].encode("utf-8")).hexdigest() != sample["reference_sha256"]:
            raise ValueError(f"Reference changed: {sample['id']}")
    for role in ("glossary", "baseline"):
        if role in manifest:
            artifact = manifest[role]
            artifact_path = path.resolve().parent / artifact["file"]
            if digest(artifact_path) != artifact["sha256"]:
                raise ValueError(f"Frozen {role} changed")
    if "baseline" in manifest:
        baseline = verify_baseline(path.resolve().parent / manifest["baseline"]["file"])
        if manifest.get("glossary", {}).get("sha256") != baseline["artifacts"]["glossary"]["sha256"]:
            raise ValueError("Frozen glossary differs from the actual baseline configuration")
        return baseline
    return None


def normalize(text):
    text = unicodedata.normalize("NFKC", text).casefold().replace("\u2212", "-")
    kept = []
    for index, char in enumerate(text):
        before = text[index - 1] if index else ""
        after = text[index + 1] if index + 1 < len(text) else ""
        # Version/decimal/time/ratio separators and numeric signs carry meaning.
        # Symbols such as %, + and currency units are never discarded either.
        semantic = (char in "%‰‱"
                    or char in ".:/," and before.isdigit() and after.isdigit()
                    or char == "-" and after.isdigit()
                    or char == "#" and before.isascii() and before.isalpha())
        if (not char.isspace() and char not in "\ufeff\u200b\u200c\u200d"
                and (not unicodedata.category(char).startswith("P") or semantic)):
            kept.append(char)
    return "".join(kept)


NUMBER_CHARS = "0-9零〇一二两三四五六七八九十百千万亿点"
DIGITS = dict(zip("零〇一二两三四五六七八九", "001223456789"))


def numeric_signature(value):
    """Conservatively prove numeric spelling equivalence; never rewrite words.

    Unsupported colloquial numbers/time expressions remain content differences
    for human review, rather than accepting an arbitrary text replacement.
    """
    value = re.sub(r"\s+", "", unicodedata.normalize("NFKC", value)).replace("−", "-")
    if value.startswith("百分之"):
        value = value[3:] + "%"
    for prefix in ("零下", "负"):
        if value.startswith(prefix):
            value = "-" + value[len(prefix):]
    match = re.fullmatch(r"([+-]?)([0-9零〇一二两三四五六七八九十百千万亿点.:]+)([^0-9零〇一二两三四五六七八九十百千万亿]*)", value)
    if not match:
        return None
    sign, number, unit = match.groups()
    if number.endswith("点"):
        number, unit = number[:-1], "点" + unit
    number = number.replace("点", ".")

    def digit_string(part):
        return "".join(DIGITS.get(char, char) for char in part)

    def integer(part):
        if not part:
            raise ValueError("Incomplete number")
        if not part.isascii():
            # Adjacent spoken digits may express a range: 两三 != 23 and
            # 一两 != 12. Only zero-containing digit sequences (e.g. 八零八零)
            # and formal, fully expressed place values are supported.
            digit_sequence = (all(char in DIGITS and char != "两" for char in part)
                              and any(char in "零〇" for char in part))
            ones = "[一二三四五六七八九]"
            leading = "[一二两三四五六七八九]"
            tens = f"(?:十{ones}?|[二三四五六七八九]十{ones}?)"
            hundreds = f"{leading}百(?:零{ones}|{ones}十{ones}?)?"
            thousands = f"{leading}千(?:零(?:{ones}|{tens})|{hundreds})?"
            if not digit_sequence and not re.fullmatch(f"(?:{ones}|两|{tens}|{hundreds}|{thousands})", part):
                raise ValueError("Unsupported or ambiguous Chinese number")
        digits = digit_string(part)
        if digits.isascii() and digits.isdigit():
            return int(digits)
        total, section, current = 0, 0, 0
        for char in part:
            if char in DIGITS:
                current = int(DIGITS[char])
            elif char in "十百千":
                section += (current or 1) * {"十": 10, "百": 100, "千": 1000}[char]
                current = 0
            else:
                raise ValueError("Unsupported number spelling")
        return total + section + current

    try:
        if ":" in number or number.count(".") > 1:
            separator = ":" if ":" in number else "."
            return ("time" if separator == ":" else "version", sign,
                    tuple(integer(part) for part in number.split(separator)), unit)
        if "." in number:
            whole, fraction = number.split(".")
            if "两" in fraction and len(fraction) > 1:
                return None
            fraction = digit_string(fraction)
            if not fraction or not fraction.isascii() or not fraction.isdigit():
                return None
            numeric = Decimal(f"{integer(whole)}.{fraction}")
        else:
            numeric = Decimal(integer(number))
        return ("number", -numeric if sign == "-" else numeric, unit)
    except (ValueError, InvalidOperation):
        return None


def validate_number_equivalence(entry):
    signature = numeric_signature(entry["canonical"])
    if signature is None or any(numeric_signature(variant) != signature for variant in entry["variants"]):
        raise ValueError("Number-format equivalence changes numeric meaning or uses unsupported syntax; retain it as a content difference")


def number_pattern(value):
    return re.compile(f"(?<![{NUMBER_CHARS}]){re.escape(value)}(?![{NUMBER_CHARS}])")


def normalize_numbers(text, equivalences):
    differences = []
    for entry in equivalences:
        validate_number_equivalence(entry)
        canonical = entry["canonical"]
        for variant in entry["variants"]:
            pattern = number_pattern(variant)
            if pattern.search(text):
                differences.append({"canonical": canonical, "observed": variant})
                text = pattern.sub(lambda match: canonical, text)
    return text, differences


def writing_differences(reference, text):
    punctuation = lambda value: "".join(
        char for char in value if unicodedata.category(char).startswith("P"))
    uppercase = lambda value: "".join(char for char in value if char.isupper())
    return {"punctuation": punctuation(reference) != punctuation(text),
            "case": uppercase(reference) != uppercase(text),
            "whitespace": re.findall(r"\s+", reference) != re.findall(r"\s+", text)}


def edits(reference, hypothesis):
    # The same Levenshtein recurrence as the replay harness, with a stable
    # traceback so insertions/deletions/substitutions are separately visible.
    matrix = [[0] * (len(hypothesis) + 1) for _ in range(len(reference) + 1)]
    for row in range(len(reference) + 1):
        matrix[row][0] = row
    for col in range(len(hypothesis) + 1):
        matrix[0][col] = col
    for row, left in enumerate(reference, 1):
        for col, right in enumerate(hypothesis, 1):
            matrix[row][col] = min(matrix[row - 1][col] + 1,
                                   matrix[row][col - 1] + 1,
                                   matrix[row - 1][col - 1] + (left != right))
    counts = {"substitutions": 0, "deletions": 0, "insertions": 0}
    row, col = len(reference), len(hypothesis)
    while row or col:
        if row and col and matrix[row][col] == matrix[row - 1][col - 1] + (
                reference[row - 1] != hypothesis[col - 1]):
            counts["substitutions"] += reference[row - 1] != hypothesis[col - 1]
            row, col = row - 1, col - 1
        elif row and matrix[row][col] == matrix[row - 1][col] + 1:
            counts["deletions"] += 1
            row -= 1
        else:
            counts["insertions"] += 1
            col -= 1
    return counts


def score(args):
    for source in (args.manifest, args.results, args.reviews):
        if source and source.resolve() in (args.output.resolve(), args.output.with_suffix(".md").resolve()):
            raise ValueError("Evaluation output must not overwrite its source evidence")
    manifest, run = read_json(args.manifest), read_json(args.results)
    baseline = verify_frozen(args.manifest, manifest)
    manifest_sha, results_sha = digest(args.manifest), digest(args.results)
    if run.get("corpus_manifest_sha256") != manifest_sha:
        raise ValueError("Results belong to a different frozen manifest (or have no manifest hash)")
    by_id = index_samples(run["samples"])
    if not set(by_id) <= set(index_samples(manifest["samples"])):
        raise ValueError("Results contain unknown sample identities")
    total = {"substitutions": 0, "deletions": 0, "insertions": 0}
    rows, reference_chars, unattempted, attempted = [], 0, 0, 0
    keys = []
    for sample in manifest["samples"]:
        result = by_id.get(sample["id"], {"phase": "NotRun", "text": ""})
        phase = result["phase"]
        if phase not in ("Result", "NoSpeech", "Failure", "Timeout", "NotRun"):
            raise ValueError(f"Unknown result phase: {phase}")
        if sample.get("sha256") and sample["id"] in by_id and result.get("sha256") != sample["sha256"]:
            raise ValueError(f"Result used different audio: {sample['id']}")
        if "reference" in result and result["reference"] != sample["reference"]:
            raise ValueError(f"Result used a different reference: {sample['id']}")
        text = result.get("text", "") if phase == "Result" else ""
        if not isinstance(text, str):
            raise ValueError(f"Result text is not a string: {sample['id']}")
        equivalent_text, number_differences = normalize_numbers(
            text, sample.get("number_equivalences", []))
        reference, hypothesis = normalize(sample["reference"]), normalize(equivalent_text)
        if not reference:
            raise ValueError(f"Empty content reference cannot be scored as a real utterance: {sample['id']}")
        counts = edits(reference, hypothesis)
        reference_chars += len(reference)
        unattempted += phase == "NotRun"
        attempted += (phase != "NotRun" and isinstance(result.get("wall_stop_to_result_ms"), (float, int)))
        for name, count in counts.items():
            total[name] += count
        rows.append({"id": sample["id"], "phase": phase,
                     "reference": sample["reference"], "text": text,
                     "reported_text": result.get("text", ""), "error": result.get("error"),
                     "stop_to_result_ms": result.get("stop_to_result_ms"),
                     "wall_stop_to_result_ms": result.get("wall_stop_to_result_ms"),
                     "reference_chars": len(reference), "edits": counts,
                     "content_cer": sum(counts.values()) / len(reference) if reference else None,
                     "number_format_differences": number_differences,
                     "writing_differences": writing_differences(sample["reference"], text)})
        for index, expression in enumerate(sample.get("expressions", []), 1):
            keys.append({"id": f"{sample['id']}:{index:02}", "sample": sample["id"],
                         "category": expression["category"], "expected": expression["expected"],
                         "hypothesis": text, "phase": phase,
                         "presence_hint": normalize(expression["expected"]) in normalize(equivalent_text),
                         "verdict": "pending", "observed": "", "note": ""})
    review_keys(args, keys, manifest_sha, results_sha)
    pending = sum(item["verdict"] == "pending" for item in keys)
    prerequisites = []
    if manifest.get("evidence_kind") != "real-human-speech":
        prerequisites.append("This corpus is unverified or a tooling fixture; it is not real quality evidence")
    if len(rows) != 20:
        prerequisites.append("Exactly 20 frozen samples are required")
    provenance_matches = bool(baseline and baseline.get("kind") == "local-baseline-build"
                              and run.get("baseline_receipt_sha256") == manifest["baseline"]["sha256"]
                              and run.get("listen_binary_sha256") == baseline["artifacts"]["listen_binary"]["sha256"]
                              and run.get("listen_args") == baseline["listen_args"]
                              and run.get("listen_mode") == "launched"
                              and run.get("baseline_artifacts_verified") is True)
    if not provenance_matches:
        prerequisites.append("The fixed current-local binary/configuration/glossary was not verified for this run")
    if attempted != 20 or unattempted or run.get("fatal_error"):
        prerequisites.append("All 20 original samples must actually run; failures/timeouts remain in the score")
    if pending or not keys:
        prerequisites.append("Every technical/English/number/negation/uncertainty expression needs a manual verdict")
    report = {
        "schema_version": 2, "evidence_kind": manifest.get("evidence_kind"),
        "corpus_manifest_sha256": manifest_sha, "results_sha256": results_sha,
        "acceptance_prerequisites_met": not prerequisites,
        "unmet_prerequisites": prerequisites,
        "metric_policy": "Corpus micro CER = sum substitutions + deletions + insertions / all reference characters. NFKC and case-fold; drop spaces and non-semantic punctuation; preserve symbols, numeric signs and numeric separators. Only explicitly reviewed sample-specific number formats are equivalent. Failed/NoSpeech/Timeout/NotRun samples use empty text, never an old/partial result. Writing differences and key expressions are separate reviews. Stop-to-result is not complete target-delivery latency.",
        "summary": {"reference_chars": reference_chars, "edits": total,
                    "content_cer": sum(total.values()) / reference_chars if reference_chars else None,
                    "sample_count": len(rows), "actual_attempt_count": attempted,
                    "unattempted_count": unattempted,
                    "failed_count": sum(row["phase"] in ("Failure", "Timeout") for row in rows),
                    "no_speech_count": sum(row["phase"] == "NoSpeech" for row in rows),
                    "key_expression_pending_count": pending,
                    "key_expression_error_count": sum(item["verdict"] == "fail" for item in keys)},
        "samples": rows, "key_expressions": keys,
    }
    write_json(args.output, report)
    write_report(args.output.with_suffix(".md"), report)
    print(f"Content CER: {report['summary']['content_cer']}; acceptance prerequisites met: {not prerequisites}")


def review_keys(args, keys, manifest_sha, results_sha):
    if args.reviews:
        review = read_json(args.reviews)
        if review.get("corpus_manifest_sha256") != manifest_sha or review.get("results_sha256") != results_sha:
            raise ValueError("Key-expression review belongs to different audio/references/results")
        checked = index_samples(review["items"])
        if set(checked) != {item["id"] for item in keys}:
            raise ValueError("Key-expression review identities do not match")
        for item in keys:
            row = checked[item["id"]]
            if row["expected"] != item["expected"] or row["category"] != item["category"]:
                raise ValueError("Key-expression review changed the expected fact")
            verdict = row.get("verdict", "pending")
            if verdict not in ("pass", "fail", "pending"):
                raise ValueError("Review verdict must be pass, fail or pending")
            if verdict != "pending" and not all(str(value).strip() for value in (
                    review.get("reviewer", ""), review.get("reviewed_at", ""), row.get("observed", ""))):
                raise ValueError("A verdict requires reviewer, date and observed text (or explicit omission)")
            item.update(verdict=verdict, observed=row.get("observed", ""), note=row.get("note", ""))
    else:
        output = args.output.with_name(args.output.stem + "-key-review.json")
        if not output.exists():
            write_json(output, {"corpus_manifest_sha256": manifest_sha, "results_sha256": results_sha,
                                "reviewer": "", "reviewed_at": "", "items": keys})


def write_report(path, report):
    cell = lambda text: ("" if text is None else str(text)).replace("|", "\\|").replace("\n", " ")
    summary = report["summary"]
    lines = ["# Meeting corpus evaluation", "",
             f"Acceptance prerequisites met: **{report['acceptance_prerequisites_met']}**", "",
             f"Evidence: {report['evidence_kind']}. Corpus micro content CER: {summary['content_cer']}. "
             f"Reference characters: {summary['reference_chars']}; edits: {summary['edits']}.", "",
             "This report does not establish streaming stop-to-complete-delivery latency.", ""]
    lines.extend("- " + value for value in report["unmet_prerequisites"])
    lines += ["", "| sample | phase | content CER | stop→result ms | reference | full text |",
              "| --- | --- | ---: | ---: | --- | --- |"]
    for row in report["samples"]:
        lines.append("| " + " | ".join(cell(row[key]) for key in (
            "id", "phase", "content_cer", "stop_to_result_ms", "reference", "text")) + " |")
    lines += ["", "| sample | writing differences (not content errors) | reviewed equivalent number formats |",
              "| --- | --- | --- |"]
    for row in report["samples"]:
        lines.append(f"| {cell(row['id'])} | {cell(row['writing_differences'])} | "
                     f"{cell(row['number_format_differences'])} |")
    lines += ["", "| expression | category | expected | verdict | observed | full hypothesis |",
              "| --- | --- | --- | --- | --- | --- |"]
    for item in report["key_expressions"]:
        lines.append("| " + " | ".join(cell(item[key]) for key in (
            "id", "category", "expected", "verdict", "observed", "hypothesis")) + " |")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    scorer = commands.add_parser("score")
    for flag in ("manifest", "results", "output"):
        scorer.add_argument("--" + flag, type=Path, required=True)
    scorer.add_argument("--reviews", type=Path)
    template = commands.add_parser("review-template")
    freezer = commands.add_parser("freeze")
    for command in (template, freezer):
        for flag in ("definition", "recordings", "output"):
            command.add_argument("--" + flag, type=Path, required=True)
    freezer.add_argument("--review", type=Path, required=True)
    freezer.add_argument("--baseline", type=Path, required=True)
    args = parser.parse_args()
    try:
        {"score": score, "review-template": review_template, "freeze": freeze}[args.command](args)
    except (ValueError, KeyError, OSError) as error:
        parser.exit(2, f"{error}\n")


if __name__ == "__main__":
    main()
