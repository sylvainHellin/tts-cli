//! Local Python-engine backend (kokoro / chatterbox / styletts2).
//!
//! Each engine is a script `engines/<name>_infer.py` run inside its own uv venv
//! at `.venvs/<name>`, conforming to `engines/CONTRACT.md`: it reads a text file,
//! writes a WAV, and prints exactly one JSON line on stdout. In `--benchmark`
//! mode the engine is launched through `engines/bench_run.py` (in `.venvs/bench`)
//! which layers resource metrics around that same call.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::backend::bench::{self, BenchRecord};
use crate::backend::{Backend, SynthOut};
use crate::{AppError, Cli, EXIT_BACKEND};

/// Local engine backend. Owns a temp dir that holds the input-text file and the
/// engine's WAV output for the lifetime of the run.
pub struct PythonBackend<'a> {
    cli: &'a Cli,
    engine: &'static str,
    repo_root: PathBuf,
    tmp: tempfile::TempDir,
}

impl<'a> PythonBackend<'a> {
    pub fn new(cli: &'a Cli, engine: &'static str) -> Result<Self, AppError> {
        let repo_root = resolve_repo_root()?;
        let tmp = tempfile::Builder::new()
            .prefix("tts-engine-")
            .tempdir()
            .map_err(|e| AppError::new(EXIT_BACKEND, format!("cannot create temp dir: {}", e)))?;
        Ok(PythonBackend {
            cli,
            engine,
            repo_root,
            tmp,
        })
    }

    fn venv_python(&self) -> PathBuf {
        self.repo_root
            .join(".venvs")
            .join(self.engine)
            .join("bin")
            .join("python")
    }

    fn engine_script(&self) -> PathBuf {
        self.repo_root
            .join("engines")
            .join(format!("{}_infer.py", self.engine))
    }

    /// Build the base engine argv (python + script + contract flags). The user's
    /// `--voice` is forwarded only when explicitly set, so each engine keeps its
    /// own default voice otherwise. `--ref-audio` is forwarded when present.
    fn engine_argv(&self, text_file: &Path, out_wav: &Path) -> Vec<String> {
        let mut argv = vec![
            self.venv_python().to_string_lossy().into_owned(),
            self.engine_script().to_string_lossy().into_owned(),
            "--text-file".into(),
            text_file.to_string_lossy().into_owned(),
            "--out".into(),
            out_wav.to_string_lossy().into_owned(),
        ];
        if let Some(voice) = self.cli.explicit_voice() {
            argv.push("--voice".into());
            argv.push(voice.to_string());
        }
        if let Some(ref_audio) = &self.cli.ref_audio {
            argv.push("--ref-audio".into());
            argv.push(ref_audio.clone());
        }
        argv
    }
}

impl Backend for PythonBackend<'_> {
    fn text_limit(&self) -> usize {
        // Local engines chunk internally; keep them to a single call for the
        // passage-length inputs this feature targets.
        100_000
    }

    fn synthesize(&self, text: &str) -> Result<SynthOut, AppError> {
        self.preflight()?;

        let text_file = self.tmp.path().join("input.txt");
        let out_wav = self.tmp.path().join("out.wav");
        {
            let mut f = std::fs::File::create(&text_file).map_err(|e| {
                AppError::new(EXIT_BACKEND, format!("cannot write engine input: {}", e))
            })?;
            f.write_all(text.as_bytes()).map_err(|e| {
                AppError::new(EXIT_BACKEND, format!("cannot write engine input: {}", e))
            })?;
        }

        let engine_argv = self.engine_argv(&text_file, &out_wav);

        let (audio_seconds, sample_rate) = if self.cli.benchmark {
            self.run_benchmarked(text, &engine_argv, &out_wav)?
        } else {
            self.run_plain(&engine_argv, &out_wav)?
        };

        Ok(SynthOut::Wav {
            path: out_wav,
            audio_seconds,
            sample_rate,
        })
    }
}

