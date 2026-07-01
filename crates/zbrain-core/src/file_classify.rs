//! File type classification.
//!
//! Detects code languages by file extension and image formats.
//! Ported from TS `importCodeFile` / `importImageFile` detection logic.
//!
//! Part of roadmap node 1-7-1-4: Content extraction.

// ─── Code language extension mapping (31 languages) ────────────────────

/// Map file extensions to code language identifiers.
/// Mirrors TS `detectCodeLanguage` language table.
static CODE_EXTENSIONS: &[(&str, &str)] = &[
    // JavaScript / TypeScript ecosystem
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("mts", "typescript"),
    ("cts", "typescript"),
    // Web
    ("html", "html"),
    ("htm", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("sass", "sass"),
    ("less", "less"),
    // Systems
    ("rs", "rust"),
    ("go", "go"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("cc", "cpp"),
    ("hpp", "cpp"),
    ("hxx", "cpp"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("swift", "swift"),
    // Scripting
    ("py", "python"),
    ("pyi", "python"),
    ("pyx", "python"),
    ("rb", "ruby"),
    ("php", "php"),
    ("pl", "perl"),
    ("pm", "perl"),
    ("lua", "lua"),
    ("r", "r"),
    // Shell / Config
    ("sh", "bash"),
    ("bash", "bash"),
    ("zsh", "bash"),
    ("fish", "fish"),
    ("ps1", "powershell"),
    ("psm1", "powershell"),
    ("psd1", "powershell"),
    // Data / Config
    ("json", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("xml", "xml"),
    ("svg", "xml"),
    // SQL
    ("sql", "sql"),
    // Markdown
    ("md", "markdown"),
    ("mdx", "markdown"),
    ("markdown", "markdown"),
    // Other
    ("dockerfile", "dockerfile"),
    ("makefile", "makefile"),
    ("cmake", "cmake"),
    ("graphql", "graphql"),
    ("gql", "graphql"),
    ("proto", "protobuf"),
];

// ─── Image format extension mapping (7 formats) ────────────────────────

/// Map file extensions to image format identifiers.
static IMAGE_EXTENSIONS: &[(&str, &str)] = &[
    ("png", "png"),
    ("jpg", "jpg"),
    ("jpeg", "jpg"),
    ("gif", "gif"),
    ("svg", "svg"),
    ("webp", "webp"),
    ("bmp", "bmp"),
];

// ─── FileType enum ─────────────────────────────────────────────────────

/// Classification result for a file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileType {
    /// Markdown file (`.md`, `.mdx`, `.markdown`).
    Markdown,
    /// Code file with detected language (e.g. `Code("rust")`).
    Code(String),
    /// Image file with detected format (e.g. `Image("png")`).
    Image(String),
    /// Unknown / unclassified file type.
    Unknown,
}

// ─── Detection functions ───────────────────────────────────────────────

/// Detect code language from file extension.
///
/// Returns `Some("language")` if the extension matches a known code language.
/// Returns `None` for unknown or non-code extensions.
pub fn detect_code_language(path: &str) -> Option<String> {
    let ext = file_extension(path)?;
    for &(e, lang) in CODE_EXTENSIONS {
        if e.eq_ignore_ascii_case(ext) {
            return Some(lang.to_string());
        }
    }
    None
}

/// Detect image format from file extension.
///
/// Returns `Some("format")` if the extension matches a known image format.
/// Returns `None` for unknown or non-image extensions.
pub fn detect_image_format(path: &str) -> Option<String> {
    let ext = file_extension(path)?;
    for &(e, fmt) in IMAGE_EXTENSIONS {
        if e.eq_ignore_ascii_case(ext) {
            return Some(fmt.to_string());
        }
    }
    None
}

/// Check if a path is a markdown file.
pub fn is_markdown_path(path: &str) -> bool {
    let ext = file_extension(path);
    match ext {
        Some(e) => {
            e.eq_ignore_ascii_case("md")
                || e.eq_ignore_ascii_case("mdx")
                || e.eq_ignore_ascii_case("markdown")
        }
        None => false,
    }
}

/// Classify a file by its path extension.
///
/// Priority: markdown > image > code > unknown.
pub fn classify_file(path: &str) -> FileType {
    if is_markdown_path(path) {
        return FileType::Markdown;
    }
    if let Some(fmt) = detect_image_format(path) {
        return FileType::Image(fmt);
    }
    if let Some(lang) = detect_code_language(path) {
        return FileType::Code(lang);
    }
    FileType::Unknown
}

/// Extract the file extension (without the dot) from a path.
/// Returns `None` if there is no extension.
fn file_extension(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    // Handle special filenames like "Dockerfile" (no dot extension)
    match name.rfind('.') {
        Some(pos) if pos > 0 && pos < name.len() - 1 => Some(&name[pos + 1..]),
        _ => {
            // Check for extensionless known filenames
            let lower = name.to_lowercase();
            if lower == "dockerfile" {
                Some("dockerfile")
            } else if lower == "makefile" {
                Some("makefile")
            } else {
                None
            }
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── file_extension ─────────────────────────────────────────────────

    #[test]
    fn ext_normal() {
        assert_eq!(file_extension("main.rs"), Some("rs"));
        assert_eq!(file_extension("lib.py"), Some("py"));
    }

    #[test]
    fn ext_multiple_dots() {
        assert_eq!(file_extension("archive.tar.gz"), Some("gz"));
        assert_eq!(file_extension("test.spec.ts"), Some("ts"));
    }

    #[test]
    fn ext_no_extension() {
        assert_eq!(file_extension("Makefile"), Some("makefile"));
        assert_eq!(file_extension("Dockerfile"), Some("dockerfile"));
        assert_eq!(file_extension("README"), None);
    }

    #[test]
    fn ext_dotfile() {
        // Leading dot files: ".gitignore" → extension is "gitignore"
        // But our implementation treats pos==0 as no extension
        assert_eq!(file_extension(".gitignore"), None);
    }

    #[test]
    fn ext_windows_path() {
        assert_eq!(file_extension("src\\main.rs"), Some("rs"));
    }

    // ── detect_code_language ───────────────────────────────────────────

    #[test]
    fn detect_rust() {
        assert_eq!(detect_code_language("main.rs"), Some("rust".into()));
    }

    #[test]
    fn detect_typescript() {
        assert_eq!(detect_code_language("app.ts"), Some("typescript".into()));
        assert_eq!(detect_code_language("component.tsx"), Some("typescript".into()));
    }

    #[test]
    fn detect_javascript() {
        assert_eq!(detect_code_language("app.js"), Some("javascript".into()));
        assert_eq!(detect_code_language("component.jsx"), Some("javascript".into()));
        assert_eq!(detect_code_language("module.mjs"), Some("javascript".into()));
        assert_eq!(detect_code_language("module.cjs"), Some("javascript".into()));
    }

    #[test]
    fn detect_python() {
        assert_eq!(detect_code_language("main.py"), Some("python".into()));
        assert_eq!(detect_code_language("stub.pyi"), Some("python".into()));
    }

    #[test]
    fn detect_go() {
        assert_eq!(detect_code_language("main.go"), Some("go".into()));
    }

    #[test]
    fn detect_cpp_variants() {
        assert_eq!(detect_code_language("main.cpp"), Some("cpp".into()));
        assert_eq!(detect_code_language("main.cc"), Some("cpp".into()));
        assert_eq!(detect_code_language("header.hpp"), Some("cpp".into()));
    }

    #[test]
    fn detect_c() {
        assert_eq!(detect_code_language("main.c"), Some("c".into()));
        assert_eq!(detect_code_language("header.h"), Some("c".into()));
    }

    #[test]
    fn detect_java() {
        assert_eq!(detect_code_language("Main.java"), Some("java".into()));
    }

    #[test]
    fn detect_kotlin() {
        assert_eq!(detect_code_language("Main.kt"), Some("kotlin".into()));
    }

    #[test]
    fn detect_swift() {
        assert_eq!(detect_code_language("main.swift"), Some("swift".into()));
    }

    #[test]
    fn detect_ruby() {
        assert_eq!(detect_code_language("main.rb"), Some("ruby".into()));
    }

    #[test]
    fn detect_shell() {
        assert_eq!(detect_code_language("script.sh"), Some("bash".into()));
        assert_eq!(detect_code_language("script.bash"), Some("bash".into()));
        assert_eq!(detect_code_language("script.zsh"), Some("bash".into()));
    }

    #[test]
    fn detect_powershell() {
        assert_eq!(detect_code_language("script.ps1"), Some("powershell".into()));
    }

    #[test]
    fn detect_json() {
        assert_eq!(detect_code_language("data.json"), Some("json".into()));
    }

    #[test]
    fn detect_yaml() {
        assert_eq!(detect_code_language("config.yaml"), Some("yaml".into()));
        assert_eq!(detect_code_language("config.yml"), Some("yaml".into()));
    }

    #[test]
    fn detect_toml() {
        assert_eq!(detect_code_language("Cargo.toml"), Some("toml".into()));
    }

    #[test]
    fn detect_xml() {
        assert_eq!(detect_code_language("data.xml"), Some("xml".into()));
    }

    #[test]
    fn detect_sql() {
        assert_eq!(detect_code_language("query.sql"), Some("sql".into()));
    }

    #[test]
    fn detect_graphql() {
        assert_eq!(detect_code_language("query.graphql"), Some("graphql".into()));
        assert_eq!(detect_code_language("query.gql"), Some("graphql".into()));
    }

    #[test]
    fn detect_protobuf() {
        assert_eq!(detect_code_language("schema.proto"), Some("protobuf".into()));
    }

    #[test]
    fn detect_dockerfile() {
        assert_eq!(detect_code_language("Dockerfile"), Some("dockerfile".into()));
    }

    #[test]
    fn detect_makefile() {
        assert_eq!(detect_code_language("Makefile"), Some("makefile".into()));
    }

    #[test]
    fn detect_markdown_as_code() {
        // Markdown IS in the code extensions table (for syntax highlighting purposes)
        assert_eq!(detect_code_language("readme.md"), Some("markdown".into()));
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(detect_code_language("unknown.xyz"), None);
        assert_eq!(detect_code_language("noextension"), None);
    }

    #[test]
    fn detect_case_insensitive() {
        assert_eq!(detect_code_language("Main.RS"), Some("rust".into()));
        assert_eq!(detect_code_language("App.PY"), Some("python".into()));
    }

    // ── detect_image_format ────────────────────────────────────────────

    #[test]
    fn detect_png() {
        assert_eq!(detect_image_format("image.png"), Some("png".into()));
    }

    #[test]
    fn detect_jpg() {
        assert_eq!(detect_image_format("photo.jpg"), Some("jpg".into()));
        assert_eq!(detect_image_format("photo.jpeg"), Some("jpg".into()));
    }

    #[test]
    fn detect_gif() {
        assert_eq!(detect_image_format("anim.gif"), Some("gif".into()));
    }

    #[test]
    fn detect_svg() {
        assert_eq!(detect_image_format("icon.svg"), Some("svg".into()));
    }

    #[test]
    fn detect_webp() {
        assert_eq!(detect_image_format("photo.webp"), Some("webp".into()));
    }

    #[test]
    fn detect_bmp() {
        assert_eq!(detect_image_format("image.bmp"), Some("bmp".into()));
    }

    #[test]
    fn detect_not_image() {
        assert_eq!(detect_image_format("readme.md"), None);
        assert_eq!(detect_image_format("main.rs"), None);
    }

    #[test]
    fn detect_image_case_insensitive() {
        assert_eq!(detect_image_format("Image.PNG"), Some("png".into()));
    }

    // ── is_markdown_path ───────────────────────────────────────────────

    #[test]
    fn is_markdown() {
        assert!(is_markdown_path("readme.md"));
        assert!(is_markdown_path("page.mdx"));
        assert!(is_markdown_path("doc.markdown"));
        assert!(is_markdown_path("Doc.MD")); // case insensitive
    }

    #[test]
    fn is_not_markdown() {
        assert!(!is_markdown_path("main.rs"));
        assert!(!is_markdown_path("image.png"));
        assert!(!is_markdown_path("noextension"));
    }

    // ── classify_file ──────────────────────────────────────────────────

    #[test]
    fn classify_markdown() {
        assert_eq!(classify_file("readme.md"), FileType::Markdown);
        assert_eq!(classify_file("page.mdx"), FileType::Markdown);
    }

    #[test]
    fn classify_image() {
        assert_eq!(classify_file("photo.png"), FileType::Image("png".into()));
        assert_eq!(classify_file("icon.svg"), FileType::Image("svg".into()));
    }

    #[test]
    fn classify_code() {
        assert_eq!(classify_file("main.rs"), FileType::Code("rust".into()));
        assert_eq!(classify_file("app.ts"), FileType::Code("typescript".into()));
    }

    #[test]
    fn classify_unknown() {
        assert_eq!(classify_file("data.bin"), FileType::Unknown);
        assert_eq!(classify_file("noext"), FileType::Unknown);
    }

    #[test]
    fn classify_markdown_wins_over_code() {
        // .md is both a markdown path and a code language (markdown)
        // classify_file should return Markdown, not Code
        assert_eq!(classify_file("readme.md"), FileType::Markdown);
    }

    #[test]
    fn classify_svg_is_image_not_xml_code() {
        // .svg is both an image format and an XML code language
        // classify_file should return Image (checked before code)
        assert_eq!(classify_file("icon.svg"), FileType::Image("svg".into()));
    }
}
