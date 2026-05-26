//! `zbrain-web` — axum-based HTTP API. Slice 1 is a placeholder.

/// Static crate name.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-web"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_zbrain_web() {
        assert_eq!(crate_name(), "zbrain-web");
    }
}