impl PythonBackend<'_> {
    /// Fail early with a clear message if the venv or script is missing.
    fn preflight(&self) -> Result<(), AppError> {
        let py = self.venv_python();
        if !py.is_file() {
            return Err(AppError::new(
                EXIT_BACKEND,
                format!(
                    "engine venv not found: {} (expected .venvs/{}/bin/python under repo root {})",
                    py.display(),
                    self.engine,
                    self.repo_root.display()
                ),
            ));
        }
        let script = self.engine_script();
        if !script.is_file() {
            return Err(AppError::new(
                EXIT_BACKEND,
                format!("engine script not found: {}", script.display()),
            ));
        }
        Ok(())
    }

    /// Run the engine directly and parse its one-line contract JSON.
    fn run_plain(
        &self,
        engine_argv: &[String],
        out_wav: &Path,
    ) -> Result<(f64, i64), AppError> {
        let output = Command::new(&engine_argv[0])
            .args(&engine_argv[1..])
            .output()
            .map_err(|e| {
                AppError::new(
                    EXIT_BACKEND,
                    format!("failed to launch {} engine: {}", self.engine, e),
                )
            })?;

        // Stream the engine's stderr through so its progress/logs are visible.
        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }

        if !output.status.success() {
            return Err(AppError::new(
                EXIT_BACKEND,
                format!(
                    "{} engine failed ({}). {}",
                    self.engine,
                    output.status,
                    stderr_tail(&output.stderr)
                ),
            ));
        }

        let contract = parse_engine_line(&output.stdout).ok_or_else(|| {
            AppError::new(
                EXIT_BACKEND,
                format!(
                    "{} engine produced no valid JSON line. {}",
                    self.engine,
                    stderr_tail(&output.stderr)
                ),
            )
        })?;
        ensure_wav(out_wav, self.engine)?;
        Ok((contract.audio_seconds, contract.sample_rate))
    }

    /// Run the engine through the bench wrapper, then persist and summarize the
    /// resource record.
    fn run_benchmarked(
        &self,
        text: &str,
        engine_argv: &[String],
        out_wav: &Path,
    ) -> Result<(f64, i64), AppError> {
        let bench_py = self.repo_root.join(".venvs").join("bench").join("bin").join("python");
        let bench_script = self.repo_root.join("engines").join("bench_run.py");
        if !bench_py.is_file() {
            return Err(AppError::new(
                EXIT_BACKEND,
                format!(
                    "benchmark venv not found: {} (create it with: uv venv --python 3.12 .venvs/bench && uv pip install --python .venvs/bench/bin/python psutil)",
                    bench_py.display()
                ),
            ));
        }
        if !bench_script.is_file() {
            return Err(AppError::new(
                EXIT_BACKEND,
                format!("bench_run.py not found: {}", bench_script.display()),
            ));
        }

        let mut cmd = Command::new(&bench_py);
        cmd.arg(&bench_script).arg("--");
        cmd.args(engine_argv);

        let output = cmd.output().map_err(|e| {
            AppError::new(EXIT_BACKEND, format!("failed to launch bench wrapper: {}", e))
        })?;

        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        if !output.status.success() {
            return Err(AppError::new(
                EXIT_BACKEND,
                format!(
                    "benchmark run failed ({}). {}",
                    output.status,
                    stderr_tail(&output.stderr)
                ),
            ));
        }

        let raw = parse_bench_line(&output.stdout).ok_or_else(|| {
            AppError::new(
                EXIT_BACKEND,
                format!(
                    "bench wrapper produced no valid JSON line. {}",
                    stderr_tail(&output.stderr)
                ),
            )
        })?;

        let contract = raw
            .engine_json
            .as_ref()
            .and_then(contract_from_value)
            .ok_or_else(|| {
                AppError::new(
                    EXIT_BACKEND,
                    format!("{} engine emitted no valid contract JSON under benchmark", self.engine),
                )
            })?;
        ensure_wav(out_wav, self.engine)?;

        let record = BenchRecord::from_raw(self.engine, text, &self.repo_root, &raw, &contract);
        let out_path = bench::default_record_path(&self.repo_root, self.engine);
        if let Err(e) = record.write_json(&out_path) {
            eprintln!("tts: warning: could not write bench record: {}", e.msg);
        } else {
            eprintln!("tts: bench record -> {}", out_path.display());
        }
        eprintln!("{}", record.human_summary());

        Ok((contract.audio_seconds, contract.sample_rate))
    }
}

