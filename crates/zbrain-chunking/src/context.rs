use std::collections::HashMap;

use tree_sitter::Parser;

use crate::LanguageId;

/// Holds a reusable tree-sitter Parser and cached language grammars.
///
/// Only one language can be active on the parser at a time; switching calls
/// `parser.set_language()` internally.
pub struct ChunkingContext {
    pub parser: Parser,
    /// Cache of loaded tree-sitter Language references so we don't re-load
    /// the same grammar on every `set_language` call.
    language_cache: HashMap<LanguageId, tree_sitter::Language>,
}

impl ChunkingContext {
    pub fn new() -> Self {
        let parser = Parser::new();
        // tree-sitter 0.26.x: Parser::new() handles internal setup automatically.
        // No explicit init call needed (unlike WASM web-tree-sitter).
        let _ = parser; // silence unused warning until first use
        Self {
            parser,
            language_cache: HashMap::new(),
        }
    }

    /// Load and cache a language grammar, then set it on the parser.
    ///
    /// Returns an error if the language feature flag is not enabled.
    pub fn set_language(&mut self, lang: LanguageId) -> Result<(), ChunkingError> {
        if let Some(ts_lang) = self.language_cache.get(&lang) {
            self.parser.set_language(ts_lang).map_err(|e| {
                ChunkingError::ParserError(format!("set_language failed: {e}"))
            })?;
            return Ok(());
        }
        let ts_lang = load_language(lang)?;
        self.parser
            .set_language(&ts_lang)
            .map_err(|e| ChunkingError::ParserError(format!("set_language failed: {e}")))?;
        self.language_cache.insert(lang, ts_lang);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ChunkingError {
    #[error("unsupported language: {0:?}")]
    UnsupportedLanguage(LanguageId),
    #[error("language feature not enabled: {0:?}")]
    FeatureNotEnabled(LanguageId),
    #[error("parser error: {0}")]
    ParserError(String),
    #[error("unknown language for path: {0}")]
    UnknownLanguage(String),
}

/// Load a tree-sitter grammar for the given language.
///
/// Each arm is conditionally compiled behind the corresponding feature flag.
fn load_language(lang: LanguageId) -> Result<tree_sitter::Language, ChunkingError> {
    match lang {
        #[cfg(feature = "ts")]
        LanguageId::TypeScript => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        #[cfg(feature = "ts")]
        LanguageId::Tsx => Ok(tree_sitter_typescript::LANGUAGE_TSX.into()),
        #[cfg(feature = "ts")]
        LanguageId::JavaScript => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        #[cfg(feature = "python")]
        LanguageId::Python => Ok(tree_sitter_python::LANGUAGE.into()),
        #[cfg(feature = "go")]
        LanguageId::Go => Ok(tree_sitter_go::LANGUAGE.into()),
        #[cfg(feature = "rust")]
        LanguageId::Rust => Ok(tree_sitter_rust::LANGUAGE.into()),
        #[allow(unreachable_patterns)]
        _ => Err(ChunkingError::FeatureNotEnabled(lang)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_new_creates_parser() {
        let ctx = ChunkingContext::new();
        // Parser exists; no panic on construction.
        let _ = ctx;
    }

    #[cfg(feature = "ts")]
    #[test]
    fn set_language_typescript() {
        let mut ctx = ChunkingContext::new();
        ctx.set_language(LanguageId::TypeScript).unwrap();
    }

    #[cfg(feature = "python")]
    #[test]
    fn set_language_python() {
        let mut ctx = ChunkingContext::new();
        ctx.set_language(LanguageId::Python).unwrap();
    }

    #[cfg(feature = "go")]
    #[test]
    fn set_language_go() {
        let mut ctx = ChunkingContext::new();
        ctx.set_language(LanguageId::Go).unwrap();
    }

    #[cfg(feature = "rust")]
    #[test]
    fn set_language_rust() {
        let mut ctx = ChunkingContext::new();
        ctx.set_language(LanguageId::Rust).unwrap();
    }

    #[test]
    fn load_language_unsupported() {
        // Intentionally use a variant that exists but has no feature enabled
        // in this build configuration.
        let result = load_language(LanguageId::Rust);
        #[cfg(not(feature = "rust"))]
        assert!(matches!(result, Err(ChunkingError::FeatureNotEnabled(_))));
        #[cfg(feature = "rust")]
        assert!(result.is_ok());
    }
}
