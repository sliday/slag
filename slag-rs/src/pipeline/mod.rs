pub mod surveyor;
pub mod founder;
pub mod forge;
pub mod duel;
pub mod resmelt;
pub mod assay;
pub mod warden;
pub mod fix;

use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};

use crate::config::EngineConfig;
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::smith::EngineHooks;
use crate::tui;

pub(crate) const COORDINATOR_FILES: &[&str] = &[
    "PLAN.md", "PROGRESS.md", "BLUEPRINT.md", "BAR.md", "PRD.md", "AGENTS.md",
];

struct CoordinatorFile {
    path: std::path::PathBuf,
    body: Option<Vec<u8>>,
    symlink: Option<std::path::PathBuf>,
}

struct RecoveryLog {
    path: std::path::PathBuf,
    backup: std::path::PathBuf,
    body: std::sync::Arc<Vec<u8>>,
}

pub(crate) struct CoordinatorGuard {
    root: std::path::PathBuf,
    files: Vec<CoordinatorFile>,
    logs: Vec<RecoveryLog>,
    log_links: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    log_dirs: Vec<std::path::PathBuf>,
    backup: std::path::PathBuf,
    armed: bool,
}

impl CoordinatorGuard {
    pub(crate) fn capture(root: &std::path::Path) -> std::io::Result<Self> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = root.canonicalize()?;
        let backup = std::env::temp_dir().join(format!(
            "slag-coordinator-{}-{}-{}", std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&backup)?;
        let mut guard = Self {
            root, files: Vec::new(), logs: Vec::new(), log_links: Vec::new(),
            log_dirs: Vec::new(), backup, armed: false,
        };
        for name in COORDINATOR_FILES {
            let path = guard.root.join(name);
            let body = match std::fs::read(&path) {
                Ok(body) => Some(body),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            let symlink = std::fs::read_link(&path).ok();
            guard.files.push(CoordinatorFile { path, body, symlink });
        }
        for path in recovery_paths(&guard.root.join("logs"))? {
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&path)?;
                guard.log_links.push((path, target));
                continue;
            }
            if metadata.is_dir() { guard.log_dirs.push(path.clone()); }
            if metadata.is_file() {
                let mut backup = guard.backup.join(guard.logs.len().to_string());
                if let Err(e) = std::fs::hard_link(&path, &backup) {
                    if e.raw_os_error() != Some(18) || !guard.logs.is_empty() { return Err(e); }
                    let local = guard.root.join(".slag/coordinator-backups").join(guard.backup.file_name().unwrap());
                    ensure_recovery_dir(local.parent().unwrap(), &guard.root)?;
                    builder.create(&local)?;
                    std::fs::remove_dir(&guard.backup)?;
                    guard.backup = local;
                    backup = guard.backup.join("0");
                    std::fs::hard_link(&path, &backup)?;
                }
                let body = recovery_bytes(&path)?;
                guard.logs.push(RecoveryLog { path, backup, body });
            }
        }
        guard.armed = true;
        Ok(guard)
    }

    pub(crate) fn restore(&self) -> std::io::Result<Vec<String>> {
        use std::io::{Seek, Write};
        let mut changed = Vec::new();
        for file in &self.files {
            let metadata = std::fs::symlink_metadata(&file.path).ok();
            let current = std::fs::read(&file.path).ok();
            let link = std::fs::read_link(&file.path).ok();
            let correct_type = if file.symlink.is_some() {
                link == file.symlink
            } else {
                metadata.as_ref().is_none_or(|m| m.is_file())
            };
            let append_only = file.path.ends_with("PROGRESS.md")
                && file.body.as_ref().zip(current.as_ref()).is_some_and(|(old, new)| new.starts_with(old));
            if correct_type && (current == file.body || append_only) {
                continue;
            }
            remove_recovery_path(&file.path)?;
            if let Some(target) = &file.symlink {
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &file.path)?;
                #[cfg(not(unix))]
                std::os::windows::fs::symlink_file(target, &file.path)?;
                if std::fs::read(&file.path).ok() != file.body {
                    if !file.path.canonicalize()?.starts_with(&self.root) {
                        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
                            "cannot restore coordinator symlink content outside the workspace"));
                    }
                    if let Some(body) = &file.body { std::fs::write(&file.path, body)?; }
                }
            } else if let Some(body) = &file.body {
                std::fs::write(&file.path, body)?;
            }
            changed.push(file.path.file_name().unwrap().to_string_lossy().into_owned());
        }
        for log in &self.logs {
            let current = std::fs::read(&log.backup)?;
            let rel = log.path.strip_prefix(self.root.join("logs")).unwrap();
            let tail = recovery_tail(rel, &log.body, &current);
            let intact = current.starts_with(&log.body) && current[log.body.len()..] == tail;
            if !intact {
                let mut writer = std::fs::OpenOptions::new().write(true).open(&log.backup)?;
                writer.rewind()?;
                writer.write_all(&log.body)?;
                writer.write_all(&tail)?;
                writer.set_len((log.body.len() + tail.len()) as u64)?;
            }
            let same_inode = same_recovery_file(&log.path, &log.backup);
            if !same_inode {
                ensure_recovery_dir(log.path.parent().unwrap(), &self.root)?;
                remove_recovery_path(&log.path)?;
                std::fs::hard_link(&log.backup, &log.path)?;
            }
            if !intact || !same_inode {
                changed.push(format!("logs/{}", rel.display()));
            }
        }
        for (path, target) in &self.log_links {
            if std::fs::read_link(path).ok().as_ref() != Some(target) {
                ensure_recovery_dir(path.parent().unwrap(), &self.root)?;
                remove_recovery_path(path)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, path)?;
                #[cfg(not(unix))]
                std::os::windows::fs::symlink_file(target, path)?;
                changed.push(format!("logs/{}", path.strip_prefix(self.root.join("logs")).unwrap().display()));
            }
        }
        let log_root = self.root.join("logs");
        for path in recovery_paths(&log_root)?.into_iter().rev() {
            if self.logs.iter().any(|log| log.path == path)
                || self.log_links.iter().any(|(link, _)| *link == path) { continue; }
            let rel = path.strip_prefix(&log_root).unwrap();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                if !self.log_dirs.contains(&path) && std::fs::read_dir(&path)?.next().is_none() {
                    std::fs::remove_dir(&path)?;
                }
            } else if !metadata.is_file() || !runtime_log(rel) {
                remove_recovery_path(&path)?;
                changed.push(format!("logs/{}", rel.display()));
            }
        }
        Ok(changed)
    }
}

