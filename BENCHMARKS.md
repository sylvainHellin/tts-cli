# TTS backend benchmarks

A size-vs-latency-vs-quality comparison of the four `tts` backends, measured on the
home server. The goal is to answer: *how intensive is each model on this box, and what
do you get for that cost?*

## Hardware / context

All local runs were measured on the home server:

- **CPU:** AMD Ryzen 7 PRO 8845HS — 8 cores / 16 threads
- **RAM:** 28 GB
- **GPU:** Radeon 780M **integrated** graphics only. **No CUDA / no discrete NVIDIA GPU,
  and ROCm is not installed.**

The single most important finding: **everything here runs CPU-only.** The iGPU was measured
at **0% busy on every local engine** (see `igpu_busy_peak`/`igpu_busy_mean` in the JSON) —
none of these PyTorch/ONNX stacks touch the Radeon 780M without a ROCm build, so "GPU load"
is simply not a lever on this machine. The cost of local TTS here is CPU time and RAM, full
stop. Plan accordingly: a model's real-time factor on this box is set entirely by how much
CPU math it does per second of audio.

MiniMax is included as the cloud reference point. Its synthesis runs server-side, so the
local process is just an HTTP client — its "wall time" is a network round-trip, not compute.

## Measured results

Input for every engine: the abstract of Wang & Zhang (2020), *Automation in Construction* —
**114 words / 789 characters**. Same text, same box, one warm run each.

| Engine | wall (s) | audio (s) | RTF (wall) | CPU total (s) | peak RSS (MB) | peak threads | iGPU | weights on disk | license |
|---|---|---|---|---|---|---|---|---|---|
| **MiniMax** speech-2.8-hd (cloud) | 3.97 | 48.49 | 0.08 † | n/a (server-side) | ~7 (HTTP client) | n/a | n/a | — (cloud) | proprietary / paid |
| **Kokoro-82M** (local) | 11.79 | 48.98 | **0.24** | 83.5 | 1143 | 23 | **0%** | 337 MB | Apache-2.0 |
| **StyleTTS2** (local) | 54.79 | 53.07 | **1.03** | 277 | 2905 | 38 | **0%** | 870 MB | MIT |
| **Chatterbox 0.5B** (local) | 258.7 | 37.56 ‡ | **6.89** | 1878 | 6696 | 39 | **0%** | ~3 GB (HF cache) | MIT |

> **Note on subjective quality (owner listening test):** the local voices are all a clear
> step below MiniMax in naturalness — expected for local CPU models. Among the three local
> engines, **Kokoro sounded the best overall despite being by far the smallest** (82M / 337 MB):
> the extra size and compute of StyleTTS2 and Chatterbox did not buy better everyday narration
> quality here, only expressiveness/cloning options.

**Derived metrics** (the "how intensive" numbers you actually plan around):

| Engine | compute-s per **minute of audio** | CPU-s per **1000 words** |
|---|---|---|
| Kokoro-82M | **14.4** | **732** |
| StyleTTS2 | **61.9** | **2431** |
| Chatterbox 0.5B | **413** | **16474** |

† MiniMax RTF reflects network round-trip + mp3 write, **not** synthesis compute. Not
comparable to the local RTFs; shown only to place the cloud path on the same axis.

‡ Chatterbox produced **37.6 s** of audio where the others render ~49 s. Confirmed by
listening to the actual sample: the reading is **verbatim** — the shorter duration is just a
faster, tighter speaking cadence, not dropped or paraphrased text. Its per-minute and
per-1000-word figures are therefore accurate for verbatim output.

### What the metrics mean and how they were measured

- **RTF (real-time factor) = wall_seconds / audio_seconds.** Below 1.0 means the model
  generates faster than real time (10 s of speech in under 10 s of wall clock); above 1.0
  means slower than real time. Kokoro is ~4× faster than real time, StyleTTS2 is roughly
  real time, Chatterbox is ~7× slower than real time.
- **compute-seconds per minute of audio = cpu_total_s / (audio_seconds / 60).** This uses
  total CPU time across all threads, so it captures how many core-seconds of the 16 available
  threads a minute of narration burns — a truer "server load" figure than wall RTF, which
  hides multi-threading.
- **CPU-seconds per 1000 words = cpu_total_s / (words / 1000).** A text-length-normalized cost
  so you can estimate a whole book: e.g. a 90,000-word book is ~90× these numbers of CPU-seconds.

Measurement was done by the CLI's `--benchmark` mode (wrapper: `engines/bench_run.py`),
which works like `/usr/bin/time -v` but tuned for this comparison:

