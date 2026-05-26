//! `zbrain-mcp` — MCP server. Slice 1 is a placeholder.

/// Static crate name.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-mcp"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_zbrain_mcp() {
        assert_eq!(crate_name(), "zbrain-mcp");
    }
}
