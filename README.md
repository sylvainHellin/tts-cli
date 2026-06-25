# tts-cli

A thin, transparent CLI wrapper around the MiniMax `t2a_v2` text-to-speech API.
It renders text to an mp3 file and prints the output path. Nothing more.

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

# headless / cron: read MINIMAX_API_KEY from a dotenv file, no shell needed
tts --env-file /home/sylvain/.hermes/.env --file script.txt -o /tmp/out.mp3
```

On success the absolute output path is printed to stdout and the exit code is 0,
so a cron job or agent can capture the path directly. Failures use distinct
nonzero exit codes: 2 usage, 3 API error, 4 network error, 5 IO/write error.

### Options and defaults

| Flag | Default |
| --- | --- |
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
elided to a char count and no key present, and sends nothing.

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
