//! skillify/templates — template strings for `zbrain skillify scaffold`.
//!
//! Pure-string generators. No I/O here; the caller (`generator.rs`) writes
//! the files. Ported verbatim from `src/core/skillify/templates.ts`.

/// SKILLIFY_STUB sentinel (D-CX-9). Every scaffolded script body carries
/// this marker until an implementer replaces it. `zbrain check-resolvable
/// --strict` fails if the sentinel is present in any committed skill
/// script — it means a scaffold shipped without a real implementation.
pub const SKILLIFY_STUB_MARKER: &str =
    "SKILLIFY_STUB: replace before running check-resolvable --strict";

/// Variables used to fill the scaffold templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldVars {
    /// Skill slug — must be lowercase-kebab-case.
    pub name: String,
    /// One-line description for the frontmatter.
    pub description: String,
    /// List of trigger phrases; empty → seed a TBD placeholder.
    pub triggers: Vec<String>,
    /// Directories this skill will write brain pages to; optional.
    pub writes_to: Vec<String>,
    /// Whether to mark the skill as `writes_pages: true`.
    pub writes_pages: bool,
    /// Whether to mark the skill as `mutating: true`.
    pub mutating: bool,
}

/// Build the SKILL.md frontmatter + body scaffold.
pub fn skill_md_template(v: &ScaffoldVars) -> String {
    let trigger_lines = if !v.triggers.is_empty() {
        v.triggers
            .iter()
            .map(|t| format!("  - \"{}\"", t.replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "  - \"TBD-trigger — replace with phrases users actually type\"".to_string()
    };

    let writes_to_lines = if !v.writes_to.is_empty() {
        v.writes_to
            .iter()
            .map(|d| format!("  - {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push("---".to_string());
    lines.push(format!("name: {}", v.name));
    lines.push("version: 0.1.0".to_string());
    lines.push(format!("description: {}", v.description));
    lines.push("triggers:".to_string());
    lines.push(trigger_lines);
    if v.mutating {
        lines.push("mutating: true".to_string());
    }
    if v.writes_pages {
        lines.push("writes_pages: true".to_string());
        if !writes_to_lines.is_empty() {
            lines.push("writes_to:".to_string());
            lines.push(writes_to_lines);
        }
    }
    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(format!("# {}", v.name));
    lines.push(String::new());
    lines.push(v.description.clone());
    lines.push(String::new());
    // v0.36.x scaffold pre-insert (A3 + F10 from /plan-eng-review). New
    // skills inherit the canonical brain-first Convention callout by
    // default; authors of pure-infra skills can delete this line and add
    // `brain_first: exempt` to frontmatter instead.
    lines.push(
        "> **Convention:** see [conventions/brain-first.md](../conventions/brain-first.md) \
         for the lookup chain (search → query → get_page → external)."
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## The rule".to_string());
    lines.push(String::new());
    lines.push(format!("<!-- {} -->", SKILLIFY_STUB_MARKER));
    lines.push(
        "Replace this stub with the hard rule that prevents recurrence of the failure that triggered this skill.".to_string(),
    );
    lines.push(String::new());
    lines.push("## How to use".to_string());
    lines.push(String::new());
    lines.push(format!(
        "Run the deterministic script: `bun scripts/{}.mjs` (or whatever your harness prefix is).",
        v.name
    ));
    lines.push(String::new());
    // 11-item contract (T7=C in plans/radiant-napping-lerdorf.md): the new
    // Phase 3 cross-modal eval is informational. The scaffold tells the
    // implementer where the gate lives without forcing it as a blocker.
    lines.push("## Phase 3: Cross-modal eval (informational)".to_string());
    lines.push(String::new());
    lines.push(
        "Once the SKILL.md body and `scripts/{}.mjs` are real, run the cross-modal".to_string(),
    );
    lines.push(
        "eval gate against the SKILL.md output before locking behavior in tests:".to_string(),
    );
    lines.push(String::new());
    lines.push("```bash".to_string());
    lines.push("zbrain eval cross-modal \\".to_string());
    lines.push("  --task \"What this skill is supposed to accomplish\" \\".to_string());
    lines.push(format!("  --output skills/{}/SKILL.md", v.name));
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push(
        "Three frontier models (different providers) score the output on 5 dimensions.".to_string(),
    );
    lines.push(
        "Pass criteria: every dim mean >=7 AND no model scored any dim <5. Receipts".to_string(),
    );
    lines.push(
        "land at `~/.zbrain/eval-receipts/<slug>-<sha8>.json` (sha-8 of SKILL.md".to_string(),
    );
    lines.push(
        "content). `zbrain skillify check` surfaces the receipt status as informational.".to_string(),
    );
    lines.push("See `skills/skillify/SKILL.md` Phase 3 for the full 11-item checklist.".to_string());

    lines.join("\n") + "\n"
}

/// Build the deterministic script stub. Carries the SKILLIFY_STUB_MARKER in
/// a comment — that is what `check-resolvable --strict` looks for.
pub fn script_template(v: &ScaffoldVars) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("#!/usr/bin/env bun".to_string());
    lines.push(format!("// {} — scaffolded by zbrain skillify scaffold", v.name));
    lines.push(format!("// {}", SKILLIFY_STUB_MARKER));
    lines.push("//".to_string());
    lines.push("// Replace this stub with the deterministic logic the skill needs.".to_string());
    lines.push("// Keep exports pure so tests can import them without side effects.".to_string());
    lines.push(String::new());
    lines.push("export function run(input: unknown): unknown {".to_string());
    lines.push("  // TODO: implement. This stub is detected by `zbrain check-resolvable".to_string());
    lines.push("  // --strict` and will fail CI until replaced.".to_string());
    lines.push(format!("  throw new Error('{} scaffold not yet implemented');", v.name));
    lines.push("}".to_string());
    lines.push(String::new());
    lines.push("if (import.meta.main) {".to_string());
    lines.push("  const input = process.argv.slice(2).join(' ');".to_string());
    lines.push("  console.log(JSON.stringify(run(input)));".to_string());
    lines.push("}".to_string());
    lines.join("\n") + "\n"
}

/// Build the unit-test stub.
pub fn test_template(v: &ScaffoldVars) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("/**".to_string());
    lines.push(format!(" * Tests for skills/{}/scripts/{}.mjs", v.name, v.name));
    lines.push(" *".to_string());
    lines.push(" * Scaffolded by zbrain skillify scaffold. Replace these stubs with".to_string());
    lines.push(" * real cases — start with the regression case for the failure that".to_string());
    lines.push(" * triggered this skill (essay Step 3).".to_string());
    lines.push(" */".to_string());
    lines.push(String::new());
    lines.push("import { describe, expect, it } from 'bun:test';".to_string());
    lines.push(format!(
        "import {{ run }} from '../skills/{name}/scripts/{name}.mjs';",
        name = v.name
    ));
    lines.push(String::new());
    lines.push(format!("describe('{name}', () => {{", name = v.name));
    lines.push(
        "  it('is scaffolded — replace this test with a real regression case', () => {".to_string(),
    );
    lines.push("    expect(() => run(null)).toThrow();".to_string());
    lines.push("  });".to_string());
    lines.push("});".to_string());
    lines.join("\n") + "\n"
}

/// A single resolver table row for this skill, under `## Uncategorized`.
/// The scaffolder handles the idempotency contract (D-CX-7): never
/// re-append a row that already exists.
pub fn resolver_row(v: &ScaffoldVars) -> String {
    let trigger = if !v.triggers.is_empty() {
        v.triggers[0].replace('"', "\\\"")
    } else {
        format!("TBD-trigger for {}", v.name)
    };
    format!("| \"{}\" | `skills/{}/SKILL.md` |", trigger, v.name)
}

/// Build the routing-eval fixture seed (`routing-eval.jsonl`).
pub fn routing_eval_template(v: &ScaffoldVars) -> String {
    if v.triggers.is_empty() {
        return format!(
            "// Routing eval fixtures for skills/{}.\n\
             // Add paraphrased intents.\n\
             // Each line: {{\"intent\": \"...\", \"expected_skill\": \"{}\"}}\n",
            v.name, v.name
        );
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("// Routing eval fixtures for skills/{}.", v.name));
    for t in v.triggers.iter().take(3) {
        let paraphrase = format!("please {} for me now", t.to_lowercase());
        let json = serde_json::to_string(&serde_json::json!({
            "intent": paraphrase,
            "expected_skill": v.name
        }))
        .unwrap_or_else(|_| {
            format!(
                "{{\"intent\": \"{}\", \"expected_skill\": \"{}\"}}",
                paraphrase, v.name
            )
        });
        lines.push(json);
    }
    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> ScaffoldVars {
        ScaffoldVars {
            name: name.to_string(),
            description: "demo".to_string(),
            triggers: vec![],
            writes_to: vec![],
            writes_pages: false,
            mutating: false,
        }
    }

    #[test]
    fn marker_present_in_skill_md_and_script() {
        let md = skill_md_template(&vars("foo"));
        assert!(md.contains(SKILLIFY_STUB_MARKER));
        let script = script_template(&vars("foo"));
        assert!(script.contains(SKILLIFY_STUB_MARKER));
    }

    #[test]
    fn tbd_trigger_seeded_when_empty() {
        let md = skill_md_template(&vars("empty-triggers"));
        assert!(md.contains("TBD-trigger"));
    }

    #[test]
    fn phase_three_section_present() {
        let md = skill_md_template(&vars("phase-three-demo"));
        assert!(md.contains("## Phase 3: Cross-modal eval"));
        assert!(md.contains("zbrain eval cross-modal"));
        assert!(md.contains("skills/phase-three-demo/SKILL.md"));
        assert!(md.contains("eval-receipts"));
        assert!(md.contains("<sha8>"));
    }

    #[test]
    fn writes_pages_flows_through() {
        let mut v = vars("writer");
        v.triggers = vec!["write me".to_string()];
        v.writes_to = vec!["people/".to_string(), "companies/".to_string()];
        v.writes_pages = true;
        v.mutating = true;
        let md = skill_md_template(&v);
        assert!(md.contains("writes_pages: true"));
        assert!(md.contains("- people/"));
        assert!(md.contains("- companies/"));
        assert!(md.contains("mutating: true"));
    }

    #[test]
    fn resolver_row_uses_backtick_path() {
        let mut v = vars("hello-world");
        v.triggers = vec!["say hello".to_string()];
        assert!(resolver_row(&v).contains("`skills/hello-world/SKILL.md`"));
    }
}
