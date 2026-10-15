//! echo — the pipeline's own stream lines, rebuilt from a tape.
//!
//! In a live stream-mode run the pipeline prints the forge's skeleton
//! itself: the founder's table, each ingot's header, every heat and its
//! proof, the assay. Those prints reach the terminal and nothing else, so
//! a tape cannot hold them. What a tape does hold is enough to rebuild
//! them: ingot lifecycle events, `tui` marks, and PLAN.md as it changed.
//!
//! `Echo` replays that skeleton. Marks go back through the same `tui`
//! function that printed them live. Lines the pipeline prints raw are
//! rebuilt here in the pipeline's format, from the same data. Two things
//! a tape cannot recover are approximated: the proof command a smith
//! chose (shown as the ingot's `:proof`) and why a heat failed (shown as
//! a bare ✗).

use std::time::Instant;

use crate::engine::EngineEvent;
use crate::pipeline::{bold, fg, reset};
use crate::render::tape::{Cue, Mark};
use crate::sexp::{Ingot, Status};
use crate::tui;

/// What the echo is waiting to print once the next cue fills it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    None,
    /// `Mold: …` printed; the founder's table follows once the plan lands.
    Table,
    /// `Blueprint: …` printed; the excerpt follows once the doc lands.
    Excerpt,
}

#[derive(Debug, Clone)]
struct OpenHeat {
    id: String,
    heat: u8,
}

pub struct Echo {
    plan: Vec<Ingot>,
    pending: Pending,
    doc: Option<String>,
    /// Ingots on an anvil right now, in start order.
    open: Vec<String>,
    /// Heats printed by the live pipeline only once they resolve.
    heats: Vec<OpenHeat>,
    after_header: bool,
    in_assay: bool,
    last_gap: Option<String>,
    tokens: u64,
    cost: Option<f64>,
}

impl Default for Echo {
    fn default() -> Self {
        Self::new()
    }
}

impl Echo {
    pub fn new() -> Self {
        Self {
            plan: Vec::new(),
            pending: Pending::None,
            doc: None,
            open: Vec::new(),
            heats: Vec::new(),
            after_header: false,
            in_assay: false,
            last_gap: None,
            tokens: 0,
            cost: None,
        }
    }

    /// Print whatever `cue` meant to the pipeline's stream.
    pub fn cue(&mut self, cue: &Cue, _at: Instant) {
        if !matches!(cue, Cue::Plan(_) | Cue::Doc { .. }) {
            self.flush_pending();
        }
        match cue {
            Cue::Plan(text) => {
                self.plan = crate::crucible::parse_ingot_lines(text);
                if self.pending == Pending::Table {
                    self.flush_pending();
                }
            }
            Cue::Doc { name, text } => {
                if name != crate::config::BLUEPRINT {
                    return;
                }
                self.doc = Some(text.clone());
                if self.pending == Pending::Excerpt {
                    self.flush_pending();
                }
            }
            Cue::Mark(m) => self.mark(m),
            Cue::Event(e) => self.event(e),
        }
    }

    /// End of tape: settle anything still waiting.
    pub fn finish(&mut self) {
        self.flush_pending();
        tui::flush();
    }

    fn mark(&mut self, m: &Mark) {
        let was_header = self.after_header;
        self.after_header = false;
        match m {
            Mark::Banner => tui::show_banner(),
            Mark::Header { text } => {
                self.in_assay = text == "ASSAY";
                tui::header(text);
                self.after_header = true;
                if self.in_assay {
                    self.assay_head();
                }
                if text == "GOAL NOT MET" {
                    if let Some(gap) = &self.last_gap {
                        println!("  {}{gap}{}\n", fg(tui::WARM), reset());
                        println!("  the tasks passed; the commission did not.");
                        println!(
                            "  raise --temper to forge the gap, or read {}.\n",
                            crate::config::LEDGER
                        );
                    }
                }
            }
            Mark::Status { icon, tone, text } => {
                tui::status_line(icon, tone.color(), text);
                if text.starts_with("Mold:") {
                    self.pending = Pending::Table;
                } else if text.starts_with("Blueprint:") {
                    self.pending = Pending::Excerpt;
                }
            }
            Mark::Counts { tally } => {
                print!("{}", if was_header { "  " } else { "\n  " });
                tui::ingot_status_line(&tally.counts());
                println!();
            }
            Mark::TemperBar { tally } => {
                tui::temper_bar(&tally.counts());
                if self.in_assay {
                    self.assay_tail();
                }
            }
        }
        tui::flush();
    }

