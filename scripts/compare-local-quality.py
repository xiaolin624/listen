"""Repeatable raw whisper.cpp comparison; no playback, uploads or correctness guesses."""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import time
import wave


# Keep the selected profile in sync with Whisper::transcribe. The baseline is #2.
PROFILES = {
    "baseline": [],
    "no-fallback": ["-bs", "5", "-bo", "5", "-tp", "0", "-nf", "-sns"],
    "strict-no-speech": ["-bs", "5", "-bo", "5", "-tp", "0", "-nf", "-sns",
                         "-nth", "0.5", "-lpt", "0"],
    "simplified-prompt": ["-bs", "5", "-bo", "5", "-tp", "0", "-nf", "-sns",
                          "--prompt", "以下是普通话的简体中文转写。"],
    "vad-baseline": ["--vad", "-vt", "0.5", "-vspd", "250", "-vsd", "100", "-vp", "100", "-vo", "0.1"],
    "vad-no-fallback": ["--vad", "-vt", "0.5", "-vspd", "250", "-vsd", "100", "-vp", "100", "-vo", "0.1",
                        "-bs", "5", "-bo", "5", "-tp", "0", "-nf", "-sns"],
    "auto": ["-l", "auto"],
    "greedy": ["-bs", "1", "-bo", "1", "-tp", "0", "-nf"],
    "technical-vocabulary": ["--prompt", "Rust, WASAPI loopback, CPU, CUDA, API, Windows, Whisper."],
    "no-context": ["-mc", "0"],
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=pathlib.Path, required=True)
    parser.add_argument("--model", type=pathlib.Path, required=True)
    parser.add_argument("--vad-model", type=pathlib.Path)
    parser.add_argument("--gate", action="store_true",
                        help="gate the original, uncut WAV with the sibling Silero VAD CLI")
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--profiles", nargs="+", choices=PROFILES,
                        default=["baseline", "no-fallback", "strict-no-speech", "simplified-prompt"])
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(args.manifest.read_text(encoding="utf-8-sig"))
    summary = {"engine": str(args.engine.resolve()), "engine_sha256": digest(args.engine),
               "model": str(args.model.resolve()), "model_sha256": digest(args.model),
               "vad_model_sha256": digest(args.vad_model) if args.vad_model else None,
               "gate_original_audio": args.gate,
               "manifest": manifest, "runs": []}
    for name in args.profiles:
        for sample in manifest["samples"]:
            audio = pathlib.Path(sample["audio"])
            if digest(audio) != sample["sha256"]:
                raise ValueError(f"Audio changed since corpus generation: {audio}")
            stem = args.output.resolve() / f"{name}-{sample['id']}"
            stem.with_suffix(".txt").unlink(missing_ok=True)
            command = [str(args.engine.resolve()), "-m", str(args.model.resolve()),
                       "-f", str(audio.resolve()), "-l", "zh", "-t", "6", "-nt",
                       "-otxt", "-of", str(stem), "-ng", *PROFILES[name]]
            if "--vad" in PROFILES[name]:
                if args.vad_model is None:
                    raise ValueError("--vad-model is required for a VAD profile")
                command.extend(["-vm", str(args.vad_model.resolve())])
            start = time.perf_counter()
            has_speech = True
            gate_command = None
            if args.gate:
                if args.vad_model is None:
                    raise ValueError("--vad-model is required for --gate")
                gate_command = [str(args.engine.with_name("vad-speech-segments.exe").resolve()),
                                "-vm", str(args.vad_model.resolve()), "-f", str(audio.resolve()),
                                "-t", "6", "-vt", "0.5", "-vp", "30", "-vo", "0.1", "-np"]
                gate = subprocess.run(gate_command, capture_output=True, timeout=300)
                stem.with_suffix(".vad.log").write_bytes(gate.stderr)
                stem.with_suffix(".vad.txt").write_bytes(gate.stdout)
                match = re.fullmatch(rb"\s*Detected (\d+) speech segments:\s*((?:Speech segment[^\r\n]*\s*)*)", gate.stdout)
                if gate.returncode or not match:
                    raise RuntimeError(f"Local VAD failed: {gate.returncode}; {stem}.vad.log")
                has_speech = int(match.group(1)) > 0
            if has_speech:
                result = subprocess.run(command, capture_output=True, timeout=300)
                stem.with_suffix(".log").write_bytes(result.stderr)
                if result.returncode or not stem.with_suffix(".txt").exists():
                    raise RuntimeError(f"Local backend failed: {result.returncode}; {stem}.log")
            else:
                stem.with_suffix(".txt").write_text("", encoding="utf-8")
            elapsed = time.perf_counter() - start
            error = None
            try:
                text = stem.with_suffix(".txt").read_text(encoding="utf-8").strip()
            except UnicodeDecodeError as exc:
                text = None
                error = str(exc)
            with wave.open(str(audio)) as stream:
                seconds = stream.getnframes() / stream.getframerate()
            run = {"profile": name, "sample": sample["id"], "audio_seconds": seconds,
                   "wall_seconds_including_model_load": round(elapsed, 3),
                   "command": command, "reference": sample["reference"], "transcript": text,
                   "gate_command": gate_command, "speech_detected": has_speech,
                   "decode_error": error,
                   "non_speech_empty": text == "" if sample["kind"] != "speech" else None}
            summary["runs"].append(run)
            print(json.dumps(run, ensure_ascii=False), flush=True)
            (args.output / "comparison.json").write_text(
                json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