impl Drop for CoordinatorGuard {
    fn drop(&mut self) {
        if self.armed { let _ = self.restore(); }
        let _ = std::fs::remove_dir_all(&self.backup);
        if self.backup.starts_with(self.root.join(".slag/coordinator-backups")) {
            let _ = std::fs::remove_dir(self.root.join(".slag/coordinator-backups"));
            let _ = std::fs::remove_dir(self.root.join(".slag"));
        }
    }
}

fn recovery_paths(root: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    let Ok(metadata) = std::fs::symlink_metadata(root) else { return Ok(paths) };
    if !metadata.is_dir() { paths.push(root.to_path_buf()); return Ok(paths); }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        paths.push(path.clone());
        if entry.file_type()?.is_dir() { paths.extend(recovery_paths(&path)?); }
    }
    Ok(paths)
}

fn remove_recovery_path(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn ensure_recovery_dir(path: &std::path::Path, root: &std::path::Path) -> std::io::Result<()> {
    let rel = path.strip_prefix(root).map_err(std::io::Error::other)?;
    let mut current = root.to_path_buf();
    for part in rel.components() {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.is_dir() => continue,
            Ok(_) => remove_recovery_path(&current)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::fs::create_dir(&current)?;
    }
    Ok(())
}

fn same_recovery_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::symlink_metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.is_file() && a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    { std::fs::read(a).ok() == std::fs::read(b).ok() }
}

fn recovery_bytes(path: &std::path::Path) -> std::io::Result<std::sync::Arc<Vec<u8>>> {
    use std::io::{Read, Seek};
    type Cache = std::collections::HashMap<std::path::PathBuf, std::sync::Arc<Vec<u8>>>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let mut bytes = cache.remove(path).unwrap_or_default();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0u8; 8_192];
    let mut matched = 0;
    while matched < bytes.len() {
        let size = (bytes.len() - matched).min(buffer.len());
        let read = file.read(&mut buffer[..size])?;
        if read == 0 || buffer[..read] != bytes[matched..matched + read] {
            bytes = Default::default();
            file.rewind()?;
            break;
        }
        matched += read;
    }
    file.read_to_end(std::sync::Arc::make_mut(&mut bytes))?;
    if cache.len() >= 128 || cache.values().map(|b| b.len()).sum::<usize>() + bytes.len() > 16 * 1024 * 1024 {
        cache.clear();
    }
    if bytes.len() <= 16 * 1024 * 1024 { cache.insert(path.to_path_buf(), bytes.clone()); }
    Ok(bytes)
}

fn recovery_tail(path: &std::path::Path, before: &[u8], current: &[u8]) -> Vec<u8> {
    if let Some(suffix) = current.strip_prefix(before) { return suffix.to_vec(); }
    let mut tail = Vec::new();
    for line in current.split_inclusive(|b| *b == b'\n') {
        if (line.iter().all(u8::is_ascii_whitespace) || runtime_json_line(path, line))
            && !before.split_inclusive(|b| *b == b'\n').any(|old| old == line)
        {
            tail.extend_from_slice(line);
        }
    }
    tail
}

fn runtime_json_line(path: &std::path::Path, line: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else { return false };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if path.starts_with("transcripts") {
        serde_json::from_value::<crate::engine::transcript::Entry>(value).is_ok()
    } else if path.starts_with("checkpoints") && path.extension().is_some_and(|e| e == "jsonl") {
        serde_json::from_value::<crate::anvil::checkpoint::BackupEntry>(value).is_ok()
    } else if name.starts_with("run-") {
        serde_json::from_value::<crate::engine::events::RunEntry>(value).is_ok()
    } else if name.starts_with("engine-") {
        value.get("event").is_some_and(serde_json::Value::is_string)
    } else if name.starts_with("tape-") {
        ["tape", "event", "mark", "plan", "doc", "proof", "stats"].iter().any(|key| value.get(key).is_some())
    } else { false }
}

