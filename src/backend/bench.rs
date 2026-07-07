//! Benchmark record: resource metrics from `engines/bench_run.py` plus the
//! derived headline numbers (real-time factor, CPU-seconds per 1000 words).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backend::python::EngineContract;
use crate::{AppError, EXIT_IO};

/// Raw resource metrics as emitted by `bench_run.py`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RawBench {
    pub wall_seconds: f64,
    pub cpu_user_s: f64,
    pub cpu_sys_s: f64,
    pub peak_rss_bytes: u64,
    pub peak_threads: u64,
    pub igpu_busy_peak: Option<f64>,
    pub igpu_busy_mean: Option<f64>,
    pub engine_json: Option<serde_json::Value>,
}

/// A full benchmark record: engine identity, raw metrics, and derived numbers.
#[derive(Debug, Serialize)]
pub struct BenchRecord {
    pub engine: String,
    pub voice: String,
    pub word_count: usize,
    pub char_count: usize,
    pub audio_seconds: f64,
    pub sample_rate: i64,

    pub wall_seconds: f64,
    pub cpu_user_s: f64,
    pub cpu_sys_s: f64,
    pub cpu_total_s: f64,
    pub peak_rss_bytes: u64,
    pub peak_rss_mb: f64,
    pub peak_threads: u64,
    pub igpu_busy_peak: Option<f64>,
    pub igpu_busy_mean: Option<f64>,

    /// Wall time / audio duration. <1 is faster than real time.
    pub rtf_wall: f64,
    /// Wall seconds of compute per minute of produced audio.
    pub compute_s_per_min_audio: f64,
    /// Total CPU seconds (user+sys) per 1000 input words.
    pub cpu_s_per_1000_words: f64,

    /// Model weights on disk, when discoverable under `models/<engine>/`.
    pub model_disk_bytes: Option<u64>,
}

impl BenchRecord {
    /// Combine raw bench metrics with the engine contract and input text.
    pub fn from_raw(
        engine: &str,
        text: &str,
        repo_root: &Path,
        raw: &RawBench,
        contract: &EngineContract,
    ) -> Self {
        let word_count = text.split_whitespace().count();
        let char_count = text.chars().count();
        let cpu_total_s = raw.cpu_user_s + raw.cpu_sys_s;
        let audio_seconds = contract.audio_seconds;

        let rtf_wall = if audio_seconds > 0.0 {
            raw.wall_seconds / audio_seconds
        } else {
            0.0
        };
        let compute_s_per_min_audio = rtf_wall * 60.0;
        let cpu_s_per_1000_words = if word_count > 0 {
            cpu_total_s / word_count as f64 * 1000.0
        } else {
            0.0
        };

        BenchRecord {
            engine: engine.to_string(),
            voice: contract.voice.clone(),
            word_count,
            char_count,
            audio_seconds,
            sample_rate: contract.sample_rate,
            wall_seconds: raw.wall_seconds,
            cpu_user_s: raw.cpu_user_s,
            cpu_sys_s: raw.cpu_sys_s,
            cpu_total_s,
            peak_rss_bytes: raw.peak_rss_bytes,
            peak_rss_mb: raw.peak_rss_bytes as f64 / (1024.0 * 1024.0),
            peak_threads: raw.peak_threads,
            igpu_busy_peak: raw.igpu_busy_peak,
            igpu_busy_mean: raw.igpu_busy_mean,
            rtf_wall,
            compute_s_per_min_audio,
            cpu_s_per_1000_words,
            model_disk_bytes: dir_size(&repo_root.join("models").join(engine)),
        }
    }

