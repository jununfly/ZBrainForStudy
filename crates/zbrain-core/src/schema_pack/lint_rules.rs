//! Schema pack lint rules — file-plane validation + DB-aware checks.
//!
//! Ported from TS `src/core/schema-pack/lint-rules.ts`.
//!
//! 9 file-plane rules (pure functions, no engine needed):
//! 1. aliasShadowsType — alias name collides with a type name
//! 2. aliasDeclaredByTwoTypes — same alias declared by two types
//! 3. aliasReferencesUndeclaredType — alias points to non-existent type
//! 4. enrichableTypesUndeclared — enrichable type not in page_types
//! 5. linkTypesUndeclared — frontmatter_links references undeclared link type
//! 6. frontmatterLinksUndeclared — frontmatter_links.page_type not in page_types
//! 7. expertRoutingWithoutPrefix — expert_routing=true without path_prefixes
//! 8. prefixCollision — two types share the same prefix
//! 9. prefixStrictSubsetOverlap — one prefix is a strict subset of another

use super::manifest::SchemaPackManifest;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Error,
    Warning,
}

impl LintSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LintIssue {
    pub rule: String,
    pub severity: LintSeverity,
    pub message: String,
    pub pack: String,
    pub type_name: Option<String>,
    pub link: Option<String>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LintReport {
    pub ok: bool,
    pub errors: Vec<LintIssue>,
    pub warnings: Vec<LintIssue>,
}

impl LintReport {
    pub fn from_issues(issues: Vec<LintIssue>) -> Self {
        let errors: Vec<LintIssue> = issues.iter().filter(|i| i.severity == LintSeverity::Error).cloned().collect();
        let warnings: Vec<LintIssue> = issues.iter().filter(|i| i.severity == LintSeverity::Warning).cloned().collect();
        Self {
            ok: errors.is_empty(),
            errors,
            warnings,
        }
    }
}

// ---------------------------------------------------------------------------
// Lint rules (file-plane)
// ---------------------------------------------------------------------------

/// 1. aliasShadowsType — an alias name is the same as a declared type name.
pub fn lint_alias_shadows_type(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let type_names: std::collections::HashSet<&str> = m.page_types.iter().map(|pt| pt.name.as_str()).collect();
    let mut issues = Vec::new();
    for pt in &m.page_types {
        for alias in &pt.aliases {
            if type_names.contains(alias.as_str()) {
                issues.push(LintIssue {
                    rule: "aliasShadowsType".into(),
                    severity: LintSeverity::Warning,
                    message: format!("type \"{}\" declares alias \"{}\" which is also a type name", pt.name, alias),
                    pack: m.name.clone(),
                    type_name: Some(pt.name.clone()),
                    link: None,
                    hint: Some("remove the alias or rename the type".into()),
                });
            }
        }
    }
    issues
}

/// 2. aliasDeclaredByTwoTypes — the same alias is declared by two different types.
pub fn lint_alias_declared_by_two_types(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let mut alias_owners: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for pt in &m.page_types {
        for alias in &pt.aliases {
            alias_owners.entry(alias.as_str()).or_default().push(pt.name.as_str());
        }
    }
    let mut issues = Vec::new();
    for (alias, owners) in &alias_owners {
        if owners.len() > 1 {
            issues.push(LintIssue {
                rule: "aliasDeclaredByTwoTypes".into(),
                severity: LintSeverity::Error,
                message: format!(
                    "alias \"{}\" is declared by {} types: {}",
                    alias,
                    owners.len(),
                    owners.join(", ")
                ),
                pack: m.name.clone(),
                type_name: None,
                link: None,
                hint: Some("each alias should be declared by at most one type".into()),
            });
        }
    }
    issues
}

/// 3. aliasReferencesUndeclaredType — alias points to a type not in page_types.
pub fn lint_alias_references_undeclared_type(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let type_names: std::collections::HashSet<&str> = m.page_types.iter().map(|pt| pt.name.as_str()).collect();
    let mut issues = Vec::new();
    for pt in &m.page_types {
        for alias in &pt.aliases {
            if !type_names.contains(alias.as_str()) {
                issues.push(LintIssue {
                    rule: "aliasReferencesUndeclaredType".into(),
                    severity: LintSeverity::Warning,
                    message: format!(
                        "type \"{}\" declares alias \"{}\" which is not a declared type",
                        pt.name, alias
                    ),
                    pack: m.name.clone(),
                    type_name: Some(pt.name.clone()),
                    link: None,
                    hint: Some("add the aliased type or remove the alias".into()),
                });
            }
        }
    }
    issues
}

/// 4. enrichableTypesUndeclared — enrichable_type references undeclared type.
pub fn lint_enrichable_types_undeclared(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let type_names: std::collections::HashSet<&str> = m.page_types.iter().map(|pt| pt.name.as_str()).collect();
    let mut issues = Vec::new();
    for et in &m.enrichable_types {
        if !type_names.contains(et.type_name.as_str()) {
            issues.push(LintIssue {
                rule: "enrichableTypesUndeclared".into(),
                severity: LintSeverity::Error,
                message: format!(
                    "enrichable_type \"{}\" is not a declared page type",
                    et.type_name
                ),
                pack: m.name.clone(),
                type_name: Some(et.type_name.clone()),
                link: None,
                hint: None,
            });
        }
    }
    issues
}

/// 5. linkTypesUndeclared — frontmatter_links references undeclared link type.
pub fn lint_link_types_undeclared(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let link_type_names: std::collections::HashSet<&str> = m.link_types.iter().map(|lt| lt.name.as_str()).collect();
    let mut issues = Vec::new();
    for fl in &m.frontmatter_links {
        if !link_type_names.contains(fl.link_type.as_str()) {
            issues.push(LintIssue {
                rule: "linkTypesUndeclared".into(),
                severity: LintSeverity::Error,
                message: format!(
                    "frontmatter_links for page_type \"{}\" references undeclared link_type \"{}\"",
                    fl.page_type, fl.link_type
                ),
                pack: m.name.clone(),
                type_name: Some(fl.page_type.clone()),
                link: Some(fl.link_type.clone()),
                hint: None,
            });
        }
    }
    issues
}

/// 6. frontmatterLinksUndeclared — frontmatter_links.page_type not in page_types.
pub fn lint_frontmatter_links_undeclared(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let type_names: std::collections::HashSet<&str> = m.page_types.iter().map(|pt| pt.name.as_str()).collect();
    let mut issues = Vec::new();
    for fl in &m.frontmatter_links {
        if !type_names.contains(fl.page_type.as_str()) {
            issues.push(LintIssue {
                rule: "frontmatterLinksUndeclared".into(),
                severity: LintSeverity::Error,
                message: format!(
                    "frontmatter_links references undeclared page_type \"{}\"",
                    fl.page_type
                ),
                pack: m.name.clone(),
                type_name: Some(fl.page_type.clone()),
                link: None,
                hint: None,
            });
        }
    }
    issues
}

/// 7. expertRoutingWithoutPrefix — expert_routing=true but no path_prefixes.
pub fn lint_expert_routing_without_prefix(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    for pt in &m.page_types {
        if pt.expert_routing && pt.path_prefixes.is_empty() {
            issues.push(LintIssue {
                rule: "expertRoutingWithoutPrefix".into(),
                severity: LintSeverity::Warning,
                message: format!(
                    "type \"{}\" has expert_routing=true but no path_prefixes",
                    pt.name
                ),
                pack: m.name.clone(),
                type_name: Some(pt.name.clone()),
                link: None,
                hint: Some("add at least one path_prefix for routing".into()),
            });
        }
    }
    issues
}

/// 8. prefixCollision — two types share the exact same prefix.
pub fn lint_prefix_collision(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let mut prefix_owners: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for pt in &m.page_types {
        for prefix in &pt.path_prefixes {
            prefix_owners.entry(prefix.as_str()).or_default().push(pt.name.as_str());
        }
    }
    let mut issues = Vec::new();
    for (prefix, owners) in &prefix_owners {
        if owners.len() > 1 {
            issues.push(LintIssue {
                rule: "prefixCollision".into(),
                severity: LintSeverity::Error,
                message: format!(
                    "prefix \"{}\" is shared by {} types: {}",
                    prefix,
                    owners.len(),
                    owners.join(", ")
                ),
                pack: m.name.clone(),
                type_name: None,
                link: None,
                hint: Some("each prefix should map to exactly one type".into()),
            });
        }
    }
    issues
}

/// 9. prefixStrictSubsetOverlap — one prefix is a strict subset of another.
pub fn lint_prefix_strict_subset_overlap(m: &SchemaPackManifest) -> Vec<LintIssue> {
    let mut all_prefixes: Vec<(&str, &str)> = Vec::new(); // (prefix, type_name)
    for pt in &m.page_types {
        for prefix in &pt.path_prefixes {
            all_prefixes.push((prefix.as_str(), pt.name.as_str()));
        }
    }
    let mut issues = Vec::new();
    for i in 0..all_prefixes.len() {
        for j in 0..all_prefixes.len() {
            if i == j {
                continue;
            }
            let (p1, t1) = all_prefixes[i];
            let (p2, t2) = all_prefixes[j];
            // p1 is a strict subset of p2 (p2 starts with p1, but not equal)
            if p2.starts_with(p1) && p1 != p2 {
                issues.push(LintIssue {
                    rule: "prefixStrictSubsetOverlap".into(),
                    severity: LintSeverity::Warning,
                    message: format!(
                        "prefix \"{}\" (type \"{}\") is a prefix of \"{}\" (type \"{}\")",
                        p1, t1, p2, t2
                    ),
                    pack: m.name.clone(),
                    type_name: Some(t1.to_string()),
                    link: None,
                    hint: Some("first-match-wins routing may be ambiguous".into()),
                });
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// runAllLintRules / runFilePlaneLintRules
// ---------------------------------------------------------------------------

/// All file-plane lint rules (no engine needed).
pub const FILE_PLANE_RULE_NAMES: &[&str] = &[
    "aliasShadowsType",
    "aliasDeclaredByTwoTypes",
    "aliasReferencesUndeclaredType",
    "enrichableTypesUndeclared",
    "linkTypesUndeclared",
    "frontmatterLinksUndeclared",
    "expertRoutingWithoutPrefix",
    "prefixCollision",
    "prefixStrictSubsetOverlap",
];

/// Run all file-plane lint rules (no engine).
pub fn run_file_plane_lint_rules(manifest: &SchemaPackManifest) -> LintReport {
    let mut issues = Vec::new();
    issues.extend(lint_alias_shadows_type(manifest));
    issues.extend(lint_alias_declared_by_two_types(manifest));
    issues.extend(lint_alias_references_undeclared_type(manifest));
    issues.extend(lint_enrichable_types_undeclared(manifest));
    issues.extend(lint_link_types_undeclared(manifest));
    issues.extend(lint_frontmatter_links_undeclared(manifest));
    issues.extend(lint_expert_routing_without_prefix(manifest));
    issues.extend(lint_prefix_collision(manifest));
    issues.extend(lint_prefix_strict_subset_overlap(manifest));
    LintReport::from_issues(issues)
}

/// Run all lint rules (file-plane only in this port; DB-aware rules are TODO).
pub fn run_all_lint_rules(manifest: &SchemaPackManifest) -> LintReport {
    // DB-aware rules (extractableEmptyCorpus, mutationCountAnomaly) require
    // an engine and are not yet ported. File-plane rules are the core.
    run_file_plane_lint_rules(manifest)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_pack::manifest::{
        EnrichableType, FrontmatterLinkDefinition, LinkTypeDefinition, PageTypeDefinition,
        PackPrimitive, SchemaPackManifest,
    };

    fn make_manifest(page_types: Vec<PageTypeDefinition>) -> SchemaPackManifest {
        SchemaPackManifest {
            name: "test-pack".into(),
            version: "1.0.0".into(),
            page_types,
            ..Default::default()
        }
    }

    // ---- aliasShadowsType ------------------------------------------------

    #[test]
    fn alias_shadows_type_detected() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["person".into()], // shadows self
                ..Default::default()
            },
        ]);
        let issues = lint_alias_shadows_type(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule, "aliasShadowsType");
        assert_eq!(issues[0].severity, LintSeverity::Warning);
    }

    #[test]
    fn alias_shadows_type_clean() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["individual".into()], // not a type name
                ..Default::default()
            },
            PageTypeDefinition {
                name: "researcher".into(),
                primitive: PackPrimitive::Entity,
                ..Default::default()
            },
        ]);
        let issues = lint_alias_shadows_type(&m);
        assert!(issues.is_empty());
    }

    // ---- aliasDeclaredByTwoTypes ----------------------------------------

    #[test]
    fn alias_declared_by_two_types_detected() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["contact".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "company".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["contact".into()], // duplicate alias
                ..Default::default()
            },
        ]);
        let issues = lint_alias_declared_by_two_types(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Error);
    }

    // ---- aliasReferencesUndeclaredType ----------------------------------

    #[test]
    fn alias_references_undeclared_type_detected() {
        let m = make_manifest(vec![PageTypeDefinition {
            name: "person".into(),
            primitive: PackPrimitive::Entity,
            aliases: vec!["ghost".into()], // ghost not declared
            ..Default::default()
        }]);
        let issues = lint_alias_references_undeclared_type(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Warning);
    }

    // ---- enrichableTypesUndeclared --------------------------------------

    #[test]
    fn enrichable_types_undeclared_detected() {
        let m = SchemaPackManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            enrichable_types: vec![EnrichableType {
                type_name: "ghost".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let issues = lint_enrichable_types_undeclared(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Error);
    }

    // ---- linkTypesUndeclared --------------------------------------------

    #[test]
    fn link_types_undeclared_detected() {
        let m = SchemaPackManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            frontmatter_links: vec![FrontmatterLinkDefinition {
                page_type: "person".into(),
                fields: vec!["employer".into()],
                link_type: "works_at".into(), // not declared
            }],
            ..Default::default()
        };
        let issues = lint_link_types_undeclared(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Error);
    }

    // ---- frontmatterLinksUndeclared -------------------------------------

    #[test]
    fn frontmatter_links_undeclared_detected() {
        let m = SchemaPackManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            frontmatter_links: vec![FrontmatterLinkDefinition {
                page_type: "ghost".into(), // not declared
                fields: vec!["x".into()],
                link_type: "mentions".into(),
            }],
            link_types: vec![LinkTypeDefinition {
                name: "mentions".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let issues = lint_frontmatter_links_undeclared(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Error);
    }

    // ---- expertRoutingWithoutPrefix -------------------------------------

    #[test]
    fn expert_routing_without_prefix_detected() {
        let m = make_manifest(vec![PageTypeDefinition {
            name: "person".into(),
            primitive: PackPrimitive::Entity,
            expert_routing: true,
            ..Default::default()
        }]);
        let issues = lint_expert_routing_without_prefix(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Warning);
    }

    // ---- prefixCollision ------------------------------------------------

    #[test]
    fn prefix_collision_detected() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "researcher".into(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/".into()], // same prefix
                ..Default::default()
            },
        ]);
        let issues = lint_prefix_collision(&m);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, LintSeverity::Error);
    }

    // ---- prefixStrictSubsetOverlap --------------------------------------

    #[test]
    fn prefix_subset_overlap_detected() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "researcher".into(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/researchers/".into()], // superset
                ..Default::default()
            },
        ]);
        let issues = lint_prefix_strict_subset_overlap(&m);
        assert!(!issues.is_empty());
        assert_eq!(issues[0].severity, LintSeverity::Warning);
    }

    // ---- run_file_plane_lint_rules --------------------------------------

    #[test]
    fn clean_manifest_no_issues() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/".into()],
                extractable: true,
                ..Default::default()
            },
            PageTypeDefinition {
                name: "note".into(),
                primitive: PackPrimitive::Concept,
                path_prefixes: vec!["notes/".into()],
                ..Default::default()
            },
        ]);
        let report = run_file_plane_lint_rules(&m);
        assert!(report.ok);
        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn mixed_errors_and_warnings() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["contact".into()],
                path_prefixes: vec!["people/".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "company".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["contact".into()], // error: duplicate alias
                path_prefixes: vec!["people/".into()], // error: prefix collision
                ..Default::default()
            },
        ]);
        let report = run_file_plane_lint_rules(&m);
        assert!(!report.ok);
        assert!(!report.errors.is_empty());
    }

    #[test]
    fn rule_count_matches() {
        assert_eq!(FILE_PLANE_RULE_NAMES.len(), 9);
    }
}
