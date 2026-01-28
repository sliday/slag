use super::{Ingot, Skill, Status};

/// Known field names that map to typed struct fields
const KNOWN_FIELDS: &[&str] = &[
    "id", "status", "solo", "grade", "skill", "heat", "max", "smelt", "proof", "work",
];

/// Parse a single s-expression line into an Ingot.
///
/// Format: `(ingot :key value :key "quoted value" ...)`
/// - Unquoted values end at space or `)`
/// - Quoted values are delimited by `"`
/// - Unknown fields are preserved in `extra`
pub fn parse_ingot(line: &str) -> Option<Ingot> {
    let line = line.trim();
    if !line.starts_with("(ingot ") {
        return None;
    }

    // Strip outer parens
    let inner = &line[7..line.len() - if line.ends_with(')') { 1 } else { 0 }];

    let fields = parse_fields(inner);

    let get = |key: &str| -> Option<String> {
        fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    };

    let id = get("id")?;
    let status = Status::parse(&get("status").unwrap_or_else(|| "ore".into()))?;
    let solo = get("solo").map(|s| s == "t").unwrap_or(true);
    let grade = get("grade").and_then(|s| s.parse().ok()).unwrap_or(1);
    let skill_str = get("skill").unwrap_or_else(|| "default".into());
    let skill = Skill::parse(&skill_str);
    let heat = get("heat").and_then(|s| s.parse().ok()).unwrap_or(0);
    let max = get("max").and_then(|s| s.parse().ok()).unwrap_or(5);
    let smelt = get("smelt").and_then(|s| s.parse().ok()).unwrap_or(0);
    let proof = get("proof").unwrap_or_else(|| "true".into());
    let work = get("work").unwrap_or_default();

    let extra: Vec<(String, String)> = fields
        .into_iter()
        .filter(|(k, _)| !KNOWN_FIELDS.contains(&k.as_str()))
        .collect();

    Some(Ingot {
        id,
        status,
        solo,
        grade,
        skill,
        heat,
        max,
        smelt,
        proof,
        work,
        extra,
    })
}

/// Parse `:key value` pairs from the inner content of an s-expression.
fn parse_fields(s: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Skip whitespace
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        // Expect ':'
        if chars[i] != ':' {
            i += 1;
            continue;
        }
        i += 1; // skip ':'

        // Read key
        let key_start = i;
        while i < len && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[key_start..i].iter().collect();

        // Skip whitespace before value
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len {
            fields.push((key, String::new()));
            break;
        }

        // Read value (quoted or unquoted)
        let value = if chars[i] == '"' {
            i += 1; // skip opening quote
            let val_start = i;
            while i < len && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < len {
                    i += 2; // skip escaped char
                } else {
                    i += 1;
                }
            }
            let val: String = chars[val_start..i].iter().collect();
            if i < len {
                i += 1; // skip closing quote
            }
            val
        } else {
            let val_start = i;
            while i < len && !chars[i].is_whitespace() && chars[i] != ')' {
                i += 1;
            }
            chars[val_start..i].iter().collect()
        };

        fields.push((key, value));
    }

    fields
}

