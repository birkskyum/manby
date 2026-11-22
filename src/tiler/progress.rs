//! tippecanoe's progress indicator: `  42.3%  z/x/y  \r` on standard error
//! as the tiles are made, or `{"progress":42.3}` lines with `-u`.

use std::io::Write;
use std::time::{Duration, Instant};

/// How the tiling reports how far it has got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Progress {
    /// Nothing, as with `-q` or `-Q`, and by default from the library.
    #[default]
    Off,
    /// tippecanoe's line, rewritten in place: `  42.3%  12/2048/1361  \r`.
    Line,
    /// `-u`: a JSON object per line, `{"progress":42.3}`.
    Json,
}

/// Writes the progress as it rises, by tenths of a percent, at most once per
/// `-U` interval.
pub(super) struct Reporter {
    style: Progress,
    interval: Duration,
    last: Option<Instant>,
    shown: f64,
    wrote_line: bool,
}

impl Reporter {
    pub fn new(style: Progress, interval: f64) -> Reporter {
        Reporter {
            style,
            interval: Duration::try_from_secs_f64(interval.max(0.0)).unwrap_or(Duration::MAX),
            last: None,
            shown: 0.0,
            wrote_line: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.style != Progress::Off
    }

    /// Reports `progress`, a share from 0 to 1, having just made tile
    /// `z/x/y`. It is shown floored to a tenth of a percent, as tippecanoe
    /// shows it, and only when that has risen.
    pub fn report(&mut self, progress: f64, z: i32, x: u32, y: u32) {
        if !self.enabled() {
            return;
        }
        let percent = (progress.clamp(0.0, 1.0) * 1000.0).floor() / 10.0;
        if percent < self.shown + 0.1 {
            return;
        }
        if !self.interval.is_zero() {
            let now = Instant::now();
            if self
                .last
                .is_some_and(|t| now.duration_since(t) < self.interval)
            {
                return;
            }
            self.last = Some(now);
        }
        self.shown = percent;
        let mut err = std::io::stderr().lock();
        let _ = match self.style {
            Progress::Line => {
                self.wrote_line = true;
                write!(err, "  {:3.1}%  {}/{}/{}  \r", percent, z, x, y)
            }
            Progress::Json => writeln!(err, "{{\"progress\":{:3.1}}}", percent),
            Progress::Off => Ok(()),
        };
        let _ = err.flush();
    }

    /// Blanks the line once the tiling is over, so that what is printed next
    /// starts on a clean line.
    pub fn finish(&mut self) {
        if self.wrote_line {
            eprint!("{:40}\r", "");
            self.wrote_line = false;
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_by_tenths() {
        let mut r = Reporter::new(Progress::Json, 0.0);
        r.report(0.1234, 3, 1, 2);
        assert_eq!(r.shown, 12.3);
        // the same tenth, or less, is not shown again
        r.report(0.1239, 3, 1, 2);
        r.report(0.05, 3, 1, 2);
        assert_eq!(r.shown, 12.3);
        r.report(2.0, 3, 1, 2);
        assert_eq!(r.shown, 100.0);
    }

    #[test]
    fn interval() {
        let mut r = Reporter::new(Progress::Json, 3600.0);
        r.report(0.1, 0, 0, 0);
        r.report(0.5, 0, 0, 0);
        assert_eq!(r.shown, 10.0);
    }

    #[test]
    fn off() {
        let mut r = Reporter::new(Progress::Off, 0.0);
        r.report(0.5, 0, 0, 0);
        assert_eq!(r.shown, 0.0);
        assert!(!r.enabled());
    }
}
