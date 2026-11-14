"""CLI behavior fixtures only: these are never recognition-quality evidence."""

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import wave


SCRIPT = Path(__file__).resolve().parents[1] / "evaluate-meeting-corpus.py"
REPOSITORY = SCRIPT.parent.parent
BASELINE_COMMIT = "4b8a487e5e6cb5aaedf8bfe8084291cc24540545"


def save_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")
    return path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class EvaluationCommandTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="listen-tooling-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def command(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *map(str, args)],
                              capture_output=True, encoding="utf-8")

    def frozen_fixture(self):
        definition = REPOSITORY / "quality/cloud-meeting-corpus.json"
        samples = json.loads(definition.read_text(encoding="utf-8"))["samples"]
        recorded = []
        for index, sample in enumerate(samples):
            audio = self.root / (sample["id"] + ".wav")
            with wave.open(str(audio), "wb") as stream:
                stream.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
                stream.writeframes((index + 1).to_bytes(2, "little") * 16000)
            recorded.append({"id": sample["id"], "file": audio.name,
                             "sha256": sha(audio), "recorded_at": "2026-11-14T12:00:00+08:00"})
        recordings = save_json(self.root / "recordings.json", {
            "kind": "real-meeting-evaluation", "corpus_sha256": sha(definition),
            "recording_chain": "TOOLING FIXTURE, no human or quality evidence",
            "samples": recorded,
        })
        review = self.root / "review.json"
        process = self.command("review-template", "--definition", definition,
                               "--recordings", recordings, "--output", review)
        self.assertEqual(process.returncode, 0, process.stderr)
        content = json.loads(review.read_text(encoding="utf-8"))
        content["evidence_kind"] = "tooling-fixture"
        for sample in content["samples"]:
            sample.update(human_recording_confirmed=True, listened_to_audio=True,
                          reference_verified=True, reviewer="tooling fixture",
                          reviewed_at="2026-11-14T12:00:00+08:00",
                          recording_conditions="TOOLING FIXTURE, not a real recording")
        save_json(review, content)
        binary = self.root / "not-an-executable.txt"
        binary.write_text("tooling fixture", encoding="utf-8")
        glossary = self.root / "glossary.txt"
        glossary.write_text("WASAPI\nSenseVoice\n", encoding="utf-8")
        receipt = save_json(self.root / "baseline.json", {
            "kind": "tooling-fixture", "git_commit": BASELINE_COMMIT,
            "engine": "sensevoice", "build_succeeded": True,
            "artifacts": {"listen_binary": {"path": str(binary), "sha256": sha(binary)},
                          "glossary": {"path": str(glossary), "sha256": sha(glossary)}},
            "listen_args": ["--engine", "sensevoice", "--glossary", str(glossary)],
        })
        output = self.root / "frozen"
        process = self.command("freeze", "--definition", definition, "--recordings", recordings,
                               "--review", review, "--baseline", receipt, "--output", output)
        self.assertEqual(process.returncode, 0, process.stderr)
        return output / "manifest.json"

    def score(self, samples, results):
        manifest = save_json(self.root / "manifest.json", {
            "schema_version": 2, "evidence_kind": "tooling-fixture",
            "samples": samples,
        })
        run = save_json(self.root / "run.json", {
            "corpus_manifest_sha256": sha(manifest), "samples": results,
        })
        output = self.root / "report.json"
        process = self.command("score", "--manifest", manifest, "--results", run,
                               "--output", output)
        self.assertEqual(process.returncode, 0, process.stderr)
        return json.loads(output.read_text(encoding="utf-8"))

    def test_unattempted_and_failed_samples_remain_in_corpus_cer(self):
        report = self.score([
            {"id": "long", "reference": "甲乙丙丁", "expressions": []},
            {"id": "failed", "reference": "戊己", "expressions": []},
            {"id": "missing", "reference": "庚辛壬癸", "expressions": []},
        ], [
            {"id": "long", "phase": "Result", "text": "甲乙丙错多",
             "wall_stop_to_result_ms": 100},
            {"id": "failed", "phase": "Failure", "text": "old result",
             "wall_stop_to_result_ms": 200},
        ])
        # Worked example: one substitution + one insertion + six deletions.
        self.assertEqual(report["summary"]["reference_chars"], 10)
        self.assertEqual(report["summary"]["edits"], {
            "substitutions": 1, "deletions": 6, "insertions": 1,
        })
        self.assertEqual(report["summary"]["content_cer"], 0.8)
        self.assertEqual(report["summary"]["unattempted_count"], 1)
        self.assertFalse(report["acceptance_prerequisites_met"])

    def test_numeric_meaning_negation_and_uncertainty_survive_normalization(self):
        report = self.score([
            {"id": "version", "reference": "版本1.8.2", "expressions": []},
            {"id": "negative", "reference": "温度-3度", "expressions": []},
            {"id": "negation", "reference": "不要", "expressions": []},
            {"id": "uncertain", "reference": "可能", "expressions": []},
            {"id": "equivalent", "reference": "等待2秒", "expressions": [],
             "number_equivalences": [{"canonical": "2秒", "variants": ["两秒"]}]},
            {"id": "case", "reference": "API，正常。", "expressions": []},
        ], [
            {"id": "version", "phase": "Result", "text": "版本18.2"},
            {"id": "negative", "phase": "Result", "text": "温度3度"},
            {"id": "negation", "phase": "Result", "text": "要"},
            {"id": "uncertain", "phase": "NoSpeech", "text": "old text"},
            {"id": "equivalent", "phase": "Result", "text": "等待两秒"},
            {"id": "case", "phase": "Result", "text": "api 正常"},
        ])
        self.assertEqual(report["summary"]["edits"], {
            "substitutions": 0, "deletions": 5, "insertions": 0,
        })
        equivalent, case = report["samples"][-2:]
        self.assertEqual(equivalent["number_format_differences"], [
            {"canonical": "2秒", "observed": "两秒"},
        ])
        self.assertTrue(case["writing_differences"]["case"])
        self.assertTrue(case["writing_differences"]["punctuation"])

    def test_number_equivalence_cannot_hide_signed_chinese_or_arabic_numbers(self):
        # A positive spelling approval cannot apply to a whole negative token.
        # Worked examples: 三 -> -3 needs one substitution and one insertion;
        # 三 -> -三 needs only the sign insertion, irrespective of writing style.
        for spelling, expected in (
                ("3", {"substitutions": 1, "deletions": 0, "insertions": 1}),
                ("三", {"substitutions": 0, "deletions": 0, "insertions": 1})):
            for minus in ("-", "−", "－", "﹣"):
                for spacing in ("", " \t"):
                    with self.subTest(spelling=spelling, minus=minus, spacing=spacing):
                        report = self.score([
                            {"id": "signed", "reference": "温度三度", "expressions": [],
                             "number_equivalences": [{"canonical": "三度", "variants": ["3度"]}]},
                        ], [{"id": "signed", "phase": "Result",
                             "text": f"温度{minus}{spacing}{spelling}度"}])
                        self.assertEqual(report["samples"][0]["edits"], expected)
                        self.assertEqual(report["samples"][0]["number_format_differences"], [])

    @unittest.skipUnless(sys.platform == "win32", "Windows loopback recorder entry point")
    def test_twenty_sample_recording_plan_can_be_inspected_without_recording(self):
        output = self.root / "not-created"
        process = subprocess.run([
            "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
            str(REPOSITORY / "scripts" / "record-meeting-corpus.ps1"),
            "-Corpus", str(REPOSITORY / "quality" / "cloud-meeting-corpus.json"),
            "-Output", str(output), "-ListOnly",
        ], capture_output=True, encoding="utf-8")
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertIn("Recorded: 0 / 20", process.stdout)
        self.assertIn("uncertainty", process.stdout)
        self.assertFalse(output.exists())

    def test_freeze_requires_real_audio_and_listened_reference_reviews(self):
        recordings = save_json(self.root / "recordings.json", {
            "kind": "real-meeting-evaluation", "samples": [],
        })
        review = self.root / "review.json"
        process = self.command(
            "review-template", "--definition", REPOSITORY / "quality/cloud-meeting-corpus.json",
            "--recordings", recordings, "--output", review)
        self.assertEqual(process.returncode, 0, process.stderr)
        template = json.loads(review.read_text(encoding="utf-8"))
        self.assertEqual(len(template["samples"]), 20)
        self.assertFalse(template["samples"][0]["human_recording_confirmed"])
        process = self.command(
            "freeze", "--definition", REPOSITORY / "quality/cloud-meeting-corpus.json",
            "--recordings", recordings, "--review", review,
            "--baseline", self.root / "not-yet-built.json", "--output", self.root / "frozen")
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("20 verified real samples", process.stderr)
        self.assertFalse((self.root / "frozen/manifest.json").exists())

    def test_frozen_audio_and_baseline_metadata_are_bound_by_hash(self):
        manifest = self.frozen_fixture()
        self.assertTrue(manifest.exists(), "freeze must produce a replayable manifest")
        corpus = json.loads(manifest.read_text(encoding="utf-8"))
        self.assertEqual(corpus["baseline"]["git_commit"], BASELINE_COMMIT)
        self.assertEqual(corpus["evidence_kind"], "tooling-fixture")
        self.assertEqual(len(corpus["samples"]), 20)
        self.assertEqual(len(corpus["samples"][0]["reference_sha256"]), 64)
        audio = Path(corpus["samples"][0]["audio"])
        audio.write_bytes(audio.read_bytes() + b"changed")
        run = save_json(self.root / "run.json", {"samples": []})
        process = self.command("score", "--manifest", manifest, "--results", run,
                               "--output", self.root / "report.json")
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("Audio changed", process.stderr)

    def test_result_files_from_another_frozen_manifest_are_rejected(self):
        manifest = save_json(self.root / "manifest.json", {
            "schema_version": 2, "evidence_kind": "tooling-fixture",
            "samples": [{"id": "sample", "reference": "原话", "expressions": []}],
        })
        run = save_json(self.root / "run.json", {
            "corpus_manifest_sha256": "0" * 64,
            "samples": [{"id": "sample", "phase": "Result", "text": "原话"}],
        })
        process = self.command("score", "--manifest", manifest, "--results", run,
                               "--output", self.root / "report.json")
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("different frozen manifest", process.stderr)

    def test_key_expression_presence_never_substitutes_for_manual_review(self):
        report = self.score([
            {"id": "facts", "reference": "可能不要改API的3秒超时", "expressions": [
                {"category": "technical", "expected": "API"},
                {"category": "number", "expected": "3秒"},
                {"category": "negation", "expected": "不要改"},
                {"category": "uncertainty", "expected": "可能"},
            ]},
        ], [{"id": "facts", "phase": "Result", "text": "要改API的4秒超时"}])
        self.assertEqual(report["summary"]["key_expression_pending_count"], 4)
        self.assertTrue(report["key_expressions"][0]["presence_hint"])
        self.assertEqual(report["key_expressions"][0]["verdict"], "pending")
        review = self.root / "report-key-review.json"
        content = json.loads(review.read_text(encoding="utf-8"))
        content.update(reviewer="tooling fixture", reviewed_at="2026-11-14T12:00:00+08:00")
        for index, item in enumerate(content["items"]):
            item.update(verdict="pass" if index == 0 else "fail",
                        observed="API" if index == 0 else "(changed or omitted)",
                        note="fixture behavior only")
        save_json(review, content)
        output = self.root / "reviewed.json"
        process = self.command("score", "--manifest", self.root / "manifest.json",
                               "--results", self.root / "run.json", "--reviews", review,
                               "--output", output)
        self.assertEqual(process.returncode, 0, process.stderr)
        reviewed = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(reviewed["summary"]["key_expression_pending_count"], 0)
        self.assertEqual(reviewed["summary"]["key_expression_error_count"], 3)
        self.assertFalse(reviewed["acceptance_prerequisites_met"], "fixtures never qualify")

    @unittest.skipUnless(sys.platform == "win32", "Windows replay harness entry point")
    def test_replay_rejects_changed_baseline_before_playing_or_capturing(self):
        binary = self.root / "not-an-executable.txt"
        binary.write_text("fixture", encoding="utf-8")
        receipt = save_json(self.root / "baseline.json", {
            "kind": "local-baseline-build", "git_commit": BASELINE_COMMIT,
            "artifacts": {"listen_binary": {"path": str(binary), "sha256": "0" * 64}},
            "listen_args": [],
        })
        manifest = save_json(self.root / "manifest.json", {
            "samples": [{"id": "fixture", "reference": "原话"}],
        })
        output = self.root / "not-created/run.json"
        process = subprocess.run([
            "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
            str(REPOSITORY / "scripts/benchmark-meeting-corpus.ps1"),
            "-Corpus", str(manifest), "-Baseline", str(receipt),
            "-Runtime", str(self.root / "not-created/runtime"),
            "-Output", str(output), "-ListOnly",
        ], capture_output=True, encoding="utf-8")
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("Baseline artifact changed", process.stderr)
        self.assertFalse(output.parent.exists())

    def test_percent_units_and_numeric_boundaries_are_not_erased(self):
        report = self.score([
            {"id": "percent", "reference": "5%", "expressions": []},
            {"id": "time", "reference": "3:15", "expressions": []},
            {"id": "boundary", "reference": "12秒", "expressions": [],
             "number_equivalences": [{"canonical": "2秒", "variants": ["两秒"]}]},
        ], [
            {"id": "percent", "phase": "Result", "text": "5"},
            {"id": "time", "phase": "Result", "text": "315"},
            {"id": "boundary", "phase": "Result", "text": "12秒"},
        ])
        self.assertEqual(report["summary"]["edits"], {
            "substitutions": 0, "deletions": 2, "insertions": 0,
        })
        self.assertEqual(report["samples"][-1]["number_format_differences"], [])

    def test_number_format_approval_cannot_erase_numeric_or_negation_errors(self):
        self.frozen_fixture()
        review_path = self.root / "review.json"
        content = json.loads(review_path.read_text(encoding="utf-8"))
        for variant in ("30次", "不三次"):
            with self.subTest(variant=variant):
                content["samples"][4]["number_equivalences"] = [{
                    "canonical": "3 次", "variants": [variant],
                    "verified": True, "reason": "fixture must reject changed meaning",
                }]
                save_json(review_path, content)
                process = self.command(
                    "freeze", "--definition", REPOSITORY / "quality/cloud-meeting-corpus.json",
                    "--recordings", self.root / "recordings.json", "--review", review_path,
                    "--baseline", self.root / "baseline.json", "--output", self.root / variant)
                self.assertNotEqual(process.returncode, 0)
                self.assertIn("changes numeric meaning", process.stderr)

    def test_a_real_corpus_label_cannot_bypass_missing_reference_verification(self):
        manifest = self.frozen_fixture()
        content = json.loads(manifest.read_text(encoding="utf-8"))
        content["evidence_kind"] = "real-human-speech"
        content["samples"][0]["reference_review"]["reference_verified"] = False
        save_json(manifest, content)
        run = save_json(self.root / "run.json", {
            "corpus_manifest_sha256": sha(manifest), "samples": [],
        })
        process = self.command("score", "--manifest", manifest, "--results", run,
                               "--output", self.root / "report.json")
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("Reference verification is incomplete", process.stderr)

    def test_colloquial_ranges_and_abbreviated_chinese_numbers_are_not_equivalent(self):
        for index, (canonical, variant) in enumerate((
                ("23次", "两三次"), ("12次", "一两次"), ("302次", "三百二次"))):
            with self.subTest(variant=variant):
                manifest = save_json(self.root / f"manifest-{index}.json", {
                    "schema_version": 2, "evidence_kind": "tooling-fixture",
                    "samples": [{"id": "ambiguous", "reference": canonical, "expressions": [],
                                 "number_equivalences": [{"canonical": canonical, "variants": [variant],
                                                          "verified": True, "reason": "must reject ambiguity"}]}],
                })
                run = save_json(self.root / f"run-{index}.json", {
                    "corpus_manifest_sha256": sha(manifest),
                    "samples": [{"id": "ambiguous", "phase": "Result", "text": variant}],
                })
                process = self.command("score", "--manifest", manifest, "--results", run,
                                       "--output", self.root / f"report-{index}.json")
                self.assertNotEqual(process.returncode, 0)
                self.assertIn("changes numeric meaning", process.stderr)


if __name__ == "__main__":
    unittest.main()
