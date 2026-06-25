//! MiniMax t2a_v2 text-to-speech CLI.
//!
//! Renders text to an mp3 file and prints the output path. The HTTP client is
//! rustls-based (via ureq) so the binary needs no OpenSSL system library on a
//! headless box. Audio comes back as a hex string and is decoded to bytes.

use std::io::Read;
use std::process::Command;

use clap::Parser;
use serde::Serialize;

const API_URL: &str = "https://api.minimax.io/v1/t2a_v2";
const TEXT_LIMIT: usize = 10000;
const DEFAULT_PASS_REF: &str = "pass://API Keys and tokens/Minimax/API Key";

// Exit codes.
const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_API: i32 = 3;
const EXIT_NETWORK: i32 = 4;
const EXIT_IO: i32 = 5;

/// Render text to an mp3 via the MiniMax t2a_v2 API.
//
// No `Debug` derive on purpose: this struct holds `--api-key`, and deriving
// `Debug` would let a future `{:?}` leak the secret. Nothing here needs it.
#[derive(Parser)]
#[command(
    name = "tts",
    about = "Render text to an mp3 via the MiniMax t2a_v2 API.",
    allow_negative_numbers = true
)]
struct Cli {
    /// Text to speak, or '-' to read stdin.
    text: Option<String>,

    /// Read input text from this file.
    #[arg(long)]
    file: Option<String>,

    /// mp3 file to write.
    #[arg(short = 'o', long)]
    output: String,

    #[arg(long, default_value = "speech-2.8-hd")]
    model: String,

    #[arg(long, default_value = "English_Upbeat_Woman")]
    voice: String,

    #[arg(long, default_value_t = 1.0)]
    speed: f64,

    #[arg(long, default_value_t = 1.0)]
    vol: f64,

    #[arg(long, default_value_t = 0)]
    pitch: i64,

    #[arg(long, default_value = "neutral")]
    emotion: String,

    #[arg(long = "sample-rate", default_value_t = 32000)]
    sample_rate: i64,

    #[arg(long, default_value_t = 128000)]
    bitrate: i64,

    #[arg(long, default_value = "mp3")]
    format: String,

    #[arg(long, default_value_t = 1)]
    channel: i64,

    /// MiniMax group id. Appended as ?GroupId= only when set.
    #[arg(long = "group-id", env = "MINIMAX_GROUP_ID")]
    group_id: Option<String>,

    /// MiniMax API key. Highest-priority key source. Never logged.
    #[arg(long = "api-key")]
    api_key: Option<String>,

    /// Read MINIMAX_API_KEY from this dotenv file when no higher-priority key is
    /// set. Lets cron call the binary directly, with no shell, to pick up the
    /// key (e.g. --env-file /home/sylvain/.hermes/.env). Only MINIMAX_API_KEY is
    /// read; nothing else is imported into the environment.
    #[arg(long = "env-file")]
    env_file: Option<String>,

    /// Proton Pass reference resolved via pass-cli when no key is set otherwise.
    #[arg(
        long = "pass-ref",
        env = "MINIMAX_PASS_REF",
        default_value = DEFAULT_PASS_REF
    )]
    pass_ref: String,

    /// Error instead of chunking when text exceeds the 10000-char limit.
    #[arg(long = "no-chunk")]
    no_chunk: bool,

    /// Print the request body (text elided, no key) without sending.
    #[arg(long = "dry-run", visible_alias = "print-payload")]
    dry_run: bool,
}

#[derive(Serialize)]
struct VoiceSetting {
    voice_id: String,
    speed: f64,
    vol: f64,
    pitch: i64,
    emotion: String,
}

#[derive(Serialize)]
struct AudioSetting {
    sample_rate: i64,
    bitrate: i64,
    format: String,
    channel: i64,
}

#[derive(Serialize)]
struct Payload {
    model: String,
    text: String,
    voice_setting: VoiceSetting,
    audio_setting: AudioSetting,
}

