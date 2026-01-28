use std::path::{Path, PathBuf};

use crate::sexp::parser::{parse_crucible, parse_ingot};
use crate::sexp::writer::write_ingot;
use crate::sexp::{Ingot, Status};

/// In-memory representation of PLAN.md (the crucible file).
/// All mutations happen here, then flush to disk.
#[derive(Debug)]
pub struct Crucible {
    pub path: PathBuf,
    /// Raw header lines (comments, metadata)
    header_lines: Vec<String>,
    pub ingots: Vec<Ingot>,
}

impl Crucible {
    /// Load crucible from a PLAN.md file
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        let mut header_lines = Vec::new();
        let mut ingots = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("(ingot ") {
                if let Some(ingot) = parse_ingot(trimmed) {
                    ingots.push(ingot);
                }
            } else {
                header_lines.push(line.to_string());
            }
        }

        Ok(Crucible {
            path: path.to_path_buf(),
            header_lines,
            ingots,
        })
    }

    /// Create a new crucible with header and ingots
    pub fn new(path: &Path, ingots: Vec<Ingot>) -> Self {
        let header_lines = vec![
            format!(";; CRUCIBLE {}", chrono::Local::now().format("%Y-%m-%d %H:%M")),
            format!(";; Blueprint: {}", crate::config::BLUEPRINT),
        ];
        Crucible {
            path: path.to_path_buf(),
            header_lines,
            ingots,
        }
    }

    /// Save crucible to disk
    pub fn save(&self) -> Result<(), std::io::Error> {
        let mut content = String::new();
        for line in &self.header_lines {
            content.push_str(line);
            content.push('\n');
        }
        for ingot in &self.ingots {
            content.push_str(&write_ingot(ingot));
            content.push('\n');
        }
        std::fs::write(&self.path, content)
    }

    /// Async save (for use in tokio context)
    pub async fn save_async(&self) -> Result<(), std::io::Error> {
        let mut content = String::new();
        for line in &self.header_lines {
            content.push_str(line);
            content.push('\n');
        }
        for ingot in &self.ingots {
            content.push_str(&write_ingot(ingot));
            content.push('\n');
        }
        tokio::fs::write(&self.path, content).await
    }

    /// Find ingot by id
    pub fn get(&self, id: &str) -> Option<&Ingot> {
        self.ingots.iter().find(|i| i.id == id)
    }

    /// Find mutable ingot by id
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Ingot> {
        self.ingots.iter_mut().find(|i| i.id == id)
    }

    /// Set status for an ingot by id
    pub fn set_status(&mut self, id: &str, status: Status) {
        if let Some(ingot) = self.get_mut(id) {
            ingot.status = status;
        }
    }

    /// Increment heat for an ingot
    pub fn increment_heat(&mut self, id: &str) {
        if let Some(ingot) = self.get_mut(id) {
            ingot.heat += 1;
        }
    }

    /// Get next ore ingot (any)
    pub fn next_ore(&self) -> Option<&Ingot> {
        self.ingots.iter().find(|i| i.status == Status::Ore)
    }

    /// Get all solo ore ingots (can run in parallel)
    pub fn solo_ore(&self) -> Vec<&Ingot> {
        self.ingots
            .iter()
            .filter(|i| i.status == Status::Ore && i.solo)
            .collect()
    }

    /// Get sequential ore ingots (solo=nil)
    pub fn sequential_ore(&self) -> Option<&Ingot> {
        self.ingots
            .iter()
            .find(|i| i.status == Status::Ore && !i.solo)
    }

    /// Replace ingot(s) by id. If replacement is multiple ingots (split), all are inserted.
    pub fn replace(&mut self, id: &str, replacements: Vec<Ingot>) {
        if let Some(idx) = self.ingots.iter().position(|i| i.id == id) {
            self.ingots.remove(idx);
            for (offset, ingot) in replacements.into_iter().enumerate() {
                self.ingots.insert(idx + offset, ingot);
            }
        }
    }

    /// Count ingots by status
    pub fn counts(&self) -> CrucibleCounts {
        let mut counts = CrucibleCounts::default();
        for ingot in &self.ingots {
            match ingot.status {
                Status::Ore => counts.ore += 1,
                Status::Molten => counts.molten += 1,
                Status::Forged => counts.forged += 1,
                Status::Cracked => counts.cracked += 1,
            }
        }
        counts.total = self.ingots.len();
        counts
    }

    /// Check if any ore or molten ingots remain
    pub fn has_pending(&self) -> bool {
        self.ingots
            .iter()
            .any(|i| i.status == Status::Ore || i.status == Status::Molten)
    }
}

#[derive(Debug, Default)]
pub struct CrucibleCounts {
    pub total: usize,
    pub ore: usize,
    pub molten: usize,
    pub forged: usize,
    pub cracked: usize,
}

