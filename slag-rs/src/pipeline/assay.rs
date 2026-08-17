use std::path::Path;

use crate::config::CRUCIBLE;
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::sexp::Status;
use crate::tui;

/// Phase 4: Final report
pub fn show() -> Result<(), SlagError> {
    // While the dashboard owns the terminal (raw mode + alternate
    // screen) raw prints corrupt the panes and the report is lost with
    // the screen buffer; main re-runs this after the dashboard exits.
    if tui::is_quiet() {
        return Ok(());
    }
    let crucible = Crucible::load(Path::new(CRUCIBLE))?;
    let counts = crucible.counts();

    tui::header("ASSAY");

    print!(
        "  {}{}{}{} ingots  {}{}{}{} forged",
        super::bold(),
        super::fg(tui::PURE),
        counts.total,
        super::reset(),
        super::bold(),
        super::fg(tui::PURE),
        counts.forged,
        super::reset(),
    );
    if counts.cracked > 0 {
        print!("  {}{}{} cracked", super::fg(tui::WARM), counts.cracked, super::reset());
    }
    println!();

    tui::temper_bar(&counts);

    if counts.cracked > 0 {
        println!("\n  {}Cracked:{}", super::fg(tui::WARM), super::reset());
        for ingot in &crucible.ingots {
            if ingot.status == Status::Cracked {
                println!(
                    "    {}✗{} [{}] {}",
                    super::fg(tui::WARM),
                    super::reset(),
                    ingot.id,
                    ingot.work
                );
            }
        }
    }

    println!(
        "\n  {}blueprint: {}{}",
        super::fg(tui::COLD),
        crate::config::BLUEPRINT,
        super::reset()
    );
    println!(
        "  {}crucible:  {}{}",
        super::fg(tui::COLD),
        crate::config::CRUCIBLE,
        super::reset()
    );
    println!(
        "  {}slag heap: {}{}",
        super::fg(tui::COLD),
        crate::config::LOG_DIR,
        super::reset()
    );

    if counts.cracked > 0 {
        println!(
            "\n  {}{}✗ CRACKED{}\n",
            super::fg(tui::WARM),
            super::bold(),
            super::reset()
        );
    } else {
        println!(
            "\n  {}{}█ FORGED{}\n",
            super::bold(),
            super::fg(tui::PURE),
            super::reset()
        );
    }

    Ok(())
}