/// An error carrying the process exit code to surface it with.
//
// `Debug` is safe here: this only ever holds an exit code and a human message,
// never the API key (unlike `Cli`, which deliberately has no `Debug`).
#[derive(Debug)]
struct AppError {
    code: i32,
    msg: String,
}

impl AppError {
    fn new(code: i32, msg: impl Into<String>) -> Self {
        AppError {
            code,
            msg: msg.into(),
        }
    }
}

fn count(s: &str) -> usize {
    s.chars().count()
}

/// Assemble the exact nested t2a_v2 request body.
fn build_payload(cli: &Cli, text: &str) -> Payload {
    Payload {
        model: cli.model.clone(),
        text: text.to_string(),
        voice_setting: VoiceSetting {
            voice_id: cli.voice.clone(),
            speed: cli.speed,
            vol: cli.vol,
            pitch: cli.pitch,
            emotion: cli.emotion.clone(),
        },
        audio_setting: AudioSetting {
            sample_rate: cli.sample_rate,
            bitrate: cli.bitrate,
            format: cli.format.clone(),
            channel: cli.channel,
        },
    }
}

/// Split text into <=limit-char chunks on paragraph then sentence boundaries.
///
/// Greedily packs whole paragraphs; a paragraph over the limit is broken on
/// sentence terminators; a single sentence over the limit is hard-sliced. No
/// character content is dropped, and chunk order matches input order.
fn split_text(text: &str, limit: usize) -> Vec<String> {
    if count(text) <= limit {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();

    for para in text.split("\n\n") {
        let piece = if buf.is_empty() {
            para.to_string()
        } else {
            format!("\n\n{}", para)
        };
        if count(&buf) + count(&piece) <= limit {
            buf.push_str(&piece);
            continue;
        }
        if !buf.is_empty() {
            chunks.push(std::mem::take(&mut buf));
        }
        if count(para) <= limit {
            buf = para.to_string();
            continue;
        }
        // Paragraph itself too long: break on sentences.
        for sentence in split_sentences(para, limit) {
            let cand = if buf.is_empty() {
                sentence.clone()
            } else {
                format!(" {}", sentence)
            };
            if count(&buf) + count(&cand) <= limit {
                buf.push_str(&cand);
            } else {
                if !buf.is_empty() {
                    chunks.push(std::mem::take(&mut buf));
                }
                buf = sentence;
            }
        }
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }
    chunks
}

/// Yield sentence-ish fragments, hard-slicing any that exceed the limit.
fn split_sentences(para: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut sentence = String::new();
    for ch in para.chars() {
        sentence.push(ch);
        if matches!(ch, '.' | '!' | '?' | '\n') {
            out.push(std::mem::take(&mut sentence));
        }
    }
    if !sentence.is_empty() {
        out.push(sentence);
    }

    let mut sliced: Vec<String> = Vec::new();
    for frag in out {
        let chars: Vec<char> = frag.chars().collect();
        if chars.len() <= limit {
            if !chars.is_empty() {
                sliced.push(frag);
            }
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let end = (start + limit).min(chars.len());
            sliced.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    sliced
}

/// Read MINIMAX_API_KEY from a dotenv-style file.
///
/// Returns `Ok(Some(value))` when the key is present and non-empty, `Ok(None)`
/// when the file is readable but has no usable `MINIMAX_API_KEY`, and `Err` when
/// the given path cannot be opened. Tolerates a leading `export `, ignores blank
/// lines and `#` comments, and strips one pair of surrounding quotes plus
/// surrounding whitespace. Only `MINIMAX_API_KEY` is read; nothing is exported
/// into the process environment.
fn key_from_env_file(path: &str) -> Result<Option<String>, AppError> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        AppError::new(EXIT_USAGE, format!("cannot read --env-file {}: {}", path, e))
    })?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "MINIMAX_API_KEY" {
            continue;
        }
        let value = strip_one_quote_pair(value.trim());
        if value.is_empty() {
            return Ok(None);
        }
        return Ok(Some(value.to_string()));
    }
    Ok(None)
}