/// Parse all ingot lines from a crucible file's content.
pub fn parse_crucible(content: &str) -> Vec<Ingot> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("(ingot ") {
                parse_ingot(trimmed)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_ingot() {
        let line = r#"(ingot :id "i1" :status ore :solo t :grade 2 :skill web :heat 0 :max 5 :smelt 0 :proof "test -f index.html" :work "Create HTML structure")"#;
        let ingot = parse_ingot(line).unwrap();
        assert_eq!(ingot.id, "i1");
        assert_eq!(ingot.status, Status::Ore);
        assert!(ingot.solo);
        assert_eq!(ingot.grade, 2);
        assert_eq!(ingot.skill, Skill::Web);
        assert_eq!(ingot.heat, 0);
        assert_eq!(ingot.max, 5);
        assert_eq!(ingot.smelt, 0);
        assert_eq!(ingot.proof, "test -f index.html");
        assert_eq!(ingot.work, "Create HTML structure");
    }

    #[test]
    fn parse_forged_ingot() {
        let line = r#"(ingot :id "i3" :status forged :solo nil :grade 3 :skill default :heat 2 :max 8 :proof "npm test" :work "Write tests")"#;
        let ingot = parse_ingot(line).unwrap();
        assert_eq!(ingot.id, "i3");
        assert_eq!(ingot.status, Status::Forged);
        assert!(!ingot.solo);
        assert_eq!(ingot.grade, 3);
        assert_eq!(ingot.heat, 2);
        assert_eq!(ingot.max, 8);
    }

    #[test]
    fn parse_preserves_unknown_fields() {
        let line = r#"(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "true" :work "test" :custom "hello")"#;
        let ingot = parse_ingot(line).unwrap();
        assert_eq!(ingot.extra.len(), 1);
        assert_eq!(ingot.extra[0], ("custom".to_string(), "hello".to_string()));
    }

    #[test]
    fn parse_missing_optional_fields_uses_defaults() {
        let line = r#"(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f file" :work "Do something")"#;
        let ingot = parse_ingot(line).unwrap();
        assert_eq!(ingot.smelt, 0);
        assert_eq!(ingot.skill, Skill::Default);
    }

    #[test]
    fn parse_non_ingot_returns_none() {
        assert!(parse_ingot(";; comment").is_none());
        assert!(parse_ingot("").is_none());
        assert!(parse_ingot("(not-ingot :id \"x\")").is_none());
    }

    #[test]
    fn parse_crucible_content() {
        let content = r#";; CRUCIBLE 2026-01-27
;; Blueprint: BLUEPRINT.md
(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "true" :work "First")
(ingot :id "i2" :status forged :solo nil :grade 2 :heat 3 :max 5 :proof "npm test" :work "Second")
"#;
        let ingots = parse_crucible(content);
        assert_eq!(ingots.len(), 2);
        assert_eq!(ingots[0].id, "i1");
        assert_eq!(ingots[1].id, "i2");
        assert_eq!(ingots[1].status, Status::Forged);
    }

    #[test]
    fn roundtrip_parse_write() {
        let line = r#"(ingot :id "i1" :status ore :solo t :grade 2 :skill web :heat 0 :max 5 :smelt 0 :proof "test -f index.html" :work "Create HTML structure")"#;
        let ingot = parse_ingot(line).unwrap();
        let written = super::super::writer::write_ingot(&ingot);
        let reparsed = parse_ingot(&written).unwrap();
        assert_eq!(ingot.id, reparsed.id);
        assert_eq!(ingot.status, reparsed.status);
        assert_eq!(ingot.solo, reparsed.solo);
        assert_eq!(ingot.grade, reparsed.grade);
        assert_eq!(ingot.skill, reparsed.skill);
        assert_eq!(ingot.heat, reparsed.heat);
        assert_eq!(ingot.max, reparsed.max);
        assert_eq!(ingot.proof, reparsed.proof);
        assert_eq!(ingot.work, reparsed.work);
    }

    #[test]
    fn parse_real_crucible_from_bash() {
        // This is the actual PLAN.md format produced by bash slag.sh
        let content = r#";; CRUCIBLE Tue Jan 27 10:13:45 CET 2026
;; Blueprint: BLUEPRINT.md
(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f slag/wrangler.toml" :work "Verify wrangler config exists")
(ingot :id "i2" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f slag/index.html" :work "Verify HTML entry point exists")
(ingot :id "i6" :status ore :solo nil :grade 3 :heat 0 :max 8 :proof "curl -s https://slag.dev | grep -q 'slag orchestrator'" :work "Deploy to Cloudflare Pages and verify live")
"#;
        let ingots = parse_crucible(content);
        assert_eq!(ingots.len(), 3);
        assert_eq!(ingots[0].proof, "test -f slag/wrangler.toml");
        assert_eq!(ingots[2].grade, 3);
        assert_eq!(ingots[2].max, 8);
        assert!(!ingots[2].solo);
    }
}