fn runtime_log(path: &std::path::Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if path.extension().is_some_and(|e| e == "jsonl") {
        let root_log = path.parent().is_none_or(|p| p.as_os_str().is_empty())
            && ["engine-", "run-", "tape-"].iter().any(|prefix| name.starts_with(prefix));
        let attempt_log = matches!(path.parent().and_then(std::path::Path::to_str), Some("transcripts" | "checkpoints"))
            && name.trim_end_matches(".jsonl").rsplit_once("-h")
                .is_some_and(|(id, heat)| !id.is_empty() && !heat.is_empty() && heat.bytes().all(|b| b.is_ascii_digit()));
        return root_log || attempt_log;
    }
    let hexadecimal = |name: &str| name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit());
    if path.starts_with("checkpoints/objects") { return hexadecimal(&name); }
    if path.starts_with("tool-results") { return hexadecimal(name.trim_end_matches(".txt")); }
    if path.starts_with("bg") { return name.starts_with("bg-") && name.ends_with(".log"); }
    name.ends_with(".log") && name.len() > 16 && name.as_bytes()[..8].iter().all(u8::is_ascii_digit)
        && ["_FLUX_", "_STRIKE_", "_ASSAY_", "_SURVEY_", "_FOUNDER_", "_RESMELT_"].iter().any(|part| name.contains(part))
}

/// Paint a palette color for the stream-mode `print!` lines in this crate.
/// `tui::fg` is private, so callers route through `tui::downgrade` here
/// rather than hardcoding escape sequences that drift from the palette.
/// Returns an empty string when color is off, so redirected output and
/// `NO_COLOR` runs carry no escape bytes at all.
pub(crate) fn fg(color: Color) -> String {
    if !tui::colored() {
        return String::new();
    }
    SetForegroundColor(tui::downgrade(color)).to_string()
}

/// Clears color and attributes, matching what `\x1b[0m` used to do.
pub(crate) fn reset() -> String {
    if !tui::colored() {
        return String::new();
    }
    ResetColor.to_string()
}

/// Bold, or nothing when color is off. Pairs with `reset`.
pub(crate) fn bold() -> String {
    if !tui::colored() {
        return String::new();
    }
    SetAttribute(Attribute::Bold).to_string()
}

