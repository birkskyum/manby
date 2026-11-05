//! Port of json_logger.cpp.

use std::sync::atomic::{AtomicBool, Ordering};

pub struct JsonLogger {
    pub json_enabled: AtomicBool,
}

impl JsonLogger {
    pub const fn new() -> JsonLogger {
        JsonLogger {
            json_enabled: AtomicBool::new(false),
        }
    }

    pub fn json_enabled(&self) -> bool {
        self.json_enabled.load(Ordering::Relaxed)
    }

    pub fn progress_tile(&self, progress: f64) {
        eprintln!("{{\"progress\":{:3.1}}}", progress);
    }
}

impl Default for JsonLogger {
    fn default() -> Self {
        JsonLogger::new()
    }
}