/// Strip one matching pair of surrounding single or double quotes, if present.
fn strip_one_quote_pair(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 {
        let (first, last) = (b[0], b[b.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Resolve the API key: flag, env MINIMAX_API_KEY, --env-file, then pass-cli.
///
/// The key only ever flows into the Authorization header; it is never printed.
fn resolve_api_key(cli: &Cli) -> Result<String, AppError> {
    if let Some(key) = &cli.api_key {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    if let Ok(key) = std::env::var("MINIMAX_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
    }

    // Dotenv file (e.g. ~/.hermes/.env). Lets the cron call us without a shell
    // to grep the key out, which `approvals.cron_mode: deny` would block.
    if let Some(path) = &cli.env_file {
        if let Some(key) = key_from_env_file(path)? {
            return Ok(key);
        }
    }

    // Proton Pass via pass-cli. Inherit the environment as-is so the correct
    // PROTON_PASS_KEY_PROVIDER for this machine is used.
    let no_key_msg = format!(
        "no API key. Set $MINIMAX_API_KEY, pass --api-key, or store it at \"{}\" for pass-cli.",
        cli.pass_ref
    );
    match Command::new("pass-cli")
        .arg("item")
        .arg("view")
        .arg(&cli.pass_ref)
        .output()
    {
        Ok(out) if out.status.success() => {
            let key = String::from_utf8_lossy(&out.stdout)
                .trim_end()
                .to_string();
            if key.is_empty() {
                Err(AppError::new(EXIT_USAGE, no_key_msg))
            } else {
                Ok(key)
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            Err(AppError::new(
                EXIT_USAGE,
                format!("pass-cli failed to read \"{}\": {}", cli.pass_ref, stderr),
            ))
        }
        Err(_) => Err(AppError::new(EXIT_USAGE, no_key_msg)),
    }
}

/// Resolve input text from the positional arg, --file, or stdin ('-').
fn read_input_text(cli: &Cli) -> Result<String, AppError> {
    if let Some(path) = &cli.file {
        return std::fs::read_to_string(path)
            .map_err(|e| AppError::new(EXIT_USAGE, format!("cannot read {}: {}", path, e)));
    }
    match cli.text.as_deref() {
        Some("-") => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| AppError::new(EXIT_USAGE, format!("cannot read stdin: {}", e)))?;
            Ok(s)
        }
        Some(t) => Ok(t.to_string()),
        None => Err(AppError::new(
            EXIT_USAGE,
            "no input text: pass TEXT, --file PATH, or '-' for stdin",
        )),
    }
}

/// Send one chunk to t2a_v2 and return the decoded mp3 bytes.
fn synthesize_chunk(cli: &Cli, text: &str, api_key: &str) -> Result<Vec<u8>, AppError> {
    let payload = build_payload(cli, text);
    let body = serde_json::to_string(&payload)
        .map_err(|e| AppError::new(EXIT_API, format!("could not encode request: {}", e)))?;

    let url = match &cli.group_id {
        Some(gid) if !gid.is_empty() => format!("{}?GroupId={}", API_URL, gid),
        _ => API_URL.to_string(),
    };

    let response = ureq::post(&url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", api_key))
        .send_string(&body);

    // ureq's `into_string()` caps the body at 10 MB, but a full 10000-char chunk
    // returns a hex-encoded mp3 well over that (~20-25 MB), so read uncapped via
    // `into_reader()` and parse the JSON from bytes.
    let body_bytes = match response {
        Ok(resp) => read_body(resp)
            .map_err(|e| AppError::new(EXIT_NETWORK, format!("could not read response: {}", e)))?,
        Err(ureq::Error::Status(code, resp)) => {
            let detail = String::from_utf8_lossy(&read_body(resp).unwrap_or_default()).into_owned();
            return Err(AppError::new(
                EXIT_API,
                format!("HTTP {} from t2a_v2: {}", code, detail),
            ));
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(AppError::new(
                EXIT_NETWORK,
                format!("network failure reaching t2a_v2: {}", t),
            ));
        }
    };

    parse_t2a_response(&body_bytes)
}

/// Read a ureq response body without the 10 MB `into_string()` cap.
fn read_body(resp: ureq::Response) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

/// Parse a t2a_v2 JSON response (as raw bytes) and return the decoded mp3 bytes.
fn parse_t2a_response(body_bytes: &[u8]) -> Result<Vec<u8>, AppError> {
    let result: serde_json::Value = serde_json::from_slice(body_bytes)
        .map_err(|e| AppError::new(EXIT_API, format!("could not parse t2a_v2 response: {}", e)))?;

    let status_code = result
        .get("base_resp")
        .and_then(|b| b.get("status_code"))
        .and_then(|c| c.as_i64());
    if status_code != Some(0) {
        let msg = result
            .get("base_resp")
            .and_then(|b| b.get("status_msg"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(AppError::new(
            EXIT_API,
            format!(
                "t2a_v2 error (status_code={}): {}",
                status_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                msg
            ),
        ));
    }

    let audio_hex = result
        .get("data")
        .and_then(|d| d.get("audio"))
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::new(EXIT_API, "t2a_v2 success response had no data.audio field"))?;

    hex::decode(audio_hex)
        .map_err(|e| AppError::new(EXIT_API, format!("data.audio was not valid hex: {}", e)))
}

fn run(cli: &Cli) -> Result<(), AppError> {
    let text = read_input_text(cli)?;

    if cli.dry_run {
        // Serialize the struct (not a Value map) so the printed body matches the
        // exact field order sent on the wire; text is elided and no key appears.
        let redacted = format!("<{} chars elided>", count(&text));
        let payload = build_payload(cli, &redacted);
        let pretty = serde_json::to_string_pretty(&payload)
            .map_err(|e| AppError::new(EXIT_API, format!("could not encode request: {}", e)))?;
        println!("{}", pretty);
        return Ok(());
    }

    let api_key = resolve_api_key(cli)?;

    let chunks = if count(&text) > TEXT_LIMIT {
        if cli.no_chunk {
            return Err(AppError::new(
                EXIT_USAGE,
                format!(
                    "text is {} chars, over the {} limit, and --no-chunk was set.",
                    count(&text),
                    TEXT_LIMIT
                ),
            ));
        }
        split_text(&text, TEXT_LIMIT)
    } else {
        vec![text]
    };

    let mut audio: Vec<u8> = Vec::new();
    for chunk in &chunks {
        audio.extend(synthesize_chunk(cli, chunk, &api_key)?);
    }

    std::fs::write(&cli.output, &audio)
        .map_err(|e| AppError::new(EXIT_IO, format!("cannot write {}: {}", cli.output, e)))?;

    let abs = std::fs::canonicalize(&cli.output)
        .unwrap_or_else(|_| std::path::PathBuf::from(&cli.output));
    println!("{}", abs.display());
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => std::process::exit(EXIT_OK),
        Err(e) => {
            eprintln!("tts: {}", e.msg);
            std::process::exit(e.code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("args parse")
    }

    #[test]
    fn payload_serializes_to_documented_json_with_defaults() {
        let cli = cli_from(&["tts", "-o", "/tmp/x.mp3", "hello world"]);
        let payload = build_payload(&cli, "hello world");
        let got = serde_json::to_value(&payload).unwrap();
        let expected = serde_json::json!({
            "model": "speech-2.8-hd",
            "text": "hello world",
            "voice_setting": {
                "voice_id": "English_Upbeat_Woman",
                "speed": 1.0,
                "vol": 1.0,
                "pitch": 0,
                "emotion": "neutral"
            },
            "audio_setting": {
                "sample_rate": 32000,
                "bitrate": 128000,
                "format": "mp3",
                "channel": 1
            }
        });
        assert_eq!(got, expected);
    }

    #[test]
    fn payload_honors_overrides() {
        let cli = cli_from(&[
            "tts", "-o", "/tmp/x.mp3", "--voice", "German_Calm_Man", "--speed", "1.2", "--pitch",
            "-2", "hi",
        ]);
        let payload = build_payload(&cli, "hi");
        assert_eq!(payload.voice_setting.voice_id, "German_Calm_Man");
        assert_eq!(payload.voice_setting.speed, 1.2);
        assert_eq!(payload.voice_setting.pitch, -2);
    }

    #[test]
    fn hex_decode_known_string() {
        // Audio comes back as hex; "494433" == b"ID3" (the mp3/ID3 tag start).
        assert_eq!(hex::decode("494433").unwrap(), vec![0x49u8, 0x44, 0x33]);
        assert_eq!(hex::decode("494433").unwrap(), b"ID3");
        assert_eq!(hex::decode("48656c6c6f").unwrap(), b"Hello");
    }

    #[test]
    fn chunking_splits_over_limit_without_dropping_content() {
        let para = "Sentence one is here. Sentence two is also here. ".repeat(400);
        let para = para.trim();
        let text = [para, para, para].join("\n\n");
        assert!(count(&text) > TEXT_LIMIT);

        let chunks = split_text(&text, TEXT_LIMIT);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| count(c) <= TEXT_LIMIT));

        // No character content dropped; only separators differ.
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let joined: String = chunks.concat();
        assert_eq!(strip(&joined), strip(&text));
    }

    #[test]
    fn parse_t2a_response_decodes_audio_from_bytes() {
        // Same code path as the success branch: parse JSON from raw bytes (as a
        // reader would yield) and hex-decode data.audio. "494433" == b"ID3".
        let body = br#"{"base_resp":{"status_code":0,"status_msg":"success"},"data":{"audio":"494433"}}"#;
        match parse_t2a_response(body) {
            Ok(audio) => assert_eq!(audio, b"ID3"),
            Err(e) => panic!("expected ok, got error: {}", e.msg),
        }
    }

    #[test]
    fn parse_t2a_response_surfaces_api_error() {
        let body = br#"{"base_resp":{"status_code":1004,"status_msg":"auth failed"}}"#;
        match parse_t2a_response(body) {
            Ok(_) => panic!("expected an error"),
            Err(e) => {
                assert_eq!(e.code, EXIT_API);
                assert!(e.msg.contains("1004"));
                assert!(e.msg.contains("auth failed"));
            }
        }
    }

    #[test]
    fn chunking_passthrough_under_limit() {
        assert_eq!(split_text("short text", TEXT_LIMIT), vec!["short text"]);
    }

    #[test]
    fn chunking_hard_slices_single_huge_sentence() {
        // One sentence with no boundaries, over twice the limit.
        let text = "x".repeat(TEXT_LIMIT * 2 + 5);
        let chunks = split_text(&text, TEXT_LIMIT);
        assert!(chunks.iter().all(|c| count(c) <= TEXT_LIMIT));
        assert_eq!(chunks.concat(), text);
    }

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("tts-{}-{}.env", name, std::process::id()));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn env_file_extracts_quoted_key() {
        let path = write_temp(
            "quoted",
            "# minimax credentials\nOTHER_VAR=should-be-ignored\nMINIMAX_API_KEY=\"sk-test123\"\n",
        );
        let got = key_from_env_file(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(got, Some("sk-test123".to_string()));
    }

    #[test]
    fn env_file_tolerates_export_prefix() {
        let path = write_temp("export", "export MINIMAX_API_KEY=plainvalue\n");
        let got = key_from_env_file(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(got, Some("plainvalue".to_string()));
    }

    #[test]
    fn env_file_missing_key_returns_none() {
        let path = write_temp("nokey", "# nothing useful here\nFOO=bar\n");
        let got = key_from_env_file(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(got, None);
    }

    #[test]
    fn env_file_unopenable_path_errors() {
        let err = key_from_env_file("/no/such/tts/env/file").unwrap_err();
        assert_eq!(err.code, EXIT_USAGE);
        assert!(err.msg.contains("--env-file"));
    }
}