/// Run the full 4-phase pipeline.
/// `temper_rounds` of 0 is the pre-warden behaviour exactly: forge, assay,
/// done, and nobody asks whether the commission was met.
pub async fn run(
    commission: Option<&str>,
    config: &EngineConfig,
    max_anvils: usize,
    hooks: EngineHooks,
    temper_rounds: usize,
) -> Result<(), SlagError> {
    tui::show_banner();

    let resumed_unverified = if commission.is_none() {
        unverified_goal_round(std::path::Path::new(crate::config::LEDGER))
    } else { None };
    let resumed_gap = if commission.is_none() {
        pending_goal_gap(std::path::Path::new(crate::config::LEDGER))
    } else {
        None
    };
    let temper_rounds = if resumed_unverified.is_some() {
        temper_rounds.max(1)
    } else if temper_rounds == 0 {
        resumed_gap.as_ref().map_or(0, |(round, _)| round.saturating_add(1))
    } else { temper_rounds };
    warden::set_rounds(temper_rounds);
    if let Some((round, gap)) = &resumed_gap {
        if *round >= temper_rounds {
            return Err(SlagError::GoalNotMet(format!(
                "{gap}; use --temper {} resume to forge the gap", round + 1
            )));
        }
    }

    let direct_candidate = commission.is_some_and(|c| c.chars().count() <= 800)
        && empty_project(std::path::Path::new("."))
        && temper_rounds > 0;

    let prior_head = commission.and_then(|_| review_head());
    // Fire furnace if needed
    let ore = fire_furnace(commission)?;
    let base_path = std::path::Path::new(crate::config::LOG_DIR).join("commission-base");
    if commission.is_some() {
        if let Some(base) = prior_head.or_else(review_head) {
            std::fs::create_dir_all(crate::config::LOG_DIR)?;
            std::fs::write(&base_path, base)?;
        }
    }
    warden::set_review_base(std::fs::read_to_string(base_path).ok());

    let direct = if direct_candidate && ore == Ore::Fired {
        let smith = crate::smith::make_scope_smith(config, &hooks);
        match surveyor::direct_proof(smith.as_ref(), commission.unwrap()).await? {
            Some(proof) => {
                found_direct(commission.unwrap(), &proof)?;
                tui::status_line("=", tui::COLD, "One local task, one final warden");
                true
            }
            None => false,
        }
    } else {
        false
    };

    // Item 36: a long unattended run should not die mid-ingot on an empty
    // account. Warn, never block: the balance endpoint is best-effort and
    // an unreachable one must not stop a forge.
    if let Some(floor) = crate::config::credit_floor() {
        if let Some(credits) =
            crate::engine::provider::fetch_credits(&config.api_key, &config.base_url).await
        {
            if credits.remaining() < floor {
                // Same rule as the addendum notice: an unguarded print
                // corrupts the dashboard, so this goes through the stream
                // and the stream renders it wherever the user is looking.
                crate::engine::emit(
                    &hooks.events,
                    crate::engine::EngineEvent::Warning {
                        message: format!(
                            "OpenRouter balance ${:.2} is under the ${floor:.2} floor",
                            credits.remaining()
                        ),
                    },
                );
            }
        }
    }

    // Phase 1: Survey. An extended ore re-surveys: the blueprint the
    // founder reads must describe the project the addendum asked for, not
    // the one before it.
    if ore == Ore::Extended || !std::path::Path::new(crate::config::BLUEPRINT).exists() {
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Surveyor);
        surveyor::run(smith.as_ref()).await?;
    }

    // A blueprint that cannot deliver the commission is a whole forge
    // wasted, and this is the cheapest moment to notice. Judged whether or
    // not the surveyor just ran: a thin blueprint left by an earlier run is
    // exactly the case worth catching, and gating the check on "we just
    // wrote this" made it unreachable in the common case. One corrective
    // pass, not a loop -- a surveyor that cannot cover the gap when told
    // exactly what it is will not cover it on an identical third try.
    if !direct && resumed_gap.is_none() && resumed_unverified.is_none() {
        if let Some(gap) = checkpoint(config, &hooks, "BLUEPRINT", crate::config::BLUEPRINT).await {
            let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Surveyor);
            surveyor::run_with_guidance(smith.as_ref(), Some(&gap)).await?;
        }
    }

    // Phase 2: Found
    let crucible_path = std::path::Path::new(crate::config::CRUCIBLE);
    let needs_founder = !crucible_path.exists() || {
        let content = std::fs::read_to_string(crucible_path).unwrap_or_default();
        !content.contains("(ingot ")
    };
    if needs_founder {
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Founder);
        founder::run(smith.as_ref()).await?;
        // Same question one level down: forged, do these ingots add up to
        // the commission? A missing capability costs nothing to fix here
        // and a full forge to fix later.
        if let Some(gap) = checkpoint(config, &hooks, "PLAN", crate::config::CRUCIBLE).await {
            founder::run_with_guidance(smith.as_ref(), Some(&gap)).await?;
        }
    } else if ore == Ore::Extended {
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Founder);
        let added = founder::extend(smith.as_ref()).await?;
        if added == 0 {
            // The model read the addendum and found nothing to build.
            // Saying so beats a silent finish that looks like a no-op.
            // It rides the event stream rather than stdout: a bare
            // `println!` under --tui writes into the alternate screen and
            // lands glued to whatever row is being drawn.
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Warning {
                    message: "the addendum cast no ingots — nothing in it needs building"
                        .to_string(),
                },
            );
        }
    }

    if let Some((_, gap)) = &resumed_gap {
        let crucible = Crucible::load(crucible_path)?;
        if !crucible.has_pending() && crucible.counts().cracked == 0 {
            fire_furnace(Some(gap))?;
            let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Founder);
            if founder::extend(smith.as_ref()).await? == 0 {
                return Err(SlagError::GoalNotMet(format!(
                    "{gap}; the founder cast no repair ingots"
                )));
            }
        }
    }

    // Phase 3: Forge, and Phase 4: Temper.
    //
    // The forge proves tasks; `:proof` answers "did this artifact appear".
    // Nothing proved the *goal*, and the summary that claimed it was written
    // by the smith that did the work. The warden closes that: a critic that
    // built nothing opens the real artifact, compares it with the bar, and
    // on a loss names one gap. The gap becomes an addendum, the addendum
    // casts ingots, and the forge runs again -- both halves of that loop
    // already existed.
    let mut forged;
    let mut round = resumed_gap.as_ref().map_or_else(
        || resumed_unverified.map_or(0, |round| round.min(temper_rounds.saturating_sub(1))),
        |(round, _)| *round,
    );
    // The warden's last word, carried past the assay. A run that prints
    // FORGED over an unmet goal launders the doubt it was asked to remove.
    let mut goal_gap: Option<String> = None;
    // Whether any warden ever returned a verdict. Armed-but-unjudged is a
    // third outcome, distinct from met and unmet, and the one most likely
    // to be misread as success.
    let mut goal_judged = false;
    let mut goal_unverified = None;
    loop {
        tui::header("FORGE");
        let crucible = Crucible::load(crucible_path)?;
        let counts = crucible.counts();
        if !tui::is_quiet() {
            print!("  ");
            tui::ingot_status_line(&counts);
            println!();
        }

        forged = forge::run(config, max_anvils, &hooks).await;

        round += 1;
        // A cracked or cancelled forge has no artifact worth judging, and
        // asking costs a model call to be told what the counts already say.
        if temper_rounds == 0 || round > temper_rounds || forged.is_err() {
            break;
        }

        let verdict = match warden::judge_artifact(config, &hooks, round).await {
            Ok(v) => v,
            // A provider that refuses the key or the account stops the run;
            // `main` reports it once, so no warning restates it here.
            Err(e) if e.is_deterministic_provider() || crate::error::provider_run_failure().is_some() => {
                return Err(e);
            }
            // A warden that cannot run must not fail the forge: the work is
            // built either way, and an unjudged run is worth more than a
            // lost one. Say so and stop tempering.
            Err(e) => {
                goal_unverified = Some(format!("the warden could not judge the goal: {e}; goal unverified"));
                crate::engine::emit(
                    &hooks.events,
                    crate::engine::EngineEvent::Warning {
                        message: format!("warden round {round}: {}", goal_unverified.as_ref().unwrap()),
                    },
                );
                break;
            }
        };
        crate::proof::git_commit("warden", &format!("record verdict for round {round}")).await;
        if verdict.is_unverified() {
            goal_unverified = Some(verdict.gap.clone());
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Warning {
                    message: format!("warden round {round}: {}", verdict.gap),
                },
            );
            break;
        }
        goal_judged = true;
        if verdict.fulfilled {
            goal_gap = None;
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Narrate {
                    text: format!("warden round {round}: goal met — {}", verdict.evidence),
                },
            );
            break;
        }
        goal_gap = Some(verdict.gap.clone());
        crate::engine::emit(
            &hooks.events,
            crate::engine::EngineEvent::Warning {
                message: format!("warden round {round}: {}", verdict.gap),
            },
        );
        if round >= temper_rounds {
            break;
        }
        // The gap becomes the next range of issues. `fire_furnace` appends
        // it as an addendum and `founder::extend` casts ingots for it --
        // the same path a human's second commission takes.
        let gap = verdict.gap.clone();
        if fire_furnace(Some(&gap))? != Ore::Extended {
            // The same gap twice means the last round changed nothing the
            // warden could see. Looping again would burn the budget on a
            // round already proven not to move.
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Warning {
                    message: "warden repeated its gap — stopping rather than re-forging it"
                        .to_string(),
                },
            );
            break;
        }
        let smith = crate::smith::make_plan_smith(config, &hooks, crate::engine::Role::Founder);
        if founder::extend(smith.as_ref()).await? == 0 {
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Warning {
                    message: "the gap cast no ingots — nothing left this loop can build"
                        .to_string(),
                },
            );
            break;
        }
    }

    // Phase 5: Assay. It runs on failure too — a run that cracked an ingot
    // is exactly when the user needs the counts and the cracked list, and
    // returning early left them with one line of "forge failed".
    if !matches!(forged, Err(SlagError::Cancelled)) {
        let _ = assay::show();
    }
    if let Some(reason) = goal_unverified {
        return Err(SlagError::SmithFailed(reason));
    }
    // The tasks can all pass while the commission goes unmet -- that gap is
    // the whole reason the warden exists, so it decides the run's verdict
    // and its exit code. A forge error still wins: a cracked ingot is the
    // more specific failure.
    // Asked to check the goal and unable to: say so. Reporting FORGED for
    // a run whose every check silently failed is worse than never checking,
    // because it converts an unknown into a claim.
    if temper_rounds > 0
        && !goal_judged
        && forged.is_ok()
        && crate::error::provider_run_failure().is_none()
    {
        crate::engine::emit(
            &hooks.events,
            crate::engine::EngineEvent::Warning {
                message: "goal UNVERIFIED: tempering was requested but no verdict was reached"
                    .to_string(),
            },
        );
        if !tui::is_quiet() {
            println!(
                "\n  {}! goal unverified — tempering ran but reached no verdict{}",
                fg(tui::WARM),
                reset()
            );
            println!("    the tasks passed; whether the commission did is unknown.\n");
        }
    }
    if let (Some(gap), Ok(())) = (goal_gap, &forged) {
        tui::header("GOAL NOT MET");
        if !tui::is_quiet() {
            println!("  {}{gap}{}\n", fg(tui::WARM), reset());
            println!("  the tasks passed; the commission did not.");
            println!("  raise --temper to forge the gap, or read {}.\n", crate::config::LEDGER);
        }
        return Err(SlagError::GoalNotMet(gap));
    }
    forged
}

