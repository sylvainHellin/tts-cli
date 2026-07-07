//! MiniMax t2a_v2 cloud backend.
//!
//! This is the original synthesis path, factored behind the [`Backend`] trait
//! with no behavior change: same payload, same key resolution, same mp3 bytes.

use crate::backend::{Backend, SynthOut};
use crate::{resolve_api_key, synthesize_chunk, AppError, Cli, TEXT_LIMIT};

/// MiniMax cloud backend. Resolves the API key lazily on first synth call so
/// `--dry-run` and non-MiniMax paths never touch pass-cli.
pub struct MiniMaxBackend<'a> {
    cli: &'a Cli,
    api_key: std::cell::RefCell<Option<String>>,
}

impl<'a> MiniMaxBackend<'a> {
    pub fn new(cli: &'a Cli) -> Self {
        MiniMaxBackend {
            cli,
            api_key: std::cell::RefCell::new(None),
        }
    }

    /// Resolve (and cache) the API key on first use.
    fn key(&self) -> Result<String, AppError> {
        if let Some(k) = self.api_key.borrow().as_ref() {
            return Ok(k.clone());
        }
        let k = resolve_api_key(self.cli)?;
        *self.api_key.borrow_mut() = Some(k.clone());
        Ok(k)
    }
}

impl Backend for MiniMaxBackend<'_> {
    fn text_limit(&self) -> usize {
        TEXT_LIMIT
    }

    fn synthesize(&self, text: &str) -> Result<SynthOut, AppError> {
        let key = self.key()?;
        let bytes = synthesize_chunk(self.cli, text, &key)?;
        Ok(SynthOut::Bytes(bytes))
    }
}