- **CPU time**: `resource.getrusage(RUSAGE_CHILDREN)` (user + sys) for the engine subprocess.
- **Peak RSS and peak thread count**: `psutil` sampling of the whole process tree during the run.
- **iGPU busy %**: read from the AMD sysfs counter under `/sys/class/drm/.../device/gpu_busy_percent`,
  sampled alongside, to confirm the integrated GPU stays idle (it does — 0% everywhere).
- **audio_seconds**: reported by each engine on its stdout JSON line (the render's true duration).

The raw per-run JSON lives in `samples/bench/*.json`.

## Tradeoffs

Read this as **cost climbs steeply for diminishing, situational gains**:

- **Size on disk** scales 337 MB → 870 MB → ~3 GB across Kokoro → StyleTTS2 → Chatterbox.
- **Latency** scales ~4× *faster* than real time → ~real time → ~7× *slower* than real time.
  In CPU-seconds per minute of audio that's **14 → 62 → 413** — a ~29× spread end to end.
- **RAM** scales 1.1 GB → 2.9 GB → 6.7 GB peak. Chatterbox's ~6.7 GB peak is a real
  consideration on a 28 GB box that also runs other services.
- **Quality/behavior** is *not* strictly monotonic with cost:
  - **Kokoro-82M** — fast, natural, clean prosody. **54 fixed voices, no cloning.** Best CPU
    value by a wide margin. This is the sane default for narration and audiobooks on this box.
  - **StyleTTS2** — roughly real time, a higher expressiveness ceiling, and **voice cloning
    via `--ref-audio`**. Uses the `gruut` phonemizer (no system `espeak-ng` needed) and is
    effectively deterministic, so A/B tests are fair. Caveat: it fetches a small config from
    GitHub on first load, so the very first run needs network (a transient HTTP 429 is
    possible — just retry).
  - **Chatterbox 0.5B** — the most expressive and also supports voice cloning, and it reads
    **verbatim** (confirmed by listening: its shorter 37.6 s render vs the ~49 s others is a
    faster speaking cadence, not dropped text). Its real cost is speed and memory: ~7× slower
    than real time and a ~6.7 GB RAM peak. That makes it an offline/batch tool rather than an
    interactive one, but with verbatim accuracy confirmed it *is* usable for audiobook
    narration if you can tolerate the slow offline render — the tradeoff is throughput/RAM,
    not accuracy.
  - **MiniMax speech-2.8-hd** — top-tier quality with effectively zero local cost, but it is
    the **cloud/paid** path (the very thing the local backends exist to replace for long books).

### Recommendation

- **Default to Kokoro** for local narration and audiobooks: fastest, lightest, faithful,
  Apache-2.0, and easily the best cost/quality point on CPU.
- **Reach for StyleTTS2** when you want a cloned or more expressive voice and can tolerate
  ~real-time speed — a whole book is then roughly "1 minute of compute per minute of audio."
- **Use Chatterbox** when you want maximum expressiveness or voice cloning and can run it
  offline/batch — it reads verbatim, so it is fine for narration too; just budget the ~6.7 GB
  RAM peak and the ~7× slowdown. The only reasons to prefer another engine here are speed and
  memory, not accuracy.
- **Use MiniMax** when you want the highest quality and don't mind the cloud/paid path — it
  remains the CLI default and the reference for what "good" sounds like.

### `fish audio s2-pro` — evaluated and dropped

Not shipped. It is a gated, **non-commercial**, 4.4B-parameter model whose only released
inference engine is **SGLang (CUDA/GPU-only)** — there is no CPU path, so it cannot run on
this server at all. It also would not run practically on the M4 Pro Mac; of that family, only
the much smaller **OpenAudio S1-mini** would run there (via Metal/MPS). It was cut from the
local lineup for these reasons.

## Listen: A/B on Audiobookshelf

A chaptered `engine-comparison.m4b` is in `~/media/audiobooks/` (served by Audiobookshelf).
It has **4 chapters — MiniMax, Kokoro, StyleTTS2, Chatterbox — each reading the same abstract**,
so you can subjectively A/B the timbre and expressiveness on the phone alongside these numbers.

## Reproduce

Benchmark any backend on your own text:

```bash
tts --backend <minimax|kokoro|chatterbox|styletts2> --benchmark -o out.mp3 --file <text>
```

`--benchmark` writes a JSON metrics record (to `samples/bench/` by default) with wall time,
CPU time, peak RSS/threads, iGPU busy %, RTF, and the two derived per-audio / per-1000-word
figures used above.
