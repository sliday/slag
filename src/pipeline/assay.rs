use std::path::Path;

use crate::config::CRUCIBLE;
use crate::crucible::Crucible;
use crate::error::SlagError;
use crate::sexp::Status;
use crate::tui;

/// Phase 4: Final report
pub fn show(elapsed_secs: Option<u64>) -> Result<(), SlagError> {
    let crucible = Crucible::load(Path::new(CRUCIBLE))?;
    let counts = crucible.counts();

    tui::header("ASSAY");

    print!(
        "  \x1b[1;37m{}\x1b[0m ingots  \x1b[1;37m{}\x1b[0m forged",
        counts.total, counts.forged,
    );
    if counts.cracked > 0 {
        print!("  \x1b[31m{}\x1b[0m cracked", counts.cracked);
    }
    if let Some(secs) = elapsed_secs {
        print!("  \x1b[90m⏱ {}\x1b[0m", tui::format_elapsed(secs));
    }
    println!();

    tui::temper_bar(&counts);

    if counts.cracked > 0 {
        println!("\n  \x1b[31mCracked:\x1b[0m");
        for ingot in &crucible.ingots {
            if ingot.status == Status::Cracked {
                println!("    \x1b[31m✗\x1b[0m [{}] {}", ingot.id, ingot.work);
            }
        }
    }

    println!("\n  \x1b[90mblueprint: {}\x1b[0m", crate::config::BLUEPRINT);
    println!("  \x1b[90mcrucible:  {}\x1b[0m", crate::config::CRUCIBLE);
    println!("  \x1b[90mslag heap: {}\x1b[0m", crate::config::LOG_DIR);

    if counts.cracked > 0 {
        println!("\n  \x1b[31m\x1b[1m✗ CRACKED\x1b[0m\n");
    } else {
        println!("\n  \x1b[1;37m\x1b[1m█ FORGED\x1b[0m\n");
    }

    Ok(())
}
