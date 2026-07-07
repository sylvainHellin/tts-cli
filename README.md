# tts-cli

A thin, transparent CLI wrapper around the MiniMax `t2a_v2` text-to-speech API,
with optional **local CPU backends** (Kokoro, StyleTTS2, Chatterbox) for offline,
token-free synthesis. It renders text to an audio file and prints the output path.
Nothing more.

MiniMax remains the default. The local backends exist so long-form audio (books,
papers) can be rendered on the home server without spending cloud tokens; see
[BENCHMARKS.md](BENCHMARKS.md) for a full size-vs-latency-vs-quality comparison
measured on that CPU-only box.

Written in Rust. The HTTP client is rustls-based (via `ureq`), so the binary
builds and runs on a headless Linux box with no OpenSSL system library. The
command is `tts`.

## What it is, what it is not

It RENDERS audio to a file. It does not deliver. Delivery is the caller's job
(see "Delivery policy" below). Keeping render and deliver separate is what makes
this safe to call from cron, an agent, or a shell pipe.

## Install

Build and install natively from the repo root:

```bash
cargo build --release        # build in place: target/release/tts
cargo install --path .       # install the `tts` command on PATH
cargo run -- --help          # run without installing
```

Build on each platform you run on. A Mac build is a Mach-O arm64 binary and will
not run on the x86_64 Linux box; build there too (`cargo install --path .` on the
box) rather than copying the Mac binary across.

## Usage

```bash
# positional text
tts -o /tmp/note.mp3 "Hello from MiniMax."

# from a file (e.g. narrate a paper)
tts --file paper.txt -o /tmp/paper.mp3

# from stdin
echo "piped text" | tts -o /tmp/piped.mp3 -

# inspect the request without spending an API call
tts --dry-run -o /tmp/x.mp3 "hello world"

# render locally on CPU with no cloud call (Kokoro is the fast local default)
tts --backend kokoro -o /tmp/local.mp3 "Hello from a local model."

# local voice cloning from a reference clip (chatterbox or styletts2)
tts --backend styletts2 --ref-audio ref.wav -o /tmp/cloned.mp3 "Cloned voice."

# measure how intensive a render is (CPU/wall time, peak RAM, threads, iGPU)
tts --backend kokoro --benchmark -o /tmp/x.mp3 --file paper.txt

# headless / cron: read MINIMAX_API_KEY from a dotenv file, no shell needed
tts --env-file /home/sylvain/.hermes/.env --file script.txt -o /tmp/out.mp3
```

On success the absolute output path is printed to stdout and the exit code is 0,
so a cron job or agent can capture the path directly. Failures use distinct
nonzero exit codes: 2 usage, 3 API error, 4 network error, 5 IO/write error.

### Options and defaults

| Flag | Default |
| --- | --- |
| `--backend` | `minimax` (also: `kokoro`, `styletts2`, `chatterbox`) |
| `--ref-audio` | unset (reference WAV for cloning; local cloning engines only) |
| `--benchmark` | off (write a JSON resource-usage record around the run) |
| `--model` | `speech-2.8-hd` |
| `--voice` | `English_Upbeat_Woman` |
| `--speed` | `1.0` |
| `--vol` | `1.0` |
| `--pitch` | `0` |
| `--emotion` | `neutral` |
| `--sample-rate` | `32000` |
| `--bitrate` | `128000` |
| `--format` | `mp3` |
| `--channel` | `1` |
| `--group-id` | unset (env `MINIMAX_GROUP_ID`); appended as `?GroupId=` only when set |

`--no-chunk` errors instead of chunking when text is over the limit.
`--dry-run` (alias `--print-payload`) prints the request body with the text
elided to a char count and no key present, and sends nothing. `--dry-run` applies
to the `minimax` backend only (local engines have no request body to print).

## Backends

`--backend` selects the synthesis engine. `minimax` is the cloud default and is
unchanged; the rest run **locally on CPU** (no cloud call, no tokens spent):

| Backend | Where | Speed on this box | Cloning | When to use |
| --- | --- | --- | --- | --- |
| `minimax` | cloud (paid) | instant (network) | no | top quality, don't mind the paid/cloud path |
| `kokoro` | local CPU | ~4× faster than real time | no | **fast local default** for books/papers |
| `styletts2` | local CPU | ~real time | yes (`--ref-audio`) | cloned/expressive voice near real time |
| `chatterbox` | local CPU | ~7× slower than real time | yes (`--ref-audio`) | max expressiveness / cloning, offline batch (verbatim, but slow + RAM-hungry) |

