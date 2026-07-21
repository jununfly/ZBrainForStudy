//! Code Intel sink classification (mirrors TS `classifySink`).
//!
//! Used in recursive `code_flow` (callees direction) to tag terminal nodes
//! like `console.log`, `require`, `import` as special sink kinds.

/// Sink kind (terminal node classification for callees direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    Unknown,
    /// Console/log output (console.log/warn/error).
    Console,
    /// Network request (fetch, axios, http.request).
    Network,
    /// Import/require dependency.
    Import,
    /// DOM operation (document, querySelector, getElementById).
    Dom,
    /// Timer (setTimeout, setInterval).
    Timer,
    /// Math/stdlib pure function (no side effects).
    Pure,
    /// Event handler registration (addEventListener, on...).
    Event,
}

impl SinkKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SinkKind::Unknown => "unknown",
            SinkKind::Console => "console",
            SinkKind::Network => "network",
            SinkKind::Import => "import",
            SinkKind::Dom => "dom",
            SinkKind::Timer => "timer",
            SinkKind::Pure => "pure",
            SinkKind::Event => "event",
        }
    }
}

/// Classify a symbol by its qualified name into a [SinkKind].
/// Mirrors TS `classifySink` from `src/core/code-intel/sinks/index.ts`.
///
/// Returns `None` for unknown (classification skipped / no match).
pub fn classify_sink(symbol_qualified: &str, _language: &str) -> Option<SinkKind> {
    let sym = symbol_qualified;

    // Console sinks
    if sym.ends_with(".log")
        || sym.ends_with(".warn")
        || sym.ends_with(".error")
        || sym.ends_with(".info")
        || sym.ends_with(".debug")
        || sym.contains(".console.")
    {
        return Some(SinkKind::Console);
    }

    // Network sinks
    if sym.contains(".fetch")
        || sym.contains(".get")
        || sym.contains(".post")
        || sym.contains(".request")
        || sym.ends_with(".call")
        || sym.contains("axios")
        || sym.contains("http")
    {
        return Some(SinkKind::Network);
    }

    // Import/require
    if sym == "require" || sym.contains(".require") || sym.ends_with("import") {
        return Some(SinkKind::Import);
    }

    // DOM operations
    if sym.contains(".querySelector")
        || sym.contains(".getElementById")
        || sym.contains(".getElementsBy")
        || sym.contains(".querySelectorAll")
        || sym.contains(".addEventListener")
        || sym.contains(".createElement")
        || sym.starts_with("document.")
        || sym.starts_with("window.")
    {
        if sym.contains(".addEventListener") || sym.contains(".on") {
            return Some(SinkKind::Event);
        }
        return Some(SinkKind::Dom);
    }

    // Timers
    if sym == "setTimeout" || sym == "setInterval" || sym == "clearTimeout" {
        return Some(SinkKind::Timer);
    }

    // Pure math/stdlib
    if sym.contains(".Math.") || sym.contains(".parse") || sym.contains(".toString") {
        return Some(SinkKind::Pure);
    }

    None
}
