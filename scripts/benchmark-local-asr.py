"""Measure a local whisper.cpp CLI against an explicit test WAV; no uploads."""

import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import time
import wave


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    parser = argparse.ArgumentParser()
    parser.add_argument('--engine', type=pathlib.Path, required=True)
    parser.add_argument('--model', type=pathlib.Path, required=True)
    parser.add_argument('--audio', type=pathlib.Path, required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--backend', choices=['cpu', 'cuda'], required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    stem = args.output / args.backend
    text_path = stem.with_suffix('.txt')
    text_path.unlink(missing_ok=True)
    command = [str(args.engine.resolve()), '-m', str(args.model.resolve()),
               '-f', str(args.audio.resolve()), '-l', 'zh', '-t', '6',
               '-nt', '-otxt', '-of', str(stem.resolve())]
    if args.backend == 'cpu':
        command.append('-ng')
    with wave.open(str(args.audio)) as audio:
        duration = audio.getnframes() / audio.getframerate()
    if duration <= 0:
        raise ValueError('test audio must contain samples')
    start = time.perf_counter()
    run = subprocess.run(command, capture_output=True, timeout=300)
    elapsed = time.perf_counter() - start
    log = run.stderr.decode('utf-8', errors='replace')
    (stem.with_suffix('.log')).write_text(log, encoding='utf-8')
    if run.returncode != 0:
        raise RuntimeError(f'local inference failed: {run.returncode:#x}\n{log[-2000:]}')
    if args.backend == 'cuda' and ('ggml_cuda_init: found' not in log or
                                   'whisper_backend_init_gpu: using CUDA0 backend' not in log):
        raise RuntimeError('CUDA requested, but the model was not confirmed to use CUDA0')
    if not text_path.exists():
        raise RuntimeError('inference did not produce a result file')
    text = text_path.read_text(encoding='utf-8').strip()
    with args.model.open('rb') as model:
        model_hash = hashlib.file_digest(model, 'sha256').hexdigest()
    result = {'backend': args.backend, 'model_sha256': model_hash,
              'audio_seconds': round(duration, 3),
              'wall_seconds_including_model_load': round(elapsed, 3),
              'wall_seconds_per_audio_second': round(elapsed / duration, 4),
              'transcript': text}
    stem.with_suffix('.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