fn review_head() -> Option<String> {
    let output = crate::engine::sandbox::git().args(["rev-parse", "HEAD"]).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn unverified_goal_round(path: &std::path::Path) -> Option<usize> {
    let ledger = std::fs::read_to_string(path).ok()?;
    let section = ledger.rsplit("## Warden round ").next()?;
    let header = section.lines().next()?;
    if !header.trim_end().ends_with("goal unverified") { return None; }
    header.split_whitespace().next()?.trim_end_matches(':').parse().ok()
}

fn pending_goal_gap(path: &std::path::Path) -> Option<(usize, String)> {
    let ledger = std::fs::read_to_string(path).ok()?;
    let section = ledger.rsplit("## Warden round ").next()?;
    let (header, body) = section.split_once('\n')?;
    if !header.trim_end().ends_with("goal not met") { return None; }
    let round = header.split_whitespace().next()?.trim_end_matches(':').parse().ok()?;
    let gap = body.lines().find_map(|line| line.strip_prefix("- gap: "))?.trim();
    if gap.is_empty() || gap == "—" || gap.contains("goal unverified")
        || gap.contains("no readable verdict") { return None; }
    Some((round, gap.to_string()))
}

fn empty_project(root: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else { return false };
    entries.filter_map(Result::ok).all(|entry| {
        matches!(entry.file_name().to_str(), Some(".git" | ".gitignore" | "logs"))
    })
}

fn found_direct(commission: &str, proof: &str) -> Result<(), SlagError> {
    std::fs::write(crate::config::BLUEPRINT, format!(
        "# Local utility\n\n{commission}\n\nVerification: `{proof}`\n"
    ))?;
    std::fs::write(warden::BAR_FILE, format!(
        "## THE BAR\nMeet the original commission.\n\n## CHECKLIST\n\
         - [ ] {commission}\n\n## HOW TO INSPECT\n\
         Read the product files and test source. Read logs/*_ASSAY_*.log for coordinator-run test results.\n\
         Use read-only behavior probes. Do not run tests that create temporary files or fixtures.\n\
         Judge the commission without extra naming, style, or test-count rules.\n"
    ))?;
    let mut ingot = crate::sexp::parser::parse_ingot("(ingot :id \"i1\")").unwrap();
    ingot.solo = false;
    ingot.grade = 1;
    ingot.max = 3;
    ingot.casts = Some(1);
    ingot.bar.clear();
    ingot.proof = proof.to_string();
    ingot.work = commission.to_string();
    Crucible::new(std::path::Path::new(crate::config::CRUCIBLE), vec![ingot]).save()?;
    Ok(())
}

/// Initialize project structure (fire the furnace)
/// Run a pre-forge goal check, returning the gap when the document does
/// not cover the bar. `None` means it passed, the check is disarmed, or the
/// warden could not run -- all three mean "carry on", because a check that
/// exists to save a wasted forge must never cause one.
async fn checkpoint(
    config: &EngineConfig,
    hooks: &EngineHooks,
    kind: &str,
    path: &str,
) -> Option<String> {
    if !warden::armed() {
        return None;
    }
    let verdict = match warden::judge_plan_doc(config, hooks, kind, path).await {
        Ok(v) => v,
        // A provider refusal is the run's error, reported once by `main`;
        // the next smith short-circuits on it.
        Err(e) if e.is_deterministic_provider() || crate::error::provider_run_failure().is_some() => {
            return None;
        }
        // Never silent. A checkpoint that cannot run used to disappear,
        // and a run with every check disabled still printed FORGED --
        // which is the laundering this whole mechanism exists to stop.
        Err(e) => {
            crate::engine::emit(
                &hooks.events,
                crate::engine::EngineEvent::Warning {
                    message: format!("the {kind} could not be judged: {e}"),
                },
            );
            return None;
        }
    };
    if verdict.fulfilled {
        return None;
    }
    if verdict.is_unverified() {
        crate::engine::emit(
            &hooks.events,
            crate::engine::EngineEvent::Warning {
                message: format!("warden {kind}: {}", verdict.gap),
            },
        );
        return None;
    }
    crate::engine::emit(
        &hooks.events,
        crate::engine::EngineEvent::Warning {
            message: format!("warden rejected the {kind}: {}", verdict.gap),
        },
    );
    Some(verdict.gap)
}

/// What `fire_furnace` did with the commission it was handed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ore {
    /// The project already existed and no new commission came with it.
    Unchanged,
    /// A fresh project: PRD.md written from the commission.
    Fired,
    /// A live project got a second commission, appended to the ore. The
    /// blueprint and the crucible both need to catch up.
    Extended,
}

