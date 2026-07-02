pub mod manifest;
pub mod context;
pub mod chunk;

/// Language identifier for code detection and chunking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Rust,
}

/// Detect the source code language from a file path extension.
///
/// Returns `None` for unrecognized file extensions.
pub fn detect_code_language(file_path: &str) -> Option<LanguageId> {
    let ext = file_path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "ts" => Some(LanguageId::TypeScript),
        "tsx" => Some(LanguageId::Tsx),
        "js" | "mjs" | "cjs" => Some(LanguageId::JavaScript),
        "py" => Some(LanguageId::Python),
        "go" => Some(LanguageId::Go),
        "rs" => Some(LanguageId::Rust),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_typescript_by_ts_extension() {
        assert_eq!(
            detect_code_language("/src/foo.ts"),
            Some(LanguageId::TypeScript)
        );
    }

    #[test]
    fn detects_tsx() {
        assert_eq!(
            detect_code_language("components/App.tsx"),
            Some(LanguageId::Tsx)
        );
    }

    #[test]
    fn detects_javascript() {
        assert_eq!(
            detect_code_language("lib/utils.js"),
            Some(LanguageId::JavaScript)
        );
    }

    #[test]
    fn detects_python() {
        assert_eq!(
            detect_code_language("src/main.py"),
            Some(LanguageId::Python)
        );
    }

    #[test]
    fn detects_go() {
        assert_eq!(
            detect_code_language("pkg/handler.go"),
            Some(LanguageId::Go)
        );
    }

    #[test]
    fn detects_rust() {
        assert_eq!(
            detect_code_language("src/lib.rs"),
            Some(LanguageId::Rust)
        );
    }

    #[test]
    fn returns_none_for_unknown_extension() {
        assert_eq!(detect_code_language("README.md"), None);
    }

    #[test]
    fn returns_none_for_no_extension() {
        assert_eq!(detect_code_language("Makefile"), None);
    }

    #[test]
    fn handles_mixed_case_extension() {
        assert_eq!(
            detect_code_language("src/App.TS"),
            Some(LanguageId::TypeScript)
        );
    }

    #[test]
    fn handles_multi_dot_filename() {
        assert_eq!(
            detect_code_language("src/foo.test.ts"),
            Some(LanguageId::TypeScript)
        );
    }

    #[test]
    fn handles_absolute_windows_path() {
        assert_eq!(
            detect_code_language("C:\\Users\\dev\\src\\main.rs"),
            Some(LanguageId::Rust)
        );
    }

    #[test]
    fn handles_absolute_unix_path() {
        assert_eq!(
            detect_code_language("/home/dev/project/main.go"),
            Some(LanguageId::Go)
        );
    }

    #[test]
    fn returns_none_for_empty_string() {
        assert_eq!(detect_code_language(""), None);
    }

    #[test]
    fn returns_none_for_dotfile_only() {
        assert_eq!(detect_code_language(".gitignore"), None);
    }
}
