//! TTS backend abstraction.
//!
//! `minimax` is the original cloud path (returns mp3 bytes directly). The local
//! engines (`kokoro`, `chatterbox`, `styletts2`) are Python scripts, each in its
//! own uv venv, shelled out to via [`PythonBackend`]; they write a WAV that the
//! caller normalizes to the requested output format with ffmpeg.

pub mod bench;
pub mod minimax;
pub mod python;

use clap::ValueEnum;

use crate::{AppError, Cli, EXIT_BACKEND};

/// Selectable synthesis backend.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum BackendKind {
    /// MiniMax t2a_v2 cloud API (default; unchanged behavior).
    Minimax,
    /// Kokoro-82M, ONNX, CPU. Fast (~0.25 RTF).
    Kokoro,
    /// Chatterbox, torch, CPU. Expressive, slow (~7 RTF).
    Chatterbox,
    /// StyleTTS2, torch, CPU. ~real-time.
    Styletts2,
}

impl BackendKind {
    /// The engine name used for the `.venvs/<name>` and `engines/<name>_infer.py`
    /// paths. `None` for MiniMax (not a local python engine).
    pub fn engine_name(self) -> Option<&'static str> {
        match self {
            BackendKind::Minimax => None,
            BackendKind::Kokoro => Some("kokoro"),
            BackendKind::Chatterbox => Some("chatterbox"),
            BackendKind::Styletts2 => Some("styletts2"),
        }
    }

}

/// Result of a synthesis call.
///
/// MiniMax yields ready-to-write bytes in the requested container. Local engines
/// yield a WAV path (owned by a temp dir kept alive elsewhere) plus the metrics
/// their contract line reported, which the caller re-encodes with ffmpeg.
pub enum SynthOut {
    /// Encoded audio bytes ready to write to `--output` as-is (MiniMax mp3).
    Bytes(Vec<u8>),
    /// A WAV file produced by a local engine, to be normalized by the caller.
    /// The metrics are informational; benchmark mode reports the authoritative
    /// numbers separately.
    Wav {
        path: std::path::PathBuf,
        #[allow(dead_code)]
        audio_seconds: f64,
        #[allow(dead_code)]
        sample_rate: i64,
    },
}

/// A synthesis backend. Given text, produce audio (bytes or a WAV file).
pub trait Backend {
    /// Per-engine chunking cap in characters. MiniMax uses the API's 10000-char
    /// limit; local engines chunk internally, so their cap is effectively "one
    /// call" for the passage-length inputs this tool renders.
    fn text_limit(&self) -> usize;

    /// Synthesize one chunk of text.
    fn synthesize(&self, text: &str) -> Result<SynthOut, AppError>;
}

/// Build the backend selected on the CLI.
pub fn build_backend(cli: &Cli) -> Result<Box<dyn Backend + '_>, AppError> {
    match cli.backend {
        BackendKind::Minimax => Ok(Box::new(minimax::MiniMaxBackend::new(cli))),
        kind => {
            let engine = kind.engine_name().ok_or_else(|| {
                AppError::new(EXIT_BACKEND, "internal: local backend without engine name")
            })?;
            Ok(Box::new(python::PythonBackend::new(cli, engine)?))
        }
    }
}