fn fire_furnace(commission: Option<&str>) -> Result<Ore, SlagError> {
    let ore_path = std::path::Path::new(crate::config::ORE_FILE);

    if ore_path.exists() {
        // A commission for a project that already has ore used to be
        // dropped on the floor: no error, no warning, and a run that
        // looked like it did nothing. Append it instead, and let the
        // survey and the founder work out what it adds.
        let Some(commission) = commission.filter(|c| !c.trim().is_empty()) else {
            return Ok(Ore::Unchanged);
        };
        let existing = std::fs::read_to_string(ore_path).unwrap_or_default();
        if existing.contains(commission.trim()) {
            // Re-running the same commission is not a new request; it is
            // usually a repeated command. Say so rather than re-planning.
            return Ok(Ore::Unchanged);
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(ore_path)?;
        writeln!(
            f,
            "\n## Addendum — {}\n\n{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M"),
            commission.trim()
        )?;
        tui::status_line("+", tui::HOT, "Ore extended");
        return Ok(Ore::Extended);
    }

    let commission = commission.ok_or(SlagError::NoOre)?;

    tui::header("FIRING FURNACE");

    // git init
    let _ = crate::engine::sandbox::git()
        .args(["init", "-b", "main"])
        .output();

    // .gitignore
    let gitignore = std::path::Path::new(".gitignore");
    let content = std::fs::read_to_string(gitignore).unwrap_or_default();
    if !content.contains("logs/") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(gitignore)?;
        use std::io::Write;
        writeln!(f, "logs/")?;
    }

    // Create PRD.md
    std::fs::write(
        ore_path,
        format!("# Commission\n\n{commission}\n"),
    )?;
    tui::status_line("░", tui::COLD, "Ore loaded");

    // Create AGENTS.md
    let alloy_path = std::path::Path::new(crate::config::ALLOY_FILE);
    if !alloy_path.exists() {
        std::fs::write(alloy_path, "## Alloy Recipes\n")?;
        tui::status_line("+", tui::COLD, "Recipes ready");
    }

    // Create PROGRESS.md
    let ledger_path = std::path::Path::new(crate::config::LEDGER);
    if !ledger_path.exists() {
        std::fs::write(
            ledger_path,
            format!("# Smithy Ledger\nFired: {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M")),
        )?;
        tui::status_line("+", tui::COLD, "Ledger open");
    }

    // Create logs dir
    std::fs::create_dir_all(crate::config::LOG_DIR)?;

    // Initial commit, scoped to the files slag just wrote. `git add -A`
    // here swept whatever the user had lying around — including .env —
    // into a commit they never asked for.
    let _ = crate::engine::sandbox::git()
        .args([
            "add",
            "--",
            ".gitignore",
            crate::config::ORE_FILE,
            crate::config::ALLOY_FILE,
            crate::config::LEDGER,
        ])
        .output();
    let _ = crate::engine::sandbox::git()
        .args(["commit", "-m", "fire: furnace lit", "--quiet"])
        .output();

    tui::status_line("█", tui::HOT, "Furnace hot");
    Ok(Ore::Fired)
}

