---
name: rendering-speech
description: Render any text to spoken audio with the `tts` CLI (MiniMax t2a_v2), then deliver the mp3 per channel. Use for any voice note, narration, read-aloud, audio briefing, or "say this" / "send me audio" / "as a voice message" request, including narrating a paper or a long answer.
---
# Rendering speech

When a request wants audio rather than text (a voice note, narration, read-aloud,
audio briefing, or an explicit "send me this as audio"), use the `tts` CLI to
render an mp3, then deliver the file on the current channel. Do not use any
built-in text-to-speech tool: `tts` is the one path, so voice, model, and limits
stay consistent across channels.

## Render

```bash
tts -o /tmp/reply-$(date +%s).mp3 "the text to speak"
```

Render to a temp mp3 and capture the absolute path it prints on stdout. For long
text (a paper, a long answer), pass `--file` or pipe via `-`; the CLI chunks past
the 10000-char limit on its own. Defaults (model `speech-2.8-hd`, voice
`English_Upbeat_Woman`) match the daily briefing voice, so override only when
asked. Full flags in `tts --help` and the repo README.

The API key resolves in order: `--api-key`, then env `MINIMAX_API_KEY`, then
`--env-file <PATH>` (read `MINIMAX_API_KEY` from a dotenv file), then Proton Pass
via `pass-cli` (ref `pass://API Keys and tokens/Minimax/API Key`, override with
`--pass-ref`). For cron/gateway use, pass `--env-file /home/sylvain/.hermes/.env`:
that environment does not export the key and `approvals.cron_mode: deny` blocks
`bash -lc`, so a single direct call with `--env-file` (no shell) is the only path
that works there. `pass-cli` is the route for interactive and Mac use. If no
source yields a key the CLI exits nonzero naming both; surface that, do not retry
blindly. The key is never printed.

## Deliver

`tts` only renders the file. Deliver it per channel:

- Email: attach the mp3 via the `email` CLI (`attachments:` in the draft
  frontmatter). Never deliver email audio via a MEDIA line or a built-in
  text_to_speech tool.
- Signal: emit a `MEDIA:` line pointing at the mp3 path so the gateway sends it
  as a native voice note.

Match the channel: do not attach an mp3 to a Signal reply, and do not put a
MEDIA line in an email.
