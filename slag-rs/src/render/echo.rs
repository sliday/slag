//! echo — the forge's skeleton in the stream view, built from cues.
//!
//! The stream view has two layers. The narrator (`engine::events`) tells
//! what the smiths do, one tool call per line. The echo tells where the
//! run stands: phase rules, the blueprint and the mold, each ingot as it
//! reaches the anvil, every heat that fails, every ingot that lands, and
//! the assay. It reads only cues (events, `tui` marks, plan snapshots,
//! proof outcomes), so a live run and its replay print the same lines.
//!
//! Every function here returns lines; `render::stream` decides where they
//! go. Lines never wrap: each is built to fit `width`.

use std::time::{Duration, Instant};

use crate::engine::{EngineEvent, Usage};
use crate::render::tape::{Cue, Mark, Proof, Stats};
use crate::sexp::{Ingot, Status};
use crate::tui::{self, dim, paint, BRIGHT, COLD, HOT, PURE, WARM};

/// Which part of the run is on screen. Drives the footer's verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Survey,
    Plan,
    Forge,
    Warden,
    Assay,
    /// A phase this view has no word for (a new flow's own header). The
    /// footer still runs, naming it by its header.
    Other,
}

impl Phase {
    fn of(header: &str) -> Self {
        let head = header.split(" · ").next().unwrap_or(header);
        match head {
            "SURVEYOR" => Phase::Survey,
            "FOUNDER" => Phase::Plan,
            "FORGE" => Phase::Forge,
            "WARDEN" => Phase::Warden,
            "ASSAY" | "GOAL NOT MET" => Phase::Assay,
            "FIRING FURNACE" => Phase::Idle,
            _ => Phase::Other,
        }
    }

    /// The footer's verb for this phase.
    pub fn verb(self) -> Option<&'static str> {
        match self {
            Phase::Survey => Some("surveying"),
            Phase::Plan => Some("casting the mold"),
            Phase::Forge => Some("forging"),
            Phase::Warden => Some("judging"),
            Phase::Idle | Phase::Assay | Phase::Other => None,
        }
    }
}

/// What the echo waits on before it can print.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    None,
    /// `Mold: …` printed; the table follows once the plan lands.
    Table,
    /// `N ingots added`; the new rows follow once the plan lands.
    Added(Vec<String>),
    /// `Blueprint: …` printed; the excerpt follows once the doc lands.
    Excerpt,
}

/// An ingot on the anvil.
#[derive(Debug, Clone)]
pub struct Anvil {
    pub id: String,
    pub heat: u8,
    pub since: Instant,
    /// The passing proof's command, held for the "forged" line.
    proof: Option<String>,
}

pub struct Echo {
    width: usize,
    plan: Vec<Ingot>,
    pending: Pending,
    /// The status text a pending excerpt or table heads with.
    blueprint_head: Option<String>,
    mold_head: Option<String>,
    blueprint: Option<String>,
    commission_shown: bool,
    phase: Phase,
    /// The last phase header, as printed.
    header: Option<String>,
    /// Ingots on an anvil now, in start order.
    anvils: Vec<Anvil>,
    /// Heats resolved by a proof cue, so the event that ends them does not
    /// print a second, vaguer line.
    resolved: Vec<(String, u8)>,
    started: Option<Instant>,
    retries: usize,
    stats: Option<Stats>,
    usage: Usage,
    goal_gap: Option<String>,
    goal_met: bool,
    assay_done: bool,
    /// A provider refusal that stopped the run. No assay follows it: there
    /// is no verdict to report on a run that could not call its model.
    fatal: Option<String>,
    /// The last outcome of each ingot's proof, for the assay's evidence.
    proofs: std::collections::BTreeMap<String, Proof>,
    /// The highest warden round that returned a verdict.
    warden_rounds: u32,
    /// Whether this echo prints that refusal itself. Live, `main` owns the
    /// line; a replay has no `main` to print it, so the echo does.
    owns_fatal: bool,
}

