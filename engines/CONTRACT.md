# Local TTS engine script contract

Every local engine is a standalone Python script `engines/<name>_infer.py` run inside its own
uv-managed venv at `.venvs/<name>`. The Rust CLI shells out to it with a **uniform interface**
so all engines are interchangeable.

## Invocation

```
<abs venv python> engines/<name>_infer.py --text-file <in.txt> --out <out.wav> [--voice <v>] [--ref-audio <ref.wav>]
```

- `--text-file`  : path to a UTF-8 text file to synthesize (required)
- `--out`        : path to write a **WAV** file (PCM), required
- `--voice`      : engine voice/preset name (optional; each engine picks a sane default)
- `--ref-audio`  : reference wav for voice cloning (optional; ignored by engines that don't clone)

## Output rules (strict)

- Write the synthesized audio as a WAV to `--out`.
- On success, print **exactly one line of JSON to STDOUT and nothing else on stdout**:
  ```json
  {"audio_seconds": 12.34, "sample_rate": 24000, "engine": "kokoro", "voice": "af_heart"}
  ```
- **All logs, progress bars, warnings, model chatter → STDERR only.** stdout must stay clean
  (the Rust CLI parses that one JSON line).
- Exit code 0 on success; non-zero with a clear stderr message on failure.
- Must run **fully offline** after the first weight download (respect `HF_HUB_OFFLINE=1` if set;
  allow the first download when weights are absent).

## Conventions

- venv: `.venvs/<name>` (uv, pinned Python). Weights: `models/<name>/` or the HF cache.
- Keep imports lazy where possible so `--help` is fast.
- Deterministic-ish: default temperature/seed fixed where the engine allows, so A/B tests are fair.