Chatterbox reads **verbatim**; its shorter renders reflect a faster speaking cadence,
not dropped text. Its real cost is speed and memory (~7× slower than real time, ~6.7 GB
RAM peak), so use it offline/batch. On a subjective listen the local voices sit a clear
step below MiniMax, and among them **Kokoro sounded best overall despite being the
smallest**. See [BENCHMARKS.md](BENCHMARKS.md) for the numbers behind this table.

### Local engine setup

Each local backend runs from its **own [uv](https://docs.astral.sh/uv/)-managed
virtualenv** under `.venvs/<name>` (isolated so heavy PyTorch stacks never collide
with each other or the system Python), with weights under `models/` or the Hugging
Face cache. Weights download once on first run, then everything runs offline. The
uniform script contract every engine implements is documented in
[`engines/CONTRACT.md`](engines/CONTRACT.md).

**Performance is CPU-bound.** The home server (Ryzen 7 PRO 8845HS, Radeon 780M
iGPU only, no CUDA/ROCm) runs all local engines on the CPU — the iGPU stays idle
(measured 0%). Expect Kokoro to be effortless, StyleTTS2 to be roughly real time,
and Chatterbox to be slow and RAM-hungry (~6.7 GB peak). See
[BENCHMARKS.md](BENCHMARKS.md).

### `--benchmark`

Wraps a run and writes a JSON resource-usage record (default `samples/bench/`) with
wall/CPU time, peak RSS, peak thread count, iGPU busy %, real-time factor, and
derived compute-seconds-per-minute-of-audio and CPU-seconds-per-1000-words figures.
Use it to estimate how heavy a full book will be before committing to a run.

```bash
tts --backend <minimax|kokoro|chatterbox|styletts2> --benchmark -o out.mp3 --file <text>
```

## API key resolution

The key is resolved in this order, and only ever flows into the `Authorization`
header. It is never printed (not in errors, not in `--dry-run`, not in logs).

1. `--api-key <KEY>` flag.
2. Env `MINIMAX_API_KEY`.
3. `--env-file <PATH>`: read `MINIMAX_API_KEY` from a dotenv-style file. Only
   that one key is read (a leading `export ` is tolerated, `#` comments and
   blank lines are ignored, and one pair of surrounding quotes is stripped);
   nothing else is imported into the environment. A readable file without the
   key falls through to the next source; a path that cannot be opened is a hard
   error.
4. Proton Pass via `pass-cli`: runs `pass-cli item view "<ref>"` (no shell)
   and uses the trimmed stdout. Default ref
   `pass://API Keys and tokens/Minimax/API Key`, overridable with `--pass-ref`
   or env `MINIMAX_PASS_REF`. `pass-cli` is taken from PATH and inherits the
   current environment (including `PROTON_PASS_KEY_PROVIDER`, which differs per
   machine and is not set by `tts`).

Env-first is deliberate, but cron is the special case. The `hermes` cron/gateway
environment does not export `MINIMAX_API_KEY` to spawned commands and cannot
reach `pass-cli`, and it runs with `approvals.cron_mode: deny`, which blocks any
`bash -lc '...'` command. So a cron job cannot grep the key out of a dotenv file
with a shell. `--env-file` solves this: it makes the whole thing a single direct
binary call with no shell, which the approval layer does not flag:

```bash
tts --env-file /home/sylvain/.hermes/.env --file script.txt -o out.mp3
```

The `pass-cli` path is the convenient route for interactive and Mac use, where
Proton Pass is unlocked. If no source yields a key, `tts` exits nonzero naming
both `MINIMAX_API_KEY` and the pass reference.

## Long text and chunking

A single `t2a_v2` request accepts at most 10000 characters. When the input is
longer, the CLI splits it on paragraph then sentence boundaries into sub-limit
chunks, requests each chunk, and concatenates the returned mp3 bytes into the
single output file.

Caveat: this is naive byte concatenation of independent mp3 segments. Most
players handle it, but you may hear minor seams at chunk joins and total-duration
metadata can be approximate. For seamless output, re-encode the result (for
example with `ffmpeg`) or keep individual inputs under the limit.

## Delivery policy

This CLI only renders audio to a file. It does not send anything. To deliver:

- Email: attach the mp3 via the `email` CLI (the `attachments:` list in the
  draft frontmatter). Do not inline audio, use a MEDIA line, or call a built-in
  `text_to_speech` tool for email.
- Signal: hand the file to the gateway with a `MEDIA:` line referencing the
  mp3 path; the gateway delivers it as a native voice note.