/// The parsed engine contract line.
#[derive(Clone)]
pub struct EngineContract {
    pub audio_seconds: f64,
    pub sample_rate: i64,
    #[allow(dead_code)]
    pub engine: String,
    #[allow(dead_code)]
    pub voice: String,
}

/// Parse the last valid JSON object from stdout into an [`EngineContract`].
pub fn parse_engine_line(stdout: &[u8]) -> Option<EngineContract> {
    let value = last_json_object(stdout)?;
    contract_from_value(&value)
}

/// Build an [`EngineContract`] from a JSON object with the contract fields.
pub fn contract_from_value(value: &serde_json::Value) -> Option<EngineContract> {
    Some(EngineContract {
        audio_seconds: value.get("audio_seconds")?.as_f64()?,
        sample_rate: value.get("sample_rate")?.as_i64()?,
        engine: value
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        voice: value
            .get("voice")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Parse the bench wrapper's combined JSON record.
pub fn parse_bench_line(stdout: &[u8]) -> Option<bench::RawBench> {
    let value = last_json_object(stdout)?;
    serde_json::from_value(value).ok()
}

/// Find the last line of stdout that parses as a JSON object.
fn last_json_object(stdout: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.is_object() {
                return Some(v);
            }
        }
    }
    None
}

/// Confirm the engine actually produced a non-empty WAV.
fn ensure_wav(out_wav: &Path, engine: &str) -> Result<(), AppError> {
    match std::fs::metadata(out_wav) {
        Ok(m) if m.len() > 0 => Ok(()),
        Ok(_) => Err(AppError::new(
            EXIT_BACKEND,
            format!("{} engine wrote an empty WAV", engine),
        )),
        Err(e) => Err(AppError::new(
            EXIT_BACKEND,
            format!("{} engine did not produce a WAV: {}", engine, e),
        )),
    }
}

/// Last ~800 bytes of stderr, for compact error context.
fn stderr_tail(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    const MAX: usize = 800;
    let tail: String = if s.len() > MAX {
        // Advance to a char boundary so slicing never panics mid-codepoint.
        let mut start = s.len() - MAX;
        while start < s.len() && !s.is_char_boundary(start) {
            start += 1;
        }
        format!("...{}", &s[start..])
    } else {
        s.to_string()
    };
    format!("stderr: {}", tail)
}

/// Resolve the repo root: `TTS_REPO_ROOT` env override, else the current dir if it
/// looks like the repo, else walk up from the executable's location.
fn resolve_repo_root() -> Result<PathBuf, AppError> {
    if let Ok(root) = std::env::var("TTS_REPO_ROOT") {
        let p = PathBuf::from(&root);
        if looks_like_repo(&p) {
            return Ok(p);
        }
        return Err(AppError::new(
            EXIT_BACKEND,
            format!(
                "TTS_REPO_ROOT={} is not a valid tts repo (need an engines/ dir plus .venvs/ or Cargo.toml)",
                root
            ),
        ));
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = walk_up_for_repo(&cwd) {
            return Ok(root);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(root) = walk_up_for_repo(dir) {
                return Ok(root);
            }
        }
    }

    Err(AppError::new(
        EXIT_BACKEND,
        "cannot locate the tts repo root (needs the engines/ and .venvs/ dirs). \
         Run from the repo, or set TTS_REPO_ROOT.",
    ))
}

/// Walk up from `start` looking for a dir that contains `engines/`.
fn walk_up_for_repo(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if looks_like_repo(dir) {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

fn looks_like_repo(dir: &Path) -> bool {
    // Require an `engines/` dir plus a `.venvs/` dir or `Cargo.toml`, so a random
    // ancestor that merely happens to contain an `engines/` dir is not mistaken
    // for this repo.
    dir.join("engines").is_dir() && (dir.join(".venvs").is_dir() || dir.join("Cargo.toml").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_contract_line() {
        let line = br#"{"audio_seconds": 12.34, "sample_rate": 24000, "engine": "kokoro", "voice": "af_heart"}"#;
        let c = parse_engine_line(line).expect("parses");
        assert!((c.audio_seconds - 12.34).abs() < 1e-9);
        assert_eq!(c.sample_rate, 24000);
        assert_eq!(c.engine, "kokoro");
        assert_eq!(c.voice, "af_heart");
    }

    #[test]
    fn ignores_leading_log_lines_and_takes_last_json() {
        let out = b"loading model...\nsome warning\n{\"audio_seconds\": 5.0, \"sample_rate\": 24000, \"engine\": \"styletts2\", \"voice\": \"libritts\"}\n";
        let c = parse_engine_line(out).expect("parses last line");
        assert_eq!(c.sample_rate, 24000);
        assert_eq!(c.engine, "styletts2");
    }

    #[test]
    fn rejects_line_missing_required_fields() {
        // No audio_seconds -> not a valid contract.
        let line = br#"{"sample_rate": 24000, "engine": "kokoro"}"#;
        assert!(parse_engine_line(line).is_none());
    }

    #[test]
    fn no_json_returns_none() {
        assert!(parse_engine_line(b"just logs, no json here").is_none());
    }

    #[test]
    fn parses_bench_wrapper_record() {
        let line = br#"{"wall_seconds": 1.29, "cpu_user_s": 4.5, "cpu_sys_s": 0.39, "peak_rss_bytes": 572616704, "peak_threads": 23, "igpu_busy_peak": 0.0, "igpu_busy_mean": 0.0, "engine_json": {"audio_seconds": 2.03, "sample_rate": 24000, "engine": "kokoro", "voice": "af_heart"}}"#;
        let raw = parse_bench_line(line).expect("parses bench record");
        assert!((raw.wall_seconds - 1.29).abs() < 1e-9);
        assert_eq!(raw.peak_threads, 23);
        let contract = raw.engine_json.as_ref().and_then(contract_from_value).expect("engine json");
        assert!((contract.audio_seconds - 2.03).abs() < 1e-9);
    }

    #[test]
    fn looks_like_repo_detects_engines_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!looks_like_repo(tmp.path()));
        // An `engines/` dir alone is not enough (could be a wrong ancestor).
        std::fs::create_dir(tmp.path().join("engines")).unwrap();
        assert!(!looks_like_repo(tmp.path()));
        // engines/ + Cargo.toml qualifies.
        std::fs::write(tmp.path().join("Cargo.toml"), b"[package]\n").unwrap();
        assert!(looks_like_repo(tmp.path()));
    }

    #[test]
    fn looks_like_repo_accepts_engines_plus_venvs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("engines")).unwrap();
        std::fs::create_dir(tmp.path().join(".venvs")).unwrap();
        assert!(looks_like_repo(tmp.path()));
    }

    #[test]
    fn stderr_tail_handles_multibyte_char_at_boundary() {
        // Build a stderr buffer longer than MAX (800) whose byte at the cut
        // point (len-800) lands mid-codepoint, to prove no panic on slicing.
        let mut s = String::new();
        // Leading filler so total length comfortably exceeds 800 bytes.
        s.push_str(&"a".repeat(500));
        // A run of 3-byte chars around the cut region so a naive len-800 offset
        // is very likely to fall mid-codepoint.
        s.push_str(&"\u{4e2d}".repeat(200)); // 200 * 3 = 600 bytes
        s.push_str(&"z".repeat(50));
        let out = stderr_tail(s.as_bytes());
        assert!(out.starts_with("stderr: ..."));
        // The result must be valid UTF-8 (it is, being a String) and non-empty.
        assert!(out.len() > "stderr: ...".len());
    }
}