impl Echo {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            plan: Vec::new(),
            pending: Pending::None,
            blueprint_head: None,
            mold_head: None,
            blueprint: None,
            commission_shown: false,
            phase: Phase::Idle,
            header: None,
            anvils: Vec::new(),
            resolved: Vec::new(),
            started: None,
            retries: 0,
            stats: None,
            usage: Usage::default(),
            goal_gap: None,
            goal_met: false,
            assay_done: false,
            fatal: None,
            proofs: std::collections::BTreeMap::new(),
            warden_rounds: 0,
            owns_fatal: false,
        }
    }

    /// Print a run-stopping provider error at the end (replays).
    pub fn owning_fatal(mut self, on: bool) -> Self {
        self.owns_fatal = on;
        self
    }

    pub fn set_width(&mut self, width: usize) {
        self.width = width;
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The footer's verb: the phase's own word, or a new flow's header
    /// lowercased (`FIX · reproducing` → `fix`).
    pub fn phase_verb(&self) -> Option<String> {
        match self.phase {
            Phase::Other => self.header.as_deref().map(|h| {
                h.split(" · ").next().unwrap_or(h).to_lowercase()
            }),
            p => p.verb().map(str::to_string),
        }
    }

    pub fn anvils(&self) -> &[Anvil] {
        &self.anvils
    }

    pub fn started(&self) -> Option<Instant> {
        self.started
    }

    /// Run totals so far: tokens, and dollars when the provider said.
    pub fn spend(&self) -> &Usage {
        &self.usage
    }

    /// Max heats for an ingot, from the plan.
    pub fn max_heat(&self, id: &str) -> Option<u8> {
        self.ingot(id).map(|i| i.max)
    }

    /// The lines `cue` adds to the stream.
    pub fn cue(&mut self, cue: &Cue, at: Instant) -> Vec<String> {
        self.started.get_or_insert(at);
        let mut out = Vec::new();
        if !matches!(cue, Cue::Plan(_) | Cue::Doc { .. }) {
            self.flush_pending(&mut out);
        }
        match cue {
            Cue::Plan(text) => {
                let next = crate::crucible::parse_ingot_lines(text);
                if self.phase == Phase::Forge {
                    self.resmelts(&next, &mut out);
                }
                self.plan = next;
                if matches!(self.pending, Pending::Table | Pending::Added(_)) {
                    self.flush_pending(&mut out);
                }
            }
            Cue::Doc { name, text } => {
                if name == crate::config::BLUEPRINT {
                    self.blueprint = Some(text.clone());
                    if self.pending == Pending::Excerpt {
                        self.flush_pending(&mut out);
                    }
                } else if name == crate::config::ORE_FILE && !self.commission_shown {
                    self.commission_shown = true;
                    if let Some(line) = commission(text) {
                        out.push(self.labelled("◇", COLD, "commission", &line));
                    }
                }
            }
            Cue::Mark(m) => self.mark(m, at, &mut out),
            Cue::Event(e) => self.event(e, at, &mut out),
            Cue::Proof(p) => self.proof(p, &mut out),
            Cue::Stats(s) => self.stats = Some(s.clone()),
        }
        out
    }

    /// End of the run: the assay, unless a mark already printed it.
    pub fn finish(&mut self, at: Instant) -> Vec<String> {
        let mut out = Vec::new();
        self.flush_pending(&mut out);
        if let Some(message) = self.fatal.clone() {
            if self.owns_fatal {
                out.push(String::new());
                out.extend(wrap_labelled("✗", &message, self.width, 4).into_iter().map(|l| paint(WARM, &l)));
            }
            return out;
        }
        if !self.assay_done && !self.plan.is_empty() {
            self.assay(at, &mut out);
        }
        out
    }

    fn elapsed(&self, at: Instant) -> Duration {
        self.started.map_or(Duration::ZERO, |s| at.saturating_duration_since(s))
    }

    fn ingot(&self, id: &str) -> Option<&Ingot> {
        self.plan.iter().find(|i| i.id == id)
    }

    /// `  ◇ label  text`, with the label padded so a run of these aligns.
    fn labelled(&self, glyph: &str, color: crossterm::style::Color, label: &str, text: &str) -> String {
        let room = self.width.saturating_sub(6 + label.chars().count());
        format!("  {} {}  {}", paint(color, glyph), dim(label), tui::clip(text, room))
    }

    fn mark(&mut self, m: &Mark, at: Instant, out: &mut Vec<String>) {
        match m {
            Mark::Banner => {
                out.push(String::new());
                out.extend(tui::banner_lines());
            }
            Mark::Header { text } => {
                if self.fatal.is_some() {
                    return;
                }
                self.phase = Phase::of(text);
                self.header = Some(text.clone());
                if text == "ASSAY" {
                    if !self.assay_done {
                        self.assay(at, out);
                    }
                    return;
                }
                if text == "GOAL NOT MET" {
                    // The assay's verdict line already says so, with the
                    // gap under it; a second banner would only repeat it.
                    return;
                }
                out.push(String::new());
                out.push(tui::rule_line(text, &tui::clock(self.elapsed(at)), self.width));
            }
            Mark::Status { icon, tone, text } => {
                if let Some(path) = text.strip_prefix("Blueprint: ") {
                    self.pending = Pending::Excerpt;
                    self.blueprint_head = Some(path.to_string());
                    return;
                }
                if text.starts_with("Mold:") {
                    self.pending = Pending::Table;
                    self.mold_head = Some(text.trim_start_matches("Mold:").trim().to_string());
                    return;
                }
                if let Some(n) = text.strip_suffix(" ingots added") {
                    let known: Vec<String> = self.plan.iter().map(|i| i.id.clone()).collect();
                    self.pending = Pending::Added(known);
                    self.mold_head = Some(format!("{n} ingots added"));
                    return;
                }
                out.push(tui::status_text(icon, tone.color(), text, self.width));
            }
            Mark::Counts { tally } => {
                out.push(tui::counts_text(&tally.counts(), self.width));
            }
            Mark::TemperBar { .. } => {
                // Only the assay draws this bar, and the echo draws the
                // assay whole.
            }
        }
    }

    fn event(&mut self, e: &EngineEvent, at: Instant, out: &mut Vec<String>) {
        match e {
            EngineEvent::Tokens { usage } => {
                self.usage.add(usage);
            }
            EngineEvent::Error { message } if crate::engine::events::is_fatal_provider(message) => {
                if self.fatal.is_none() {
                    self.fatal = Some(message.clone());
                }
                // Nothing is running any more: the footer goes.
                self.phase = Phase::Assay;
            }
            EngineEvent::Warning { message } => {
                self.note_round(message);
                if let Some(gap) = warden_word(message) {
                    self.goal_gap = Some(gap.to_string());
                    self.goal_met = false;
                }
            }
            EngineEvent::Narrate { text } => {
                self.note_round(text);
                if warden_word(text).is_some_and(|w| w.starts_with("goal met")) {
                    self.goal_gap = None;
                    self.goal_met = true;
                }
            }
            EngineEvent::IngotStart { id, .. } => {
                if self.phase != Phase::Forge {
                    self.phase = Phase::Forge;
                }
                if self.anvils.is_empty() {
                    self.anvils_line(id, out);
                }
                self.anvils.retain(|a| &a.id != id);
                self.anvils.push(Anvil { id: id.clone(), heat: 0, since: at, proof: None });
                self.ingot_head(id, out);
            }
            EngineEvent::HeatTick { id, heat } => {
                // A new heat means the last one on this ingot failed. When
                // no proof cue said how, say only that it did.
                if let Some(prev) = self.anvils.iter().find(|a| &a.id == id).map(|a| a.heat) {
                    if prev > 0 && !self.take_resolved(id, prev) {
                        let proof = self.ingot(id).map(|i| unescape(&i.proof)).filter(|p| !p.is_empty());
                        out.push(self.heat_line(id, prev, proof.as_deref(), None));
                    }
                }
                if *heat >= 2 {
                    self.retries += 1;
                }
                if let Some(a) = self.anvils.iter_mut().find(|a| &a.id == id) {
                    a.heat = *heat;
                } else {
                    self.anvils.push(Anvil { id: id.clone(), heat: *heat, since: at, proof: None });
                }
            }
            EngineEvent::IngotDone { id, ok } => {
                let anvil = self.anvils.iter().position(|a| &a.id == id).map(|i| self.anvils.remove(i));
                let heat = anvil.as_ref().map_or(0, |a| a.heat);
                if !*ok && heat > 0 && !self.take_resolved(id, heat) {
                    let proof = self.ingot(id).map(|i| unescape(&i.proof)).filter(|p| !p.is_empty());
                    out.push(self.heat_line(id, heat, proof.as_deref(), None));
                }
                self.resolved.retain(|(i, _)| i != id);
                let took = anvil.as_ref().map(|a| at.saturating_duration_since(a.since));
                let cmd = anvil
                    .and_then(|a| a.proof)
                    .or_else(|| self.ingot(id).map(|i| unescape(&i.proof)))
                    .filter(|p| !p.is_empty());
                out.push(self.done_line(id, *ok, heat, cmd.as_deref(), took));
            }
            EngineEvent::DuelRound { id, round } => {
                out.push(format!("    {} {}", paint(BRIGHT, "⚔"), dim(&format!("{id} duel round {round}"))));
            }
            EngineEvent::DuelVerdict { id, winner, margin } => {
                out.push(format!(
                    "    {} {}",
                    paint(PURE, "⚖"),
                    dim(&format!("{id} cast {winner} wins by {margin}"))
                ));
            }
            _ => {}
        }
    }

    /// `warden round 2: …` → remember that round 2 ran.
    fn note_round(&mut self, msg: &str) {
        let round = msg
            .strip_prefix("warden round ")
            .and_then(|r| r.split(':').next())
            .and_then(|n| n.trim().parse::<u32>().ok());
        if let Some(n) = round {
            self.warden_rounds = self.warden_rounds.max(n);
        }
    }

    fn take_resolved(&mut self, id: &str, heat: u8) -> bool {
        match self.resolved.iter().position(|(i, h)| i == id && *h == heat) {
            Some(at) => {
                self.resolved.remove(at);
                true
            }
            None => false,
        }
    }

    /// A heat's outcome, read from the forge's logs. A failure prints now;
    /// a pass waits for the ingot's "forged" line, which carries it.
    fn proof(&mut self, p: &Proof, out: &mut Vec<String>) {
        // An outcome for a heat this anvil never ran: the last heat before
        // a re-smelt, closed late by the rewritten ingot's first tick. The
        // re-smelt line already accounted for it.
        if self.anvils.iter().any(|a| a.id == p.id && a.heat == 0) {
            return;
        }
        self.proofs.insert(p.id.clone(), p.clone());
        if p.ok {
            if let Some(a) = self.anvils.iter_mut().find(|a| a.id == p.id) {
                a.proof = p.cmd.clone();
            }
            return;
        }
        let heat = self.anvils.iter().find(|a| a.id == p.id).map_or(p.heat, |a| a.heat);
        self.resolved.push((p.id.clone(), heat));
        // A reason with no words in it (a rule of dashes) is no reason.
        let why = p.why.as_deref().filter(|w| w.chars().any(char::is_alphanumeric));
        out.push(self.heat_line(&p.id, heat, p.cmd.as_deref(), why));
    }

    /// `    ✗ i2 heat 1/5  AssertionError: 3 != 4`. The reason leads: the
    /// ingot's header already showed its proof, so the command only fills
    /// in when no reason was recorded.
    fn heat_line(&self, id: &str, heat: u8, cmd: Option<&str>, why: Option<&str>) -> String {
        let max = self.max_heat(id).unwrap_or(heat.max(1));
        let head = format!("{id} heat {heat}/{max}");
        let room = self.width.saturating_sub(8 + head.chars().count());
        let mut line = format!("    {} {}", paint(WARM, "✗"), paint(WARM, &head));
        match (why, cmd) {
            (Some(why), _) if room > 8 => {
                line.push_str(&format!("  {}", tui::clip(why, room)));
            }
            (None, Some(cmd)) if room > 8 => {
                line.push_str(&format!("  {}", dim(&tui::clip(cmd, room))));
            }
            _ => {}
        }
        line
    }

    /// `  ✓ i2 forged  heat 2/5 · node --test · 1:12`
    fn done_line(&self, id: &str, ok: bool, heat: u8, cmd: Option<&str>, took: Option<Duration>) -> String {
        let max = self.max_heat(id).unwrap_or(heat.max(1));
        let (glyph, color, word) = if ok { ("✓", PURE, "forged") } else { ("✗", WARM, "cracked") };
        // What slag itself checked: the proof ran and exited 0. Anything a
        // model said about it sits above, in quotes, as the model's word.
        let mut meta = Vec::new();
        if ok {
            meta.push("proof exit 0".to_string());
            meta.push(format!("heat {heat}/{max}"));
        } else {
            meta.push(format!("after {heat} heats"));
        }
        if let Some(t) = took {
            meta.push(tui::clock(t));
        }
        let head = format!("  {} {}", paint(color, glyph), paint(color, &format!("{id} {word}")));
        let used = 5 + id.chars().count() + word.len();
        let tail = meta.join(" · ");
        let room = self.width.saturating_sub(used + tail.chars().count() + 6);
        let cmd = match cmd {
            Some(c) if ok && room > 10 => format!("  {}", dim(&tui::clip(c, room))),
            _ => String::new(),
        };
        tui::spread(&format!("{head}{cmd}"), &dim(&tail), self.width)
    }

    /// `  ⚒ 3 anvils  i1 · i2 · i3`, once per parallel batch: the batch is
    /// what the plan shows molten when its first ingot starts.
    fn anvils_line(&self, first: &str, out: &mut Vec<String>) {
        if !self.ingot(first).is_some_and(|i| i.solo) {
            return;
        }
        let mut ids: Vec<&str> = self
            .plan
            .iter()
            .filter(|i| i.solo && i.status == Status::Molten)
            .map(|i| i.id.as_str())
            .collect();
        if !ids.contains(&first) {
            // A copy of the plan from before the batch was marked: rebuild
            // it the way the scheduler picks one, solo ore up to the
            // default anvil count.
            ids = self
                .plan
                .iter()
                .filter(|i| i.solo && matches!(i.status, Status::Ore | Status::Molten))
                .map(|i| i.id.as_str())
                .take(crate::config::MAX_ANVILS)
                .collect();
        }
        if ids.len() < 2 {
            return;
        }
        out.push(format!(
            "  {} {}  {}",
            paint(HOT, "⚒"),
            paint(HOT, &format!("{} anvils", ids.len())),
            paint(PURE, &ids.join(" · "))
        ));
    }

    /// `  ▣ i2  pure counting core with no I/O …          g1 · cli`
    /// `    proof  node --test test/count.test.js`
    fn ingot_head(&self, id: &str, out: &mut Vec<String>) {
        let Some(ingot) = self.ingot(id) else {
            out.push(format!("  {} {}", paint(HOT, "▣"), paint(PURE, id)));
            return;
        };
        let mut tags = vec![format!("g{}", ingot.grade)];
        let skill = ingot.skill.to_string();
        if skill != "default" {
            tags.push(skill);
        }
        let tags = tags.join(" · ");
        let room = self.width.saturating_sub(8 + id.chars().count() + tags.chars().count());
        let left = format!(
            "  {} {}  {}",
            paint(HOT, "▣"),
            paint(PURE, id),
            tui::clip(&title(&ingot.work), room)
        );
        out.push(tui::spread(&left, &dim(&tags), self.width));
        if !ingot.proof.is_empty() {
            let room = self.width.saturating_sub(12);
            out.push(format!("    {}  {}", dim("proof"), dim(&tui::clip(&unescape(&ingot.proof), room))));
        }
    }

    /// Settle an ingot's last heat before its re-smelt line: the final heat
    /// failed (that is why it was re-smelted), and no tick followed it.
    fn close_for_resmelt(&mut self, id: &str, out: &mut Vec<String>) {
        if let Some(at) = self.anvils.iter().position(|a| a.id == id) {
            let anvil = self.anvils.remove(at);
            if anvil.heat > 0 && !self.take_resolved(id, anvil.heat) {
                let proof = self.ingot(id).map(|i| unescape(&i.proof)).filter(|p| !p.is_empty());
                out.push(self.heat_line(id, anvil.heat, proof.as_deref(), None));
            }
        }
    }

    /// A failed ingot the founder rewrote or split instead of cracking.
    fn resmelts(&mut self, next: &[Ingot], out: &mut Vec<String>) {
        let previous = self.plan.clone();
        for old in &previous {
            match next.iter().find(|n| n.id == old.id) {
                Some(new) if new.smelt > old.smelt => {
                    self.close_for_resmelt(&old.id, out);
                    let room = self.width.saturating_sub(18 + old.id.len());
                    out.push(format!(
                        "  {} {}  {}",
                        paint(HOT, "♻"),
                        paint(HOT, &format!("{} re-smelted", old.id)),
                        dim(&tui::clip(&title(&new.work), room))
                    ));
                }
                None if old.status != Status::Forged => {
                    let parts: Vec<&str> = next
                        .iter()
                        .filter(|n| n.id.starts_with(&old.id) && n.id != old.id)
                        .map(|n| n.id.as_str())
                        .collect();
                    if !parts.is_empty() {
                        self.close_for_resmelt(&old.id, out);
                        out.push(format!(
                            "  {} {}  {}",
                            paint(HOT, "♻"),
                            paint(HOT, &format!("{} split", old.id)),
                            paint(PURE, &parts.join(" · "))
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    fn flush_pending(&mut self, out: &mut Vec<String>) {
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => {}
            Pending::Table => {
                let head = self.mold_head.take().unwrap_or_default();
                out.push(self.labelled("◆", PURE, "mold", &head));
                let rows: Vec<&Ingot> = self.plan.iter().collect();
                self.table(&rows, out);
            }
            Pending::Added(known) => {
                let head = self.mold_head.take().unwrap_or_default();
                out.push(self.labelled("+", HOT, "mold", &head));
                let rows: Vec<&Ingot> = self.plan.iter().filter(|i| !known.contains(&i.id)).collect();
                self.table(&rows, out);
            }
            Pending::Excerpt => self.excerpt(out),
        }
    }

    /// The mold, one ingot a row: `    i1  par g1  package manifest …`.
    /// `par` forges beside others, `seq` waits its turn.
    fn table(&self, rows: &[&Ingot], out: &mut Vec<String>) {
        let id_w = rows.iter().map(|i| i.id.chars().count()).max().unwrap_or(2);
        let shown = rows.len().min(12);
        for ingot in rows.iter().take(shown) {
            // Words, not glyphs: `∥` renders as a stray bar in half the
            // fonts a terminal ships with.
            let lane = if ingot.solo { paint(HOT, "par") } else { dim("seq") };
            let room = self.width.saturating_sub(id_w + 16);
            out.push(format!(
                "    {}  {} {}  {}",
                paint(PURE, &format!("{:<id_w$}", ingot.id)),
                lane,
                dim(&format!("g{}", ingot.grade)),
                tui::clip(&title(&ingot.work), room)
            ));
        }
        if rows.len() > shown {
            out.push(format!("    {}", dim(&format!("… {} more in {}", rows.len() - shown, crate::config::CRUCIBLE))));
        }
    }

    /// The blueprint's opening, enough to recognise it: its first lines of
    /// prose, clipped, and how much more there is.
    fn excerpt(&mut self, out: &mut Vec<String>) {
        let path = self.blueprint_head.take().unwrap_or_else(|| crate::config::BLUEPRINT.to_string());
        let Some(doc) = &self.blueprint else {
            out.push(self.labelled("◆", PURE, "blueprint", &path));
            return;
        };
        let total = doc.lines().count();
        out.push(self.labelled("◆", PURE, "blueprint", &format!("{path} · {total} lines")));
        let picked: Vec<&str> = doc
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with('|'))
            .take(4)
            .collect();
        let room = self.width.saturating_sub(4);
        for line in &picked {
            out.push(format!("    {}", dim(&tui::clip(line, room))));
        }
    }

    /// The report card. The verdict comes first and is the only claim on
    /// it: no bar, since a full bar over a failed goal reads as success.
    /// Then why and what to do next, then the evidence: what ran and how
    /// it exited, what git says changed, where the work is, what it cost.
    fn assay(&mut self, at: Instant, out: &mut Vec<String>) {
        self.assay_done = true;
        self.phase = Phase::Assay;
        let counts = self.counts();
        let stats = self.stats.clone().unwrap_or_default();
        let w = self.width;
        out.push(String::new());
        out.push(tui::rule_line("ASSAY", &tui::clock(self.elapsed(at)), w));

        // The verdict: one word, one clause.
        let left = counts.ore + counts.molten;
        let (glyph, color, word, says) = if counts.cracked > 0 {
            let n = counts.cracked;
            ("✗", WARM, "CRACKED", format!("{n} of {} ingots failed every heat; {} forged", counts.total, counts.forged))
        } else if self.goal_gap.is_some() {
            ("✗", WARM, "GOAL NOT MET", "every task passed its proof; the warden rejected the result".to_string())
        } else if left > 0 {
            ("!", BRIGHT, "PAUSED", format!("{} of {} forged; {left} still queued", counts.forged, counts.total))
        } else if self.goal_met {
            ("█", PURE, "FORGED", format!("all {} ingots passed their proofs; the warden accepted the goal", counts.total))
        } else {
            ("█", PURE, "FORGED", format!("all {} ingots passed their proofs", counts.total))
        };
        let head = format!("  {} {}  ", paint(color, glyph), paint(color, &format!("{}{word}{}", bold_on(), bold_off())));
        let room = w.saturating_sub(tui::visible_len(&head));
        out.push(format!("{head}{}", tui::clip(&says, room)));

        // Why, when it failed: the gap, or each cracked ingot's last reason.
        if let Some(gap) = &self.goal_gap {
            out.extend(wrap_indented(gap, w, 4, 3).into_iter().map(|l| paint(WARM, &l)));
        }
        let cracked: Vec<&Ingot> = self.plan.iter().filter(|i| i.status == Status::Cracked).collect();
        for ingot in &cracked {
            let why = self
                .proofs
                .get(&ingot.id)
                .and_then(|p| p.why.clone())
                .unwrap_or_else(|| "every heat failed its proof".to_string());
            let name = tui::clip(&title(&ingot.work), (w / 3).max(12));
            // `    ✗ id  name  — why`: twelve fixed columns besides the three.
            let room = w.saturating_sub(12 + ingot.id.len() + name.chars().count());
            out.push(format!(
                "    {} {}  {}  {}",
                paint(WARM, "✗"),
                paint(WARM, &ingot.id),
                name,
                dim(&format!("— {}", tui::clip(&why, room.max(8))))
            ));
        }

        // The next command, the one thing to type.
        let next = if self.goal_gap.is_some() {
            Some((format!("slag --temper {} resume", self.warden_rounds + 1), "forges the gap as new ingots".to_string()))
        } else if let Some(first) = cracked.first() {
            Some((format!("less logs/*_ASSAY_{}_*.log", first.id), "the failing proof's own output".to_string()))
        } else if left > 0 {
            Some(("slag resume".to_string(), format!("forges the {left} left")))
        } else {
            match stats.commits.filter(|n| *n > 0) {
                Some(n) => Some((format!("git log --stat -{n}"), "the work, commit by commit".to_string())),
                None => Some(("git status".to_string(), "the work in the tree".to_string())),
            }
        };
        if let Some((cmd, why)) = next {
            let room = w.saturating_sub(12 + cmd.chars().count());
            out.push(format!(
                "  {}  {}  {}",
                dim(&format!("{:<6}", "next")),
                paint(HOT, &cmd),
                dim(&tui::clip(&why, room.max(4)))
            ));
        }
        out.push(String::new());

        // Evidence: what ran, and how each proof exited.
        let mut ran: Vec<String> = Vec::new();
        let mut quiet_passes = 0;
        let cap = if w >= 100 { 8 } else { 4 };
        for ingot in self.plan.iter().filter(|i| i.status != Status::Cracked) {
            let row = match (&ingot.status, self.proofs.get(&ingot.id)) {
                (Status::Forged, Some(p)) if p.ok => {
                    if ran.len() >= cap {
                        quiet_passes += 1;
                        continue;
                    }
                    let cmd = p.cmd.clone().unwrap_or_else(|| unescape(&ingot.proof));
                    let room = w.saturating_sub(22 + ingot.id.len());
                    // The exit code sits in its own column: a reader scans
                    // down it for anything that is not 0.
                    let left = format!(
                        "  {}  {} {}  {}",
                        dim(&format!("{:<6}", if ran.is_empty() { "ran" } else { "" })),
                        paint(PURE, "✓"),
                        paint(PURE, &ingot.id),
                        tui::clip(&cmd, room.max(8))
                    );
                    ran.push(tui::spread(&left, &dim("exit 0"), w));
                    continue;
                }
                // Forged, but this tape kept no outcome for its proof: say
                // so instead of drawing a ✓ nothing backs.
                (Status::Forged, _) => format!("{} {}  {}", dim("·"), paint(PURE, &ingot.id), dim("forged; no proof outcome recorded")),
                (Status::Ore | Status::Molten, _) => format!("{} {}  {}", dim("·"), dim(&ingot.id), dim("not forged yet")),
                (Status::Cracked, _) => continue,
            };
            let label = if ran.is_empty() { "ran" } else { "" };
            ran.push(format!("  {}  {row}", dim(&format!("{label:<6}"))));
        }
        out.extend(ran.iter().cloned());
        if quiet_passes > 0 {
            let label = if ran.is_empty() { "ran" } else { "" };
            out.push(format!(
                "  {}  {}",
                dim(&format!("{label:<6}")),
                dim(&format!("… {quiet_passes} more, each proof exit 0"))
            ));
        }

        // What git says changed, and where the work now is.
        let mut diff: Vec<String> = Vec::new();
        if let Some(d) = &stats.diff {
            diff.push(d.clone());
        } else if let Some(c) = &stats.churn {
            diff.push(format!("{} lines", c.trim_start_matches("churn: ").split(' ').next().unwrap_or("")));
        }
        if let Some(n) = stats.commits.filter(|n| *n > 0) {
            diff.push(format!("{n} {}", if n == 1 { "commit" } else { "commits" }));
        }
        if !diff.is_empty() {
            out.push(self.fact("diff", &diff.join(" · ")));
        }
        let mut work: Vec<String> = Vec::new();
        if let (Some(b), Some(h)) = (&stats.branch, &stats.head) {
            work.push(format!("{b} @ {h}"));
        }
        work.push(format!("{}/", crate::config::LOG_DIR));
        work.push(crate::config::CRUCIBLE.to_string());
        out.push(self.fact("work", &work.join(" · ")));

        // What it took.
        let mut run = vec![tui::clock(self.elapsed(at))];
        if let Some(api) = stats.durations.as_deref().and_then(|d| d.split(" · ").find(|p| p.starts_with("api "))) {
            run.push(api.to_string());
        }
        if self.usage.total_tokens > 0 || self.usage.cost.is_some() {
            run.push(crate::engine::pricing::format_cost(&self.usage));
            if self.usage.cost.is_some() {
                run.push(format!("{} tok", tui::tokens(self.usage.total_tokens)));
            }
        }
        if self.retries > 0 {
            run.push(format!("{} {}", self.retries, if self.retries == 1 { "retry" } else { "retries" }));
        }
        out.push(self.fact("run", &run.join(" · ")));
        if let Some(t) = &stats.tool_errors {
            out.push(format!(
                "  {}  {}",
                paint(WARM, &format!("{:<6}", "errors")),
                tui::clip(t.trim_start_matches("tool errors: "), w.saturating_sub(10))
            ));
        }
        out.push(String::new());
    }

    fn fact(&self, label: &str, text: &str) -> String {
        format!("  {}  {}", dim(&format!("{label:<6}")), tui::clip(text, self.width.saturating_sub(10)))
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
}

/// The assay for the run that just ended in this directory, from PLAN.md
/// and the run's own figures. The dashboard's alternate screen took the
/// in-run assay with it, so the caller prints this after it closes.
pub fn assay_report(width: usize) -> Vec<String> {
    let Ok(plan) = std::fs::read_to_string(crate::config::CRUCIBLE) else {
        return Vec::new();
    };
    let stats = Stats::capture();
    let now = Instant::now();
    let wall = crate::engine::stats::snapshot().wall.unwrap_or_default();
    let mut echo = Echo::new(width);
    let start = now.checked_sub(wall).unwrap_or(now);
    echo.cue(&Cue::Plan(plan), start);
    let total = crate::engine::stats::snapshot().ledger.total();
    echo.usage = total;
    echo.cue(&Cue::Stats(stats), now);
    echo.finish(now)
}

/// `warden round 2: no tests cover the CLI` → `no tests cover the CLI`.
pub fn warden_word(msg: &str) -> Option<&str> {
    let rest = msg.strip_prefix("warden round ")?;
    rest.split_once(": ").map(|(_, w)| w)
}

/// What an ingot is, not how the founder labelled it: plans open every
/// `:work` with `Goal:` or `TASK:`, which is boilerplate once it is on
/// every row.
pub fn title(work: &str) -> String {
    let work = unescape(work);
    let mut t = work.trim();
    for label in ["GOAL:", "TASK:", "INGOT:", "WORK:"] {
        if t.len() >= label.len() && t[..label.len()].eq_ignore_ascii_case(label) {
            t = t[label.len()..].trim_start();
            break;
        }
    }
    // A brief runs on (`Why: …`, file lists, rules); its first sentence is
    // the name. Kept whole when that sentence is too short to name it.
    let cut = [". ", " Why:", " WHY:"]
        .iter()
        .filter_map(|m| t.find(m).map(|i| if *m == ". " { i + 1 } else { i }))
        .min();
    match cut {
        Some(i) if i >= 12 => t[..i].trim_end().to_string(),
        _ => t.to_string(),
    }
}

/// A plan string as a person typed it: the crucible keeps `\"` and `\\`
/// escaped, which reads as noise on screen.
pub fn unescape(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// The commission as one line: PRD.md's last non-empty line, which is
/// where `fire_furnace` writes it.
fn commission(prd: &str) -> Option<String> {
    prd.lines().map(str::trim).rfind(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string)
}

/// A spend ledger row as a reader wants it: the call site first, then
/// what it burned, then the model without its vendor prefix.
/// `anthropic/claude-haiku-5.5  smith  1619164 tok  $0.1784`
/// → `smith      1.6M tok  $0.18  claude-haiku-5.5`.
fn ledger_row(row: &str) -> String {
    let parts: Vec<&str> = row.split_whitespace().collect();
    match parts.as_slice() {
        [model, role, n, "tok", cost @ ..] => {
            let tok = n.parse::<u64>().map(tui::tokens).unwrap_or_else(|_| n.to_string());
            let dollars = cost
                .first()
                .and_then(|c| c.trim_start_matches('$').parse::<f64>().ok())
                .map(tui::money)
                .unwrap_or_else(|| cost.join(" "));
            let model = model.rsplit('/').next().unwrap_or(model);
            format!("{role:<9} {tok:>6} tok  {dollars:<7} {model}")
        }
        _ => parts.join(" "),
    }
}

/// `text` wrapped under an indent of `indent` spaces, at most `max` lines.
fn wrap_indented(text: &str, width: usize, indent: usize, max: usize) -> Vec<String> {
    let room = width.saturating_sub(indent).max(20);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > room {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max {
        let rest = lines[max - 1..].join(" ");
        lines.truncate(max - 1);
        lines.push(tui::clip(&rest, room));
    }
    lines.into_iter().map(|l| format!("{}{}", " ".repeat(indent), tui::clip(&l, room))).collect()
}

/// `  gap    text…` wrapped under its label, at most `max` lines.
fn wrap_labelled(label: &str, text: &str, width: usize, max: usize) -> Vec<String> {
    let room = width.saturating_sub(10).max(20);
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > room {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max {
        lines.truncate(max);
        if let Some(last) = lines.last_mut() {
            *last = tui::clip(&format!("{last} …"), room);
        }
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| if i == 0 { format!("  {label:<6}  {l}") } else { format!("          {l}") })
        .collect()
}

fn bold_on() -> String {
    if tui::colored() {
        "\x1b[1m".to_string()
    } else {
        String::new()
    }
}

fn bold_off() -> String {
    if tui::colored() {
        "\x1b[22m".to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"(ingot :id "i1" :status molten :solo t :grade 1 :skill default :heat 0 :max 5 :proof "test -f a" :work "Goal: write a")
(ingot :id "i2" :status molten :solo t :grade 2 :skill cli :heat 0 :max 5 :proof "node --test" :work "Goal: write b")
(ingot :id "i3" :status ore :solo nil :grade 1 :skill default :heat 0 :max 5 :proof "true" :work "Goal: docs")"#;

    fn ev(e: EngineEvent) -> Cue {
        Cue::Event(e)
    }

    fn plain(lines: &[String]) -> String {
        lines.iter().map(|l| strip(l)).collect::<Vec<_>>().join("\n")
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn feed(echo: &mut Echo, cues: &[Cue]) -> String {
        let t0 = Instant::now();
        let mut out = Vec::new();
        for (i, c) in cues.iter().enumerate() {
            out.extend(echo.cue(c, t0 + Duration::from_secs(i as u64)));
        }
        plain(&out)
    }

    #[test]
    fn a_parallel_batch_announces_its_anvils_once() {
        let mut echo = Echo::new(80);
        let text = feed(
            &mut echo,
            &[
                Cue::Mark(Mark::Header { text: "FORGE".into() }),
                Cue::Plan(PLAN.into()),
                ev(EngineEvent::IngotStart { id: "i1".into(), work: "a".into() }),
                ev(EngineEvent::IngotStart { id: "i2".into(), work: "b".into() }),
            ],
        );
        assert_eq!(text.matches("2 anvils").count(), 1, "{text}");
        assert!(text.contains("i1 · i2"), "{text}");
        assert!(text.contains("▣ i2  write b"), "{text}");
        assert!(text.contains("g2 · cli"), "{text}");
        assert!(!text.contains("Goal:"), "plan labels come off: {text}");
    }

    #[test]
    fn a_failed_heat_prints_its_reason_once() {
        let mut echo = Echo::new(100);
        let text = feed(
            &mut echo,
            &[
                Cue::Plan(PLAN.into()),
                ev(EngineEvent::IngotStart { id: "i2".into(), work: "b".into() }),
                ev(EngineEvent::HeatTick { id: "i2".into(), heat: 1 }),
                Cue::Proof(Proof {
                    id: "i2".into(),
                    heat: 1,
                    cmd: Some("node --test".into()),
                    ok: false,
                    why: Some("2 tests failed".into()),
                }),
                ev(EngineEvent::HeatTick { id: "i2".into(), heat: 2 }),
                Cue::Proof(Proof { id: "i2".into(), heat: 2, cmd: Some("node --test".into()), ok: true, why: None }),
                ev(EngineEvent::IngotDone { id: "i2".into(), ok: true }),
            ],
        );
        assert_eq!(text.matches("heat 1/5").count(), 1, "{text}");
        assert!(text.contains("✗ i2 heat 1/5  2 tests failed"), "{text}");
        assert!(text.contains("✓ i2 forged"), "{text}");
        assert!(text.contains("heat 2/5"), "{text}");
    }

    #[test]
    fn without_proof_cues_a_retry_still_shows() {
        let mut echo = Echo::new(80);
        let text = feed(
            &mut echo,
            &[
                Cue::Plan(PLAN.into()),
                ev(EngineEvent::IngotStart { id: "i1".into(), work: "a".into() }),
                ev(EngineEvent::HeatTick { id: "i1".into(), heat: 1 }),
                ev(EngineEvent::HeatTick { id: "i1".into(), heat: 2 }),
                ev(EngineEvent::IngotDone { id: "i1".into(), ok: false }),
            ],
        );
        assert!(text.contains("✗ i1 heat 1/5"), "{text}");
        assert!(text.contains("✗ i1 heat 2/5"), "{text}");
        assert!(text.contains("✗ i1 cracked"), "{text}");
    }

    #[test]
    fn the_mold_follows_its_status_once_the_plan_lands() {
        let mut echo = Echo::new(80);
        let text = feed(
            &mut echo,
            &[
                Cue::Mark(Mark::Header { text: "FOUNDER · casting mold".into() }),
                Cue::Mark(Mark::Status {
                    icon: "█".into(),
                    tone: crate::render::tape::Tone::Pure,
                    text: "Mold: 3 ingots (2 simple, 1 complex, 0 web)".into(),
                }),
                Cue::Plan(PLAN.into()),
            ],
        );
        assert!(text.contains("mold  3 ingots"), "{text}");
        assert!(text.contains("i3  seq g1  docs"), "{text}");
        assert!(text.contains("i1  par g1  write a"), "{text}");
    }

    #[test]
    fn a_ledger_row_leads_with_the_call_site() {
        assert_eq!(
            ledger_row("anthropic/claude-haiku-5.5  smith       1619164 tok  $0.1784"),
            "smith       1.6M tok  $0.18   claude-haiku-5.5"
        );
        assert_eq!(ledger_row("odd row"), "odd row");
    }

    #[test]
    fn a_title_is_the_briefs_first_sentence() {
        assert_eq!(title("Goal: make the core pure. Why: tests import it."), "make the core pure.");
        assert_eq!(title("TASK: a thin CLI over core Why: users run it"), "a thin CLI over core");
        // Too short to stand alone: keep the whole brief.
        assert_eq!(title("Fix it. Then document the API"), "Fix it. Then document the API");
        assert_eq!(title(r#"say \"hi\""#), r#"say "hi""#);
    }

    #[test]
    fn every_line_fits_the_width() {
        for width in [40usize, 79, 119] {
            let mut echo = Echo::new(width);
            let long = "x".repeat(400);
            let plan = PLAN.replace("write b", &long);
            let mut lines = Vec::new();
            let t0 = Instant::now();
            for c in [
                Cue::Mark(Mark::Banner),
                Cue::Mark(Mark::Header { text: format!("SURVEYOR · {long}") }),
                Cue::Doc { name: "PRD.md".into(), text: long.clone() },
                Cue::Plan(plan.clone()),
                ev(EngineEvent::IngotStart { id: "i2".into(), work: long.clone() }),
                ev(EngineEvent::HeatTick { id: "i2".into(), heat: 1 }),
                Cue::Proof(Proof { id: "i2".into(), heat: 1, cmd: Some(long.clone()), ok: false, why: Some(long.clone()) }),
                ev(EngineEvent::IngotDone { id: "i2".into(), ok: false }),
                Cue::Plan(plan.replace(":status molten", ":status cracked")),
                ev(EngineEvent::Warning { message: format!("warden round 1: {long}") }),
            ] {
                lines.extend(echo.cue(&c, t0));
            }
            lines.extend(echo.finish(t0));
            for l in &lines {
                let n = tui::visible_len(l);
                assert!(n <= width, "{n} > {width}: {:?}", strip(l));
            }
        }
    }

    #[test]
    fn a_run_stopped_by_its_provider_gets_no_assay() {
        let fatal = "provider error: invalid key (401): openai, role warden, model m. Set OPENAI_API_KEY";
        let done = PLAN.replace(":status molten", ":status forged").replace(":status ore", ":status forged");
        let t0 = Instant::now();
        let mut live = Echo::new(80);
        live.cue(&Cue::Plan(done.clone()), t0);
        live.cue(&ev(EngineEvent::Error { message: fatal.into() }), t0);
        let text = plain(&live.finish(t0));
        assert!(!text.contains("FORGED") && !text.contains("provider error"), "{text}");
        // A replay has no `main` to print the line, so it prints it once.
        let mut replay = Echo::new(80).owning_fatal(true);
        replay.cue(&Cue::Plan(done), t0);
        replay.cue(&ev(EngineEvent::Error { message: fatal.into() }), t0);
        let text = plain(&replay.finish(t0));
        assert_eq!(text.matches("invalid key").count(), 1, "{text}");
        assert!(!text.contains("FORGED"), "{text}");
    }

    #[test]
    fn the_assay_leads_with_the_verdict() {
        let mut echo = Echo::new(80);
        let done = PLAN.replace(":status molten", ":status forged").replace(":status ore", ":status cracked");
        let t0 = Instant::now();
        echo.cue(&Cue::Plan(done), t0);
        echo.cue(&Cue::Stats(Stats { durations: Some("wall 4m12s · api 2m01s · tools 1m40s".into()), ..Default::default() }), t0);
        let text = plain(&echo.finish(t0 + Duration::from_secs(252)));
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(lines[1].contains("CRACKED") && lines[1].contains("1 of 3"), "{text}");
        assert!(text.contains("✗ i3"), "{text}");
        assert!(text.contains("next    less logs/*_ASSAY_i3_*.log"), "{text}");
        assert!(text.contains("api 2m01s"), "{text}");
        // No bar anywhere in the verdict block: a full one reads as success.
        assert!(!text.contains('█') && !text.contains('░'), "{text}");
    }

    #[test]
    fn a_forged_ingot_without_a_proof_outcome_gets_no_check_mark() {
        let mut echo = Echo::new(100);
        let done = PLAN.replace(":status molten", ":status forged").replace(":status ore", ":status forged");
        let t0 = Instant::now();
        echo.cue(&Cue::Plan(done), t0);
        echo.cue(&Cue::Proof(Proof { id: "i1".into(), heat: 1, cmd: Some("test -f a".into()), ok: true, why: None }), t0);
        echo.cue(&ev(EngineEvent::Warning { message: "warden round 1: the README is missing".into() }), t0);
        let text = plain(&echo.finish(t0));
        assert!(text.contains("GOAL NOT MET"), "{text}");
        assert!(text.contains("slag --temper 2 resume"), "{text}");
        assert!(text.contains("✓ i1  test -f a") && text.contains("exit 0"), "{text}");
        assert!(text.contains("· i2  forged; no proof outcome recorded"), "{text}");
        assert!(!text.contains("✓ i2"), "{text}");
    }

    /// Every done screen the assay draws, painted as a terminal paints it:
    /// each escape must be whole. A lost ESC prints its body as text.
    #[test]
    fn every_assay_verdict_renders_with_every_escape_whole() {
        let t0 = Instant::now();
        let forged = PLAN.replace(":status molten", ":status forged").replace(":status ore", ":status forged");
        let cracked = PLAN.replace(":status molten", ":status forged").replace(":status ore", ":status cracked");
        let cases: [(&str, String, Vec<Cue>); 3] = [
            ("FORGED", forged.clone(), vec![]),
            (
                "GOAL NOT MET",
                forged,
                vec![ev(EngineEvent::Warning { message: "warden round 1: the README never shows the CLI".into() })],
            ),
            ("CRACKED", cracked, vec![]),
        ];
        for (word, plan, extra) in cases {
            for width in [79usize, 159] {
                let text = tui::with_color(true, || {
                    let mut echo = Echo::new(width);
                    echo.cue(&Cue::Plan(plan.clone()), t0);
                    echo.cue(&Cue::Proof(Proof { id: "i1".into(), heat: 1, cmd: Some("test -f a".into()), ok: true, why: None }), t0);
                    echo.cue(&Cue::Proof(Proof { id: "i3".into(), heat: 5, cmd: Some("test -f c".into()), ok: false, why: Some("no such file".into()) }), t0);
                    for c in &extra {
                        echo.cue(c, t0);
                    }
                    echo.cue(&Cue::Stats(Stats { diff: Some("2 files changed, 9 insertions(+)".into()), commits: Some(2), head: Some("0f6ac66".into()), branch: Some("main".into()), ..Default::default() }), t0);
                    echo.finish(t0 + Duration::from_secs(90)).join("\n")
                });
                assert!(text.contains(word), "{word}: {text}");
                let problems = tui::ansi_problems(&text);
                assert!(problems.is_empty(), "{word} at {width}: {problems:?}\n{text}");
            }
        }
    }
}