#[cfg(test)]
mod coordinator_tests {
    use super::*;

    #[test]
    fn saved_goal_gaps_accept_both_ledger_headers_and_ignore_unverified_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PROGRESS.md");
        for header in ["## Warden round 1 — goal not met", "## Warden round 1: goal not met"] {
            std::fs::write(&path, format!("{header}\n\n- gap: PEP 668 install fails\n- evidence: pip exit 1\n")).unwrap();
            assert_eq!(pending_goal_gap(&path), Some((1, "PEP 668 install fails".into())));
        }
        for outcome in ["goal met", "goal unverified"] {
            std::fs::write(&path, format!("## Warden round 2: {outcome}\n\n- gap: no readable verdict\n")).unwrap();
            assert!(pending_goal_gap(&path).is_none());
        }
    }

    #[test]
    fn restore_preserves_state_and_removes_model_created_documents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("PLAN.md"), "forged i1\n").unwrap();
        std::fs::write(dir.path().join("PROGRESS.md"), "i1 passed\n").unwrap();
        let guard = CoordinatorGuard::capture(dir.path()).unwrap();
        std::fs::write(dir.path().join("PLAN.md"), "ore i1\n").unwrap();
        std::fs::remove_file(dir.path().join("PROGRESS.md")).unwrap();
        std::fs::write(dir.path().join("BAR.md"), "invented rule\n").unwrap();
        std::fs::write(dir.path().join("utility.py"), "print(1)\n").unwrap();
        assert_eq!(guard.restore().unwrap().len(), 3);
        assert_eq!(std::fs::read_to_string(dir.path().join("PLAN.md")).unwrap(), "forged i1\n");
        assert_eq!(std::fs::read_to_string(dir.path().join("PROGRESS.md")).unwrap(), "i1 passed\n");
        assert!(!dir.path().join("BAR.md").exists());
        assert!(dir.path().join("utility.py").exists());
        assert!(guard.restore().unwrap().is_empty());
    }

    #[test]
    fn restore_handles_replaced_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("PLAN.md"), "forged\n").unwrap();
        let guard = CoordinatorGuard::capture(dir.path()).unwrap();
        std::fs::remove_file(dir.path().join("PLAN.md")).unwrap();
        std::fs::create_dir(dir.path().join("PLAN.md")).unwrap();
        assert_eq!(guard.restore().unwrap(), ["PLAN.md"]);
        assert_eq!(std::fs::read_to_string(dir.path().join("PLAN.md")).unwrap(), "forged\n");
    }

    #[test]
    fn recovery_logs_restore_unlinked_sinks_and_keep_valid_appends() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(logs.join("transcripts")).unwrap();
        let engine = logs.join("engine-20261017_120000.jsonl");
        let transcript = logs.join("transcripts/i1-h1.jsonl");
        let first = b"{\"event\":\"turn_start\",\"turn\":1}\n";
        let next = b"{\"event\":\"turn_start\",\"turn\":2}\n";
        std::fs::write(&engine, first).unwrap();
        std::fs::write(&transcript, b"{\"entry\":\"resume_attempt\"}\n").unwrap();
        let mut sink = std::fs::OpenOptions::new().append(true).open(&engine).unwrap();
        let guard = CoordinatorGuard::capture(dir.path()).unwrap();
        std::fs::remove_file(&engine).unwrap();
        std::fs::write(&engine, "model replacement\n").unwrap();
        sink.write_all(next).unwrap();
        std::fs::write(&transcript, "model truncation\n").unwrap();
        std::fs::OpenOptions::new().append(true).open(&transcript).unwrap()
            .write_all(b"{\"entry\":\"end\",\"ok\":true}\n").unwrap();
        std::fs::write(logs.join("marker"), "model marker").unwrap();
        std::fs::write(logs.join("marker.jsonl"), "").unwrap();
        std::fs::create_dir_all(logs.join("checkpoints/objects")).unwrap();
        std::fs::write(logs.join("checkpoints/objects/0123456789abcdef"), "object").unwrap();
        let changed = guard.restore().unwrap();
        assert!(changed.iter().any(|path| path == "logs/marker"));
        assert_eq!(std::fs::read(&engine).unwrap(), [first.as_slice(), next.as_slice()].concat());
        assert_eq!(std::fs::read_to_string(&transcript).unwrap(),
            "{\"entry\":\"resume_attempt\"}\n{\"entry\":\"end\",\"ok\":true}\n");
        assert!(!logs.join("marker").exists());
        assert!(!logs.join("marker.jsonl").exists());
        assert!(logs.join("checkpoints/objects/0123456789abcdef").exists());
        sink.write_all(next).unwrap();
        assert_eq!(std::fs::read(&engine).unwrap(), [first.as_slice(), next.as_slice(), next.as_slice()].concat());
        assert!(guard.restore().unwrap().is_empty());
    }

    #[test]
    fn coordinator_guard_preserves_ledger_appends_and_repairs_on_drop() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("PROGRESS.md");
        std::fs::write(&ledger, "original\n").unwrap();
        let backup;
        {
            let guard = CoordinatorGuard::capture(dir.path()).unwrap();
            backup = guard.backup.clone();
            std::fs::OpenOptions::new().append(true).open(&ledger).unwrap()
                .write_all(b"duel result\n").unwrap();
            assert!(guard.restore().unwrap().is_empty());
            std::fs::write(dir.path().join("PLAN.md"), "stale status").unwrap();
        }
        assert_eq!(std::fs::read_to_string(ledger).unwrap(), "original\nduel result\n");
        assert!(!dir.path().join("PLAN.md").exists());
        assert!(!backup.exists());
    }

    #[cfg(unix)]
    #[test]
    fn coordinator_guard_keeps_an_unchanged_document_symlink() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.md"), "project rules").unwrap();
        std::os::unix::fs::symlink("source.md", dir.path().join("AGENTS.md")).unwrap();
        let guard = CoordinatorGuard::capture(dir.path()).unwrap();
        assert!(guard.restore().unwrap().is_empty());
        assert_eq!(std::fs::read_link(dir.path().join("AGENTS.md")).unwrap(), std::path::Path::new("source.md"));
    }

    #[test]
    fn only_empty_projects_qualify_for_direct_scope() {
        let dir = tempfile::tempdir().unwrap();
        assert!(empty_project(dir.path()));
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join("logs")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "logs/\n").unwrap();
        assert!(empty_project(dir.path()));
        std::fs::write(dir.path().join("existing.py"), "print(1)\n").unwrap();
        assert!(!empty_project(dir.path()));
    }
}

