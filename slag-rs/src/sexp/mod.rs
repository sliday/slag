pub mod parser;
pub mod writer;

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ore,
    Molten,
    Forged,
    Cracked,
}

impl Status {
    pub fn as_str(&self) -> &str {
        match self {
            Status::Ore => "ore",
            Status::Molten => "molten",
            Status::Forged => "forged",
            Status::Cracked => "cracked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ore" => Some(Status::Ore),
            "molten" => Some(Status::Molten),
            "forged" => Some(Status::Forged),
            "cracked" => Some(Status::Cracked),
            _ => None,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skill {
    Web,
    Api,
    Cli,
    Default,
}

impl Skill {
    pub fn as_str(&self) -> &str {
        match self {
            Skill::Web => "web",
            Skill::Api => "api",
            Skill::Cli => "cli",
            Skill::Default => "default",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "web" | "frontend" | "ui" | "css" | "html" => Skill::Web,
            "api" => Skill::Api,
            "cli" => Skill::Cli,
            _ => Skill::Default,
        }
    }
}

impl fmt::Display for Skill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Ingot {
    pub id: String,
    pub status: Status,
    pub solo: bool,
    pub grade: u8,
    pub skill: Skill,
    pub heat: u8,
    pub max: u8,
    pub smelt: u8,
    pub proof: String,
    /// The sub-goal's acceptance bar: what "done" means for THIS ingot,
    /// stated so a reviewer can inspect it. Distinct from `proof`, which is
    /// the cheap mechanical gate run every heat -- `test -f` proves a file
    /// appeared, never that the ingot's goal was met. Empty when the plan
    /// predates the field, in which case the ingot is judged by proof alone.
    pub bar: String,
    pub work: String,
    /// Twin-cast duel override: `:duel t` forces, `:duel nil` blocks,
    /// absent (None) defers to the configured Auto policy.
    pub duel: Option<bool>,
    /// Adaptive cast count override: `:casts 1|2|3` pins how many smiths
    /// forge this ingot; absent (None) defers to the config heuristics.
    pub casts: Option<u8>,
    /// Preserve unknown fields for forward compatibility
    pub extra: Vec<(String, String)>,
}

impl Ingot {
    pub fn is_complex(&self) -> bool {
        self.grade >= crate::config::HIGH_GRADE
    }

    pub fn is_web(&self) -> bool {
        self.skill == Skill::Web
    }
}