    /// Write the record as pretty JSON, creating parent dirs as needed.
    pub fn write_json(&self, path: &Path) -> Result<(), AppError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::new(EXIT_IO, format!("cannot create {}: {}", parent.display(), e))
            })?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::new(EXIT_IO, format!("cannot encode bench record: {}", e)))?;
        std::fs::write(path, json)
            .map_err(|e| AppError::new(EXIT_IO, format!("cannot write {}: {}", path.display(), e)))
    }

    /// A concise multi-line summary for stderr.
    pub fn human_summary(&self) -> String {
        let igpu = self
            .igpu_busy_peak
            .map(|p| format!("{:.0}% peak", p))
            .unwrap_or_else(|| "n/a".to_string());
        let disk = self
            .model_disk_bytes
            .map(|b| format!("{:.0} MB", b as f64 / (1024.0 * 1024.0)))
            .unwrap_or_else(|| "n/a".to_string());
        format!(
            "── benchmark [{engine}] ──\n\
             words={words}  audio={audio:.1}s  wall={wall:.1}s\n\
             RTF(wall)={rtf:.2}  compute_s/min_audio={cpm:.1}  cpu_s/1000w={cpw:.1}\n\
             cpu(user+sys)={cpu:.1}s  peak_rss={rss:.0} MB  peak_threads={threads}\n\
             iGPU={igpu}  model_on_disk={disk}",
            engine = self.engine,
            words = self.word_count,
            audio = self.audio_seconds,
            wall = self.wall_seconds,
            rtf = self.rtf_wall,
            cpm = self.compute_s_per_min_audio,
            cpw = self.cpu_s_per_1000_words,
            cpu = self.cpu_total_s,
            rss = self.peak_rss_mb,
            threads = self.peak_threads,
            igpu = igpu,
            disk = disk,
        )
    }
}

/// Default record path: `samples/bench/<engine>-<unix_ts>.json` under repo root.
pub fn default_record_path(repo_root: &Path, engine: &str) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    repo_root
        .join("samples")
        .join("bench")
        .join(format!("{}-{}.json", engine, ts))
}

/// Best-effort recursive size of a directory in bytes; `None` if it is absent.
fn dir_size(dir: &Path) -> Option<u64> {
    if !dir.is_dir() {
        return None;
    }
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(path),
                Ok(ft) if ft.is_file() => {
                    if let Ok(m) = entry.metadata() {
                        total += m.len();
                    }
                }
                _ => {}
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::python::EngineContract;

    fn contract(audio: f64) -> EngineContract {
        EngineContract {
            audio_seconds: audio,
            sample_rate: 24000,
            engine: "kokoro".into(),
            voice: "af_heart".into(),
        }
    }

    fn raw(wall: f64, user: f64, sys: f64) -> RawBench {
        RawBench {
            wall_seconds: wall,
            cpu_user_s: user,
            cpu_sys_s: sys,
            peak_rss_bytes: 512 * 1024 * 1024,
            peak_threads: 16,
            igpu_busy_peak: Some(0.0),
            igpu_busy_mean: Some(0.0),
            engine_json: None,
        }
    }

    #[test]
    fn derived_metrics_math() {
        // 10s wall, 40s audio -> RTF 0.25, 15 compute-s per audio-minute.
        let text = "one two three four five"; // 5 words
        let rec = BenchRecord::from_raw("kokoro", text, Path::new("/nonexistent"), &raw(10.0, 4.0, 1.0), &contract(40.0));
        assert!((rec.rtf_wall - 0.25).abs() < 1e-9);
        assert!((rec.compute_s_per_min_audio - 15.0).abs() < 1e-9);
        // cpu_total 5.0s over 5 words -> 1000 s / 1000 words.
        assert!((rec.cpu_s_per_1000_words - 1000.0).abs() < 1e-9);
        assert_eq!(rec.word_count, 5);
        assert!((rec.peak_rss_mb - 512.0).abs() < 1e-6);
    }

    #[test]
    fn zero_audio_does_not_divide_by_zero() {
        let rec = BenchRecord::from_raw("x", "a b", Path::new("/nonexistent"), &raw(1.0, 1.0, 0.0), &contract(0.0));
        assert_eq!(rec.rtf_wall, 0.0);
        assert_eq!(rec.compute_s_per_min_audio, 0.0);
    }

    #[test]
    fn empty_text_does_not_divide_by_zero() {
        let rec = BenchRecord::from_raw("x", "", Path::new("/nonexistent"), &raw(1.0, 1.0, 0.0), &contract(5.0));
        assert_eq!(rec.word_count, 0);
        assert_eq!(rec.cpu_s_per_1000_words, 0.0);
    }
}
