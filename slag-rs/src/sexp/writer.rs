use super::Ingot;

fn escape_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Serialize an Ingot back to its s-expression string representation.
pub fn write_ingot(ingot: &Ingot) -> String {
    let solo = if ingot.solo { "t" } else { "nil" };
    let mut s = format!(
        "(ingot :id \"{}\" :status {} :solo {} :grade {} :skill {} :heat {} :max {} :smelt {} :proof \"{}\" :work \"{}\"",
        escape_quoted(&ingot.id),
        ingot.status,
        solo,
        ingot.grade,
        ingot.skill,
        ingot.heat,
        ingot.max,
        ingot.smelt,
        escape_quoted(&ingot.proof),
        escape_quoted(&ingot.work),
    );

    // The sub-goal bar, when the plan carries one. Optional so a crucible
    // written before the field round-trips byte-for-byte.
    if !ingot.bar.is_empty() {
        s.push_str(&format!(" :bar \"{}\"", escape_quoted(&ingot.bar)));
    }

    // Optional duel override: absent stays absent (Auto policy).
    if let Some(duel) = ingot.duel {
        s.push_str(&format!(" :duel {}", if duel { "t" } else { "nil" }));
    }

    // Optional cast-count pin: absent stays absent (heuristics decide).
    if let Some(casts) = ingot.casts {
        s.push_str(&format!(" :casts {casts}"));
    }

    // Append unknown extra fields for forward compatibility
    for (key, value) in &ingot.extra {
        // Quote values that cannot remain a single unquoted token.
        if value.is_empty()
            || value
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, '"' | ')' | '\\'))
        {
            s.push_str(&format!(" :{key} \"{}\"", escape_quoted(value)));
        } else {
            s.push_str(&format!(" :{key} {value}"));
        }
    }

    s.push(')');
    s
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_bar_round_trips_and_its_absence_leaves_the_line_unchanged() {
        use crate::sexp::parser::parse_ingot;
        let with_bar = r#"(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :smelt 0 :proof "true" :bar "the CLI prints 14" :work "w")"#;
        let back = write_ingot(&parse_ingot(with_bar).expect("parses"));
        assert!(back.contains(r#":bar "the CLI prints 14""#), "{back}");

        // A crucible written before the field must not grow one.
        let without = r#"(ingot :id "i1" :status ore :solo t :grade 1 :skill default :heat 0 :max 5 :smelt 0 :proof "true" :work "w")"#;
        let back = write_ingot(&parse_ingot(without).expect("parses"));
        assert!(!back.contains(":bar"), "{back}");
    }

    use super::*;
    use crate::sexp::{Skill, Status};

    #[test]
    fn quoted_strings_round_trip() {
        use crate::sexp::parser::{parse_crucible, parse_ingot};
        let mut values = vec![String::new()];
        for _ in 0..3 {
            let previous = values.clone();
            for value in previous {
                for ch in ['a', '"', '\\', '\n', '\r', '\t', '$', ')', 'λ'] {
                    values.push(format!("{value}{ch}"));
                }
            }
        }
        values.extend([
            r#"python3 -c "import hello; print(hello.main())""#.into(),
            r#"grep 'a\.b\|c' file && printf '%s\n' "$HOME""#.into(),
            "printf 'first\nsecond'\ntrue".into(),
            r#"python3 -c "print('nested \\"quote\\"')""#.into(),
        ]);
        for value in values {
            let mut ingot = parse_ingot(r#"(ingot :id "i1" :proof "true")"#).unwrap();
            ingot.id = value.clone();
            ingot.proof = value.clone();
            ingot.work = value.clone();
            ingot.bar = value.clone();
            ingot.extra = vec![("custom".into(), value.clone())];
            let written = write_ingot(&ingot);
            for parsed in [
                parse_ingot(&written).unwrap(),
                parse_crucible(&written).remove(0),
            ] {
                assert_eq!(parsed.id, value, "{written:?}");
                assert_eq!(parsed.proof, value, "{written:?}");
                assert_eq!(parsed.work, value, "{written:?}");
                assert_eq!(parsed.bar, value, "{written:?}");
                assert_eq!(parsed.extra, ingot.extra, "{written:?}");
            }
        }
    }

    #[test]
    fn write_basic_ingot() {
        let ingot = Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 2,
            skill: Skill::Web,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "test -f index.html".into(),
            bar: String::new(),
            work: "Create HTML structure".into(),
            duel: None,
            casts: None,
            extra: vec![],
        };
        let s = write_ingot(&ingot);
        assert!(s.starts_with("(ingot "));
        assert!(s.ends_with(')'));
        assert!(s.contains(":id \"i1\""));
        assert!(s.contains(":status ore"));
        assert!(s.contains(":solo t"));
        assert!(s.contains(":grade 2"));
        assert!(s.contains(":skill web"));
    }

    #[test]
    fn write_sequential_ingot() {
        let ingot = Ingot {
            id: "i5".into(),
            status: Status::Cracked,
            solo: false,
            grade: 4,
            skill: Skill::Cli,
            heat: 6,
            max: 8,
            smelt: 1,
            proof: "npm test".into(),
            bar: String::new(),
            work: "Deploy app".into(),
            duel: None,
            casts: None,
            extra: vec![],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains(":solo nil"));
        assert!(s.contains(":status cracked"));
        assert!(s.contains(":smelt 1"));
    }

    #[test]
    fn write_casts_pin_only_when_set() {
        let mut ingot = Ingot {
            id: "i9".into(),
            status: Status::Ore,
            solo: true,
            grade: 3,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "true".into(),
            bar: String::new(),
            work: "design the API".into(),
            duel: None,
            casts: None,
            extra: vec![],
        };
        assert!(!write_ingot(&ingot).contains(":casts"));

        ingot.casts = Some(3);
        assert!(write_ingot(&ingot).contains(":casts 3"));
    }

    #[test]
    fn write_preserves_extra_fields() {
        let ingot = Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "true".into(),
            bar: String::new(),
            work: "test".into(),
            duel: None,
            casts: None,
            extra: vec![("custom".into(), "hello".into())],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains(":custom hello"));
    }
}