    fn event(&mut self, e: &EngineEvent) {
        match e {
            EngineEvent::Tokens { usage } => {
                self.tokens += usage.total_tokens;
                if let Some(c) = usage.cost {
                    *self.cost.get_or_insert(0.0) += c;
                }
            }
            EngineEvent::Warning { message } if message.starts_with("warden round") => {
                self.last_gap = message.split_once(": ").map(|(_, gap)| gap.to_string());
            }
            EngineEvent::IngotStart { id, .. } => {
                if self.open.is_empty() {
                    self.anvils_line(id);
                }
                if !self.open.contains(id) {
                    self.open.push(id.clone());
                }
                self.ingot_head(id);
            }
            EngineEvent::HeatTick { id, heat } => {
                // A new heat means the last one on this ingot failed.
                self.resolve(id, false);
                self.heats.push(OpenHeat { id: id.clone(), heat: *heat });
            }
            EngineEvent::IngotDone { id, ok } => {
                self.resolve(id, *ok);
                self.open.retain(|o| o != id);
            }
            EngineEvent::DuelRound { id, round } => {
                println!(
                    "    {}⚔ [{id}] duel round {round}{}",
                    fg(tui::BRIGHT),
                    reset()
                );
            }
            _ => {}
        }
        tui::flush();
    }

    fn ingot(&self, id: &str) -> Option<&Ingot> {
        self.plan.iter().find(|i| i.id == id)
    }

    /// `⚒⚒⚒ 3 anvils: i1 i2 i3`, once per parallel batch. The batch is
    /// what the plan shows molten when its first ingot starts.
    fn anvils_line(&self, first: &str) {
        let solo = self.ingot(first).is_some_and(|i| i.solo);
        if !solo {
            return;
        }
        let mut ids: Vec<&str> = self
            .plan
            .iter()
            .filter(|i| i.solo && i.status == Status::Molten)
            .map(|i| i.id.as_str())
            .collect();
        if ids.is_empty() {
            ids.push(first);
        }
        println!(
            "\n  {}⚒{}⚒{}{}⚒{} {}{} anvils:{} {}{}{}{}",
            fg(tui::HOT),
            fg(tui::BRIGHT),
            bold(),
            fg(tui::PURE),
            reset(),
            fg(tui::COLD),
            ids.len(),
            reset(),
            bold(),
            fg(tui::PURE),
            ids.join(" "),
            reset(),
        );
    }

    fn ingot_head(&self, id: &str) {
        let Some(ingot) = self.ingot(id) else {
            println!("\n  {}▣{} {}{}[{id}]{}", fg(tui::HOT), reset(), bold(), fg(tui::PURE), reset());
            return;
        };
        println!(
            "\n  {}▣{} {}{}[{}]{} {}{}{}",
            fg(tui::HOT),
            reset(),
            bold(),
            fg(tui::PURE),
            ingot.id,
            reset(),
            tui::truncate(&ingot.work, 42),
            if ingot.is_complex() {
                format!(" {}◉{}", fg(tui::BRIGHT), reset())
            } else {
                String::new()
            },
            if ingot.is_web() {
                format!(" {}⚡{}", fg(tui::HOT), reset())
            } else {
                String::new()
            },
        );
        println!(
            "    {}gr:{} skill:{} proof:{}{}",
            fg(tui::COLD),
            ingot.grade,
            ingot.skill,
            tui::truncate(&ingot.proof, 30),
            reset(),
        );
    }