#[cfg(test)]
mod ore_tests {
    use super::*;

    fn in_dir<T>(f: impl FnOnce() -> T) -> T {
        // fire_furnace works on relative paths, so each case needs its own
        // directory. The lock keeps concurrent tests from sharing a cwd.
        static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prior = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let out = f();
        std::env::set_current_dir(prior).unwrap();
        out
    }

    #[test]
    fn a_commission_for_a_live_project_extends_the_ore() {
        // The bug: this returned Ok and dropped the commission, so the run
        // surveyed nothing, founded nothing, and looked like a no-op.
        in_dir(|| {
            std::fs::write(crate::config::ORE_FILE, "# Commission\n\nBuild a shooter.\n").unwrap();
            let ore = fire_furnace(Some("add a leaderboard")).unwrap();
            assert_eq!(ore, Ore::Extended);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert!(prd.contains("Build a shooter."), "the original ore survives: {prd}");
            assert!(prd.contains("## Addendum"), "{prd}");
            assert!(prd.contains("add a leaderboard"), "{prd}");
        });
    }

    #[test]
    fn no_commission_on_a_live_project_changes_nothing() {
        in_dir(|| {
            std::fs::write(crate::config::ORE_FILE, "# Commission\n\nBuild a shooter.\n").unwrap();
            assert_eq!(fire_furnace(None).unwrap(), Ore::Unchanged);
            assert_eq!(fire_furnace(Some("   ")).unwrap(), Ore::Unchanged);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert!(!prd.contains("Addendum"), "an empty commission is not a request: {prd}");
        });
    }

    #[test]
    fn re_running_the_same_commission_is_not_a_new_request() {
        // Pressing Up+Enter, or re-running a shell command, must not stack
        // duplicate addenda and re-plan the same work.
        in_dir(|| {
            std::fs::write(crate::config::ORE_FILE, "# Commission\n\nBuild a shooter.\n").unwrap();
            assert_eq!(fire_furnace(Some("add a leaderboard")).unwrap(), Ore::Extended);
            assert_eq!(fire_furnace(Some("add a leaderboard")).unwrap(), Ore::Unchanged);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert_eq!(prd.matches("add a leaderboard").count(), 1, "{prd}");
        });
    }

    #[test]
    fn a_cold_directory_still_fires_the_furnace() {
        in_dir(|| {
            assert_eq!(fire_furnace(Some("Build a shooter")).unwrap(), Ore::Fired);
            let prd = std::fs::read_to_string(crate::config::ORE_FILE).unwrap();
            assert!(prd.contains("Build a shooter"), "{prd}");
            assert!(!prd.contains("Addendum"), "a first commission is the ore, not an addendum");
        });
    }

    #[test]
    fn a_cold_directory_with_no_commission_still_asks_for_one() {
        in_dir(|| {
            assert!(matches!(fire_furnace(None), Err(SlagError::NoOre)));
        });
    }
}
