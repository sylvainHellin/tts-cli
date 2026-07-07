#!/usr/bin/env python3
"""StyleTTS2 local TTS engine for tts-cli.

Contract (engines/CONTRACT.md):
  <venv python> engines/styletts2_infer.py --text-file <in.txt> --out <out.wav> \
      [--voice <v>] [--ref-audio <ref.wav>]

- Synthesizes the UTF-8 text in --text-file to a mono 24 kHz PCM WAV at --out.
- On success prints EXACTLY one JSON line to STDOUT and nothing else:
    {"audio_seconds": .., "sample_rate": 24000, "engine": "styletts2", "voice": ".."}
- ALL model chatter / progress / warnings go to STDERR.
- Deterministic: fixed seed + fixed diffusion_steps for fair A/B tests.

Weights: the maintained `styletts2` PyPI package downloads the LibriTTS
pretrained checkpoint (yl4579/StyleTTS2-LibriTTS, MIT) plus the ASR/F0/PLBERT
aux checkpoints via the `cached_path` library. We pin that cache under the repo
at models/styletts2/cached_path so weights live with the repo and run offline
after the first download. Override the location with STYLETTS2_MODELS_DIR (or
the generic TTS_MODELS_DIR); it becomes <dir>/cached_path.

Phonemization: uses gruut (pure-Python, bundled by the package). No espeak-ng
system dependency is required.
"""

import argparse
import contextlib
import functools
import io
import json
import os
import sys
import wave
from pathlib import Path

# ---- logging: everything human-facing goes to stderr -----------------------


def log(*args):
    print(*args, file=sys.stderr, flush=True)


REPO_ROOT = Path(__file__).resolve().parent.parent
ENGINE_NAME = "styletts2"
SAMPLE_RATE = 24000
DIFFUSION_STEPS = 8  # fixed, modest: good quality/speed balance on CPU
SEED = 0


def resolve_models_dir() -> Path:
    """Resolve the on-disk weights cache root, honoring an env override."""
    for env in ("STYLETTS2_MODELS_DIR", "TTS_MODELS_DIR"):
        val = os.environ.get(env)
        if val:
            base = Path(val).expanduser()
            # If a generic models dir is given, namespace this engine under it.
            if env == "TTS_MODELS_DIR":
                base = base / ENGINE_NAME
            return base.resolve()
    return (REPO_ROOT / "models" / ENGINE_NAME).resolve()


def main() -> int:
    parser = argparse.ArgumentParser(description="StyleTTS2 TTS engine")
    parser.add_argument("--text-file", required=True, help="UTF-8 text file to synthesize")
    parser.add_argument("--out", required=True, help="output WAV path")
    parser.add_argument("--voice", default="libritts", help="voice/preset label (informational)")
    parser.add_argument("--ref-audio", default=None, help="reference wav for voice cloning")
    parser.add_argument("--diffusion-steps", type=int, default=DIFFUSION_STEPS,
                        help="diffusion sampler steps (default %(default)s)")
    args = parser.parse_args()

    text_path = Path(args.text_file)
    if not text_path.is_file():
        log(f"error: text file not found: {text_path}")
        return 2
    text = text_path.read_text(encoding="utf-8").strip()
    if not text:
        log("error: text file is empty")
        return 2

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    # Pin the cached_path weight cache under the repo (or the env override) so
    # weights live with the repo and run offline after the first download.
    models_dir = resolve_models_dir()
    cache_root = models_dir / "cached_path"
    cache_root.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("CACHED_PATH_CACHE_ROOT", str(cache_root))
    log(f"[{ENGINE_NAME}] weights cache: {cache_root}")

    # Deterministic seeds before heavy imports so the diffusion sampler is fixed.
    import random
    random.seed(SEED)

    log(f"[{ENGINE_NAME}] loading torch / styletts2 ...")
    import numpy as np
    import torch

    np.random.seed(SEED)
    torch.manual_seed(SEED)
    torch.backends.cudnn.deterministic = True
    torch.backends.cudnn.benchmark = False

    # StyleTTS2 checkpoints (yl4579, MIT) are full pickles. torch>=2.6 defaults
    # torch.load(weights_only=True), which rejects them. The package does not
    # pass weights_only=False, so default it here. Source is trusted (pinned HF
    # repo + the package's own GitHub raw URLs).
    _orig_torch_load = torch.load

    @functools.wraps(_orig_torch_load)
    def _torch_load(*a, **k):
        k.setdefault("weights_only", False)
        return _orig_torch_load(*a, **k)

    torch.load = _torch_load

    # word_tokenize needs the punkt_tab resource on nltk>=3.9; the package only
    # fetches legacy 'punkt'. Ensure both are present (no-op if cached).
    import nltk
    for res, path in (("punkt", "tokenizers/punkt"),
                      ("punkt_tab", "tokenizers/punkt_tab")):
        try:
            nltk.data.find(path)
        except LookupError:
            log(f"[{ENGINE_NAME}] downloading nltk resource: {res}")
            with contextlib.redirect_stdout(sys.stderr):
                nltk.download(res, quiet=True)

    # The package prints model chatter to stdout at import + load time; keep
    # stdout clean by redirecting those prints to stderr.
    with contextlib.redirect_stdout(sys.stderr):
        from styletts2 import tts as st2

        log(f"[{ENGINE_NAME}] building model (device=cpu) ...")
        model = st2.StyleTTS2(phoneme_converter="gruut")

        ref_audio = args.ref_audio
        if ref_audio and not Path(ref_audio).is_file():
            log(f"[{ENGINE_NAME}] warning: --ref-audio not found, using default voice: {ref_audio}")
            ref_audio = None

        log(f"[{ENGINE_NAME}] synthesizing ({len(text)} chars, diffusion_steps={args.diffusion_steps}) ...")
        # Re-seed right before sampling for reproducibility across calls.
        torch.manual_seed(SEED)
        np.random.seed(SEED)
        random.seed(SEED)
        audio = model.inference(
            text,
            target_voice_path=ref_audio,
            output_wav_file=None,  # we write the WAV ourselves as int16 PCM
            output_sample_rate=SAMPLE_RATE,
            diffusion_steps=args.diffusion_steps,
        )

    audio = np.asarray(audio, dtype=np.float32).reshape(-1)
    if audio.size == 0:
        log("error: model produced empty audio")
        return 1

    # Normalize to avoid clipping, then write standard 16-bit PCM mono WAV.
    peak = float(np.max(np.abs(audio))) if audio.size else 0.0
    if peak > 1.0:
        audio = audio / peak
    pcm16 = np.clip(audio * 32767.0, -32768, 32767).astype("<i2")

    with wave.open(str(out_path), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(pcm16.tobytes())

    audio_seconds = round(pcm16.shape[0] / SAMPLE_RATE, 3)
    log(f"[{ENGINE_NAME}] wrote {out_path} ({audio_seconds}s @ {SAMPLE_RATE} Hz)")

    result = {
        "audio_seconds": audio_seconds,
        "sample_rate": SAMPLE_RATE,
        "engine": ENGINE_NAME,
        "voice": args.voice if not ref_audio else "cloned",
    }
    # The one and only line on stdout.
    sys.stdout.write(json.dumps(result) + "\n")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001 - surface a clean error on stderr
        import traceback
        traceback.print_exc(file=sys.stderr)
        log(f"error: {exc}")
        sys.exit(1)