impl CrucibleCounts {
    pub fn pct_forged(&self) -> u8 {
        if self.total == 0 {
            0
        } else {
            (self.forged * 100 / self.total) as u8
        }
    }
}

/// Parse ingot lines from raw founder output
pub fn parse_ingot_lines(raw: &str) -> Vec<Ingot> {
    parse_crucible(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sample_crucible() -> String {
        r#";; CRUCIBLE 2026-01-27
;; Blueprint: BLUEPRINT.md
(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f file" :work "First task")
(ingot :id "i2" :status forged :solo nil :grade 2 :heat 3 :max 5 :proof "npm test" :work "Second task")
(ingot :id "i3" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "true" :work "Third task")
(ingot :id "i4" :status cracked :solo nil :grade 3 :heat 5 :max 5 :proof "curl -s url" :work "Fourth task")
"#
        .into()
    }

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn load_crucible() {
        let f = write_temp(&sample_crucible());
        let c = Crucible::load(f.path()).unwrap();
        assert_eq!(c.ingots.len(), 4);
        assert_eq!(c.header_lines.len(), 2);
    }

    #[test]
    fn counts() {
        let f = write_temp(&sample_crucible());
        let c = Crucible::load(f.path()).unwrap();
        let counts = c.counts();
        assert_eq!(counts.total, 4);
        assert_eq!(counts.ore, 2);
        assert_eq!(counts.forged, 1);
        assert_eq!(counts.cracked, 1);
        assert_eq!(counts.molten, 0);
    }

    #[test]
    fn set_status_and_save() {
        let f = write_temp(&sample_crucible());
        let mut c = Crucible::load(f.path()).unwrap();
        c.set_status("i1", Status::Molten);
        c.save().unwrap();

        let c2 = Crucible::load(f.path()).unwrap();
        assert_eq!(c2.get("i1").unwrap().status, Status::Molten);
    }

    #[test]
    fn increment_heat() {
        let f = write_temp(&sample_crucible());
        let mut c = Crucible::load(f.path()).unwrap();
        assert_eq!(c.get("i1").unwrap().heat, 0);
        c.increment_heat("i1");
        assert_eq!(c.get("i1").unwrap().heat, 1);
        c.increment_heat("i1");
        assert_eq!(c.get("i1").unwrap().heat, 2);
    }

    #[test]
    fn solo_ore() {
        let f = write_temp(&sample_crucible());
        let c = Crucible::load(f.path()).unwrap();
        let solo = c.solo_ore();
        assert_eq!(solo.len(), 2); // i1 and i3
        assert!(solo.iter().all(|i| i.solo));
    }

    #[test]
    fn sequential_ore() {
        let f = write_temp(&sample_crucible());
        let c = Crucible::load(f.path()).unwrap();
        // No sequential ore in sample (i2 is forged, i4 is cracked)
        assert!(c.sequential_ore().is_none());
    }

    #[test]
    fn replace_single() {
        let f = write_temp(&sample_crucible());
        let mut c = Crucible::load(f.path()).unwrap();
        let new_ingot = Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: crate::sexp::Skill::Default,
            heat: 0,
            max: 5,
            smelt: 1,
            proof: "test -f newfile".into(),
            work: "Rewritten task".into(),
            extra: vec![],
        };
        c.replace("i1", vec![new_ingot]);
        assert_eq!(c.ingots.len(), 4);
        assert_eq!(c.get("i1").unwrap().work, "Rewritten task");
        assert_eq!(c.get("i1").unwrap().smelt, 1);
    }

    #[test]
    fn replace_split() {
        let f = write_temp(&sample_crucible());
        let mut c = Crucible::load(f.path()).unwrap();
        let sub_a = Ingot {
            id: "i1a".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: crate::sexp::Skill::Default,
            heat: 0,
            max: 5,
            smelt: 1,
            proof: "true".into(),
            work: "Sub-task A".into(),
            extra: vec![],
        };
        let sub_b = Ingot {
            id: "i1b".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: crate::sexp::Skill::Default,
            heat: 0,
            max: 5,
            smelt: 1,
            proof: "true".into(),
            work: "Sub-task B".into(),
            extra: vec![],
        };
        c.replace("i1", vec![sub_a, sub_b]);
        assert_eq!(c.ingots.len(), 5);
        assert!(c.get("i1").is_none());
        assert!(c.get("i1a").is_some());
        assert!(c.get("i1b").is_some());
    }

    #[test]
    fn has_pending() {
        let f = write_temp(&sample_crucible());
        let mut c = Crucible::load(f.path()).unwrap();
        assert!(c.has_pending());

        // Mark all ore as forged
        c.set_status("i1", Status::Forged);
        c.set_status("i3", Status::Forged);
        assert!(!c.has_pending());
    }

    #[test]
    fn pct_forged() {
        let f = write_temp(&sample_crucible());
        let c = Crucible::load(f.path()).unwrap();
        assert_eq!(c.counts().pct_forged(), 25); // 1 out of 4
    }
}
