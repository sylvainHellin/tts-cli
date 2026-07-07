#!/usr/bin/env python3
"""Chatterbox local TTS engine (CPU).

Uniform engine interface per engines/CONTRACT.md:

    <venv python> engines/chatterbox_infer.py --text-file <in.txt> --out <out.wav> \
        [--voice <v>] [--ref-audio <ref.wav>]

Uses the BASE ENGLISH ChatterboxTTS model (multilingual model has known broken
CPU support). Emits exactly one JSON line on stdout; all model/torch chatter is
forced to stderr. Sample rate is model.sr (24000).
"""

import argparse
import contextlib
import json
import os
import sys
import wave


# Fixed seed for reproducible-ish A/B comparisons.
SEED = 1234
# Fixed sampling temperature (generate() default is 0.8; we pin it explicitly).
TEMPERATURE = 0.8


def eprint(*args, **kwargs):
    kwargs.setdefault("file", sys.stderr)
    print(*args, **kwargs)


@contextlib.contextmanager
def stdout_to_stderr():
    """Redirect anything a library writes to stdout onto stderr.

    Keeps the real stdout clean for the single JSON result line. Works at the
    file-descriptor level so C/torch prints are captured too.
    """
    saved_fd = os.dup(1)
    try:
        os.dup2(2, 1)
        yield
    finally:
        sys.stdout.flush()
        os.dup2(saved_fd, 1)
        os.close(saved_fd)


def main():
    parser = argparse.ArgumentParser(description="Chatterbox TTS engine (CPU)")
    parser.add_argument("--text-file", required=True, help="UTF-8 text file to synthesize")
    parser.add_argument("--out", required=True, help="output WAV path")
    parser.add_argument("--voice", default="builtin", help="voice/preset (informational)")
    parser.add_argument("--ref-audio", default=None, help="reference wav for voice cloning")
    args = parser.parse_args()

    with open(args.text_file, "r", encoding="utf-8") as fh:
        text = fh.read().strip()
    if not text:
        eprint("error: --text-file is empty")
        sys.exit(1)

    # Heavy imports lazy so --help stays fast.
    import torch
    import numpy as np

    torch.manual_seed(SEED)
    np.random.seed(SEED)

    voice_label = "builtin"

    with stdout_to_stderr():
        from chatterbox.tts import ChatterboxTTS

        model = ChatterboxTTS.from_pretrained(device="cpu")

        gen_kwargs = {"temperature": TEMPERATURE}
        if args.ref_audio:
            gen_kwargs["audio_prompt_path"] = args.ref_audio
            voice_label = "cloned"

        wav = model.generate(text, **gen_kwargs)  # torch.Tensor, shape (1, N) or (N,)
        sr = int(model.sr)

        # Normalize to a 1-D float32 numpy array.
        if hasattr(wav, "detach"):
            wav = wav.detach().cpu().to(torch.float32).numpy()
        wav = np.asarray(wav, dtype=np.float32).reshape(-1)

    # Convert float [-1, 1] to 16-bit PCM WAV.
    clipped = np.clip(wav, -1.0, 1.0)
    pcm16 = (clipped * 32767.0).astype("<i2")

    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    with wave.open(args.out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm16.tobytes())

    audio_seconds = round(len(pcm16) / float(sr), 2)

    print(json.dumps({
        "audio_seconds": audio_seconds,
        "sample_rate": sr,
        "engine": "chatterbox",
        "voice": voice_label,
    }))


if __name__ == "__main__":
    main()
