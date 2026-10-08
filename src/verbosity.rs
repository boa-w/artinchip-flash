use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable (`true`) or disable verbose transport-level logging.
///
/// CLI `--verbose` and GUI `config.verbose` both funnel through here so the
/// library never decides on its own what reaches stderr.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::SeqCst);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::SeqCst)
}

/// Low-level transport chatter (CBW/CSW bytes, UART framing, reconnect polls).
/// Gated behind [`verbose`] so GUI logs and default CLI output stay clean.
#[macro_export]
macro_rules! log_verbose {
    ($($arg:tt)*) => {
        if $crate::verbosity::verbose() {
            eprintln!($($arg)*);
        }
    };
}
