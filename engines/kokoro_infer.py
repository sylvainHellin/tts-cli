#!/usr/bin/env python3
"""Kokoro (kokoro-onnx) local TTS engine.

Conforms to engines/CONTRACT.md:
  <venv python> engines/kokoro_infer.py --text-file <in.txt> --out <out.wav> [--voice <v>] [--ref-audio <ref.wav>]

Prints exactly one JSON line to STDOUT on success; all logs go to STDERR.
Runs CPU-only via onnxruntime. Fully offline after weights are present.
"""

import argparse
import json
import os
import sys
from pathlib import Path

# Repo root is the parent of the engines/ directory that contains this file.
REPO_ROOT = Path(__file__).resolve().parent.parent

# Default on-disk weights location; overridable via KOKORO_MODEL_DIR.
DEFAULT_MODEL_DIR = REPO_ROOT / "models" / "kokoro"
MODEL_FILENAME = "kokoro-v1.0.onnx"
VOICES_FILENAME = "voices-v1.0.bin"

DEFAULT_VOICE = "af_heart"
SAMPLE_RATE = 24000


def log(*args):
    print(*args, file=sys.stderr, flush=True)


def resolve_model_dir() -> Path:
    override = os.environ.get("KOKORO_MODEL_DIR")
    if override:
        return Path(override).expanduser().resolve()
    return DEFAULT_MODEL_DIR


def main() -> int:
    parser = argparse.ArgumentParser(description="Kokoro (kokoro-onnx) TTS engine")
    parser.add_argument("--text-file", required=True, help="UTF-8 text file to synthesize")
    parser.add_argument("--out", required=True, help="output WAV path (PCM)")
    parser.add_argument("--voice", default=DEFAULT_VOICE, help=f"voice name (default {DEFAULT_VOICE})")
    parser.add_argument("--ref-audio", default=None, help="ignored (Kokoro does not voice-clone)")
    args = parser.parse_args()

    text_path = Path(args.text_file)
    if not text_path.is_file():
        log(f"[kokoro] error: text file not found: {text_path}")
        return 1
    text = text_path.read_text(encoding="utf-8").strip()
    if not text:
        log(f"[kokoro] error: text file is empty: {text_path}")
        return 1

    if args.ref_audio:
        log("[kokoro] note: --ref-audio is ignored (Kokoro does not support voice cloning)")

    model_dir = resolve_model_dir()
    model_path = model_dir / MODEL_FILENAME
    voices_path = model_dir / VOICES_FILENAME
    for p in (model_path, voices_path):
        if not p.is_file():
            log(f"[kokoro] error: missing weight file: {p}")
            log(f"[kokoro] set KOKORO_MODEL_DIR or download weights into {model_dir}")
            return 1

    # Lazy imports so --help stays fast.
    log(f"[kokoro] loading model from {model_dir}")
    import soundfile as sf
    from kokoro_onnx import Kokoro

    kokoro = Kokoro(str(model_path), str(voices_path))

    voice = args.voice or DEFAULT_VOICE
    available = set(kokoro.get_voices())
    if voice not in available:
        log(f"[kokoro] error: unknown voice '{voice}'. Available: {sorted(available)}")
        return 1

    log(f"[kokoro] synthesizing {len(text)} chars with voice '{voice}'")
    # speed/lang fixed for fair, deterministic A/B tests.
    samples, sr = kokoro.create(text, voice=voice, speed=1.0, lang="en-us")
    if sr != SAMPLE_RATE:
        log(f"[kokoro] warning: model returned sample_rate={sr}, expected {SAMPLE_RATE}")

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(out_path), samples, sr)

    audio_seconds = float(len(samples)) / float(sr) if sr else 0.0
    log(f"[kokoro] wrote {out_path} ({audio_seconds:.2f}s @ {sr} Hz)")

    print(json.dumps({
        "audio_seconds": round(audio_seconds, 2),
        "sample_rate": sr,
        "engine": "kokoro",
        "voice": voice,
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