    /// Print a heat's line now that its outcome is known. Live, the line
    /// sits in stdout's buffer until the proof runs, so it reaches the
    /// screen whole, after the smith's cards: the same place it lands here.
    fn resolve(&mut self, id: &str, ok: bool) {
        let Some(at) = self.heats.iter().position(|h| h.id == id) else {
            return;
        };
        let heat = self.heats.remove(at);
        let (max, proof) = self
            .ingot(id)
            .map(|i| (i.max, i.proof.clone()))
            .unwrap_or((5, String::new()));
        let hc = match heat.heat {
            1..=2 => tui::WARM,
            3 => tui::HOT,
            4 => tui::BRIGHT,
            _ => tui::PURE,
        };
        let hb = if heat.heat > 4 { bold() } else { String::new() };
        let cmd = if proof.is_empty() {
            String::new()
        } else {
            format!("{}{}{} ", fg(tui::COLD), tui::truncate(&proof, 32), reset())
        };
        let verdict = if ok {
            format!("{}{}█{}", bold(), fg(tui::PURE), reset())
        } else {
            format!("{}✗{}", fg(tui::WARM), reset())
        };
        println!("    {hb}{}⚒ {}/{max}{} {cmd}{verdict}", fg(hc), heat.heat, reset());
    }

    fn flush_pending(&mut self) {
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => {}
            Pending::Table => self.founder_table(),
            Pending::Excerpt => self.excerpt(),
        }
    }

    fn founder_table(&self) {
        println!();
        println!(
            "  {}{:<5} {:<3} {:<4} {:<7} {}{}",
            fg(tui::COLD),
            "ID",
            "GR",
            "SOLO",
            "SKILL",
            "WORK",
            reset()
        );
        for ingot in self.plan.iter().take(10) {
            let solo_sym = if ingot.solo { "∥" } else { "→" };
            println!(
                "  {}{:<5}{} {:<3} {:<4} {:<7} {}",
                fg(tui::HOT),
                ingot.id,
                reset(),
                ingot.grade,
                solo_sym,
                ingot.skill.to_string(),
                tui::truncate(&ingot.work, 38),
            );
        }
        if self.plan.len() > 10 {
            println!("  {}+{} more{}", fg(tui::COLD), self.plan.len() - 10, reset());
        }
    }

    fn excerpt(&self) {
        let Some(doc) = &self.doc else { return };
        println!();
        let lines: Vec<&str> = doc.lines().collect();
        for line in lines.iter().take(20) {
            println!("  {}{line}{}", fg(tui::COLD), reset());
        }
        if lines.len() > 20 {
            println!("\n  {}... +{} lines{}", fg(tui::COLD), lines.len() - 20, reset());
        }
    }

    fn counts(&self) -> crate::crucible::CrucibleCounts {
        let mut c = crate::crucible::CrucibleCounts { total: self.plan.len(), ..Default::default() };
        for i in &self.plan {
            match i.status {
                Status::Ore => c.ore += 1,
                Status::Molten => c.molten += 1,
                Status::Forged => c.forged += 1,
                Status::Cracked => c.cracked += 1,
            }
        }
        c
    }

    fn assay_head(&self) {
        let counts = self.counts();
        print!(
            "  {}{}{}{} ingots  {}{}{}{} forged",
            bold(),
            fg(tui::PURE),
            counts.total,
            reset(),
            bold(),
            fg(tui::PURE),
            counts.forged,
            reset(),
        );
        if counts.cracked > 0 {
            print!("  {}{}{} cracked", fg(tui::WARM), counts.cracked, reset());
        }
        println!();
    }

    fn assay_tail(&mut self) {
        self.in_assay = false;
        let counts = self.counts();
        if let Some(cost) = self.cost {
            println!("\n  {}spend ${cost:.2}{}", fg(tui::COLD), reset());
        }
        if counts.cracked > 0 {
            println!("\n  {}Cracked:{}", fg(tui::WARM), reset());
            for ingot in self.plan.iter().filter(|i| i.status == Status::Cracked) {
                println!("    {}✗{} [{}] {}", fg(tui::WARM), reset(), ingot.id, ingot.work);
            }
        }
        println!("\n  {}blueprint: {}{}", fg(tui::COLD), crate::config::BLUEPRINT, reset());
        println!("  {}crucible:  {}{}", fg(tui::COLD), crate::config::CRUCIBLE, reset());
        println!("  {}slag heap: {}{}", fg(tui::COLD), crate::config::LOG_DIR, reset());
        if counts.cracked > 0 {
            println!("\n  {}{}✗ CRACKED{}\n", fg(tui::WARM), bold(), reset());
        } else {
            println!("\n  {}{}█ FORGED{}\n", bold(), fg(tui::PURE), reset());
        }
    }
}
