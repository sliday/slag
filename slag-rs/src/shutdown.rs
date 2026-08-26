//! Central cleanup registry.
//!
//! Every exit path a forge can take — a clean return, Ctrl-C at the
//! shell, a panic inside a draw call — has to run the same short list of
//! chores: flush the JSONL event sink, save the crucible under its lock,
//! put the terminal back. Scattering those across `main`, the dashboard,
//! and the pipeline guarantees one path forgets one of them, and the
//! path that forgets is always the panic (it is the one nobody tests).
//!
//! So cleanups register here as boxed closures and run from exactly two
//! triggers: the Ctrl-C handler and the panic hook, both installed once
//! from `main`. Registration order is honored in reverse — last in, first
//! out — because a cleanup registered later usually depends on state a
//! cleanup registered earlier still owns (the terminal is claimed last
//! and must be released first, or the crucible's error message prints
//! into the alternate screen and dies with it).
//!
//! Two properties the callers depend on:
//!
//! * **Idempotent.** `run_all` drains the registry, so a Ctrl-C arriving
//!   while a panic unwinds does not double-save.
//! * **Panic-proof.** Each cleanup runs inside `catch_unwind`; one that
//!   panics is skipped and the rest still run. A cleanup that takes the
//!   process down with it defeats the entire point.

use std::sync::{Mutex, OnceLock};

/// A registered cleanup. `Send` because the panic hook can fire on any
/// thread; `'static` because the registry outlives every caller.
type Cleanup = Box<dyn FnOnce() + Send + 'static>;

fn registry() -> &'static Mutex<Vec<Cleanup>> {
    static REGISTRY: OnceLock<Mutex<Vec<Cleanup>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a cleanup to run on shutdown. Runs at most once: `run_all`
/// takes ownership of the whole list.
pub fn register<F: FnOnce() + Send + 'static>(f: F) {
    // A poisoned registry means some *other* cleanup panicked. Recover
    // the guard rather than propagating: dropping the remaining cleanups
    // because one of them was buggy is exactly the failure this module
    // exists to prevent.
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.push(Box::new(f));
}

/// Run every registered cleanup, most-recent first, and clear the
/// registry. Returns how many ran, so a caller can log "nothing to do"
/// distinctly from "the registry was already drained".
pub fn run_all() -> usize {
    let taken: Vec<Cleanup> = {
        let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *reg)
    };
    let n = taken.len();
    for cleanup in taken.into_iter().rev() {
        // A panicking cleanup must not abort the others. `AssertUnwindSafe`
        // is honest here: the closure is consumed either way, so there is
        // no shared state left observably half-updated afterwards.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
    }
    n
}

/// How many cleanups are currently registered.
#[cfg(test)]
pub fn pending() -> usize {
    registry().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Register the crucible rescue: on an abrupt exit, reload the crucible
/// from disk, hand any Molten ingot back to Ore, and save. Molten means
/// "an anvil owns this", and after a panic or a Ctrl-C no anvil owns
/// anything — leaving the status behind makes the run look busy forever
/// and strands the work `slag resume` would otherwise pick up.
///
/// Skips silently when `CRUCIBLE_LOCK` is held: the holder is mid-save
/// under the lock this cleanup cannot await from a sync hook, and its
/// write is fresher than anything read here. Skips when nothing is
/// molten, so a clean finish never rewrites the file.
pub fn register_crucible_rescue() {
    register(|| {
        let path = std::path::Path::new(crate::config::CRUCIBLE);
        if !path.exists() {
            return;
        }
        let Ok(_guard) = crate::crucible::CRUCIBLE_LOCK.try_lock() else {
            return;
        };
        let Ok(mut crucible) = crate::crucible::Crucible::load(path) else {
            return;
        };
        if crucible.reset_stale_molten() > 0 {
            let _ = crucible.save();
        }
    });
}

/// Install the panic hook. Cleanups run *before* the previous hook, so
/// the terminal is out of raw mode and off the alternate screen by the
/// time a backtrace prints — a backtrace rendered into an alternate
/// screen that then goes away is a backtrace nobody reads.
pub fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            run_all();
            previous(info);
        }));
    });
}

/// Install the Ctrl-C handler: run the cleanups, then leave. Spawned on
/// the tokio runtime, so it needs one to already be running.
///
/// The forge's *own* Ctrl-C (the dashboard's cancel key) never reaches
/// here — this is the shell-level signal, the one that arrives while the
/// dashboard is not up, or a second time when the first cancel did not
/// take.
pub fn install_signal_handler() {
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            run_all();
            std::process::exit(130); // 128 + SIGINT, what a shell expects.
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // The registry is process-global, so these tests share it. They run
    // under one lock to keep a concurrent `run_all` from draining another
    // test's registrations.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn cleanups_run_in_reverse_registration_order() {
        let _g = guard();
        run_all();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for tag in ["sink", "crucible", "terminal"] {
            let seen = seen.clone();
            register(move || seen.lock().unwrap().push(tag));
        }
        assert_eq!(run_all(), 3);
        // Terminal was claimed last, so it is released first.
        assert_eq!(*seen.lock().unwrap(), vec!["terminal", "crucible", "sink"]);
    }

    #[test]
    fn draining_is_idempotent() {
        let _g = guard();
        run_all();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        register(move || {
            h.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(run_all(), 1);
        // A Ctrl-C landing while a panic unwinds must not double-save.
        assert_eq!(run_all(), 0);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panicking_cleanup_does_not_strand_the_others() {
        let _g = guard();
        run_all();
        let survived = Arc::new(AtomicUsize::new(0));
        let a = survived.clone();
        let b = survived.clone();
        register(move || {
            a.fetch_add(1, Ordering::SeqCst);
        });
        register(|| panic!("a buggy cleanup"));
        register(move || {
            b.fetch_add(1, Ordering::SeqCst);
        });

        // Silence the panic backtrace this test deliberately provokes.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let ran = run_all();
        std::panic::set_hook(hook);

        assert_eq!(ran, 3);
        assert_eq!(survived.load(Ordering::SeqCst), 2, "both good cleanups ran");
    }

    #[test]
    fn pending_reports_registry_depth() {
        let _g = guard();
        run_all();
        assert_eq!(pending(), 0);
        register(|| {});
        register(|| {});
        assert_eq!(pending(), 2);
        run_all();
        assert_eq!(pending(), 0);
    }
}
