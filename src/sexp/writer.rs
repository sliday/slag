use super::Ingot;

/// Serialize an Ingot back to its s-expression string representation.
pub fn write_ingot(ingot: &Ingot) -> String {
    let solo = if ingot.solo { "t" } else { "nil" };
    let mut s = format!(
        "(ingot :id \"{}\" :status {} :solo {} :grade {} :skill {} :heat {} :max {} :smelt {} :proof \"{}\" :work \"{}\"",
        ingot.id,
        ingot.status,
        solo,
        ingot.grade,
        ingot.skill,
        ingot.heat,
        ingot.max,
        ingot.smelt,
        ingot.proof,
        ingot.work,
    );

    // Append unknown extra fields for forward compatibility
    for (key, value) in &ingot.extra {
        // If value looks like it needs quoting (contains spaces), quote it
        if value.contains(' ') || value.contains('"') {
            s.push_str(&format!(" :{key} \"{value}\""));
        } else {
            s.push_str(&format!(" :{key} {value}"));
        }
    }

    s.push(')');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::{Skill, Status};

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
            work: "Create HTML structure".into(),
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
            work: "Deploy app".into(),
            extra: vec![],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains(":solo nil"));
        assert!(s.contains(":status cracked"));
        assert!(s.contains(":smelt 1"));
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
            work: "test".into(),
            extra: vec![("custom".into(), "hello".into())],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains(":custom hello"));
    }
}
