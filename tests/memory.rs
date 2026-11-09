//! Memory for each tile. A zoom level keeps a little for every tile until
//! the level is written (DESIGN.md), and at the high zooms of a large polygon
//! that is most of what manby holds (#32).

use std::io::Write;
use std::process::Command;

/// Runs the command and returns its peak resident memory in bytes.
fn peak_memory(command: &mut Command) -> u64 {
    let pid = command.spawn().unwrap().id() as libc::pid_t;
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::wait4(pid, &mut status, 0, &mut usage) }, pid);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    // macOS counts bytes and Linux kilobytes
    let unit = if cfg!(target_os = "macos") { 1 } else { 1024 };
    usage.ru_maxrss as u64 * unit
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow without --release")]
fn tiles_of_a_large_polygon_stay_small_in_memory() {
    let dir = std::env::temp_dir().join(format!("manby-memory-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("square.geojson");
    std::fs::write(
        &input,
        r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[-15,-15],[15,-15],[15,15],[-15,15],[-15,-15]]]}}"#,
    )
    .unwrap();
    let output = dir.join("square.pmtiles");

    // 631,789 tiles, 473,328 of them at z13
    let peak = peak_memory(
        Command::new(env!("CARGO_BIN_EXE_manby"))
            .args(["-q", "-f", "-z13", "--threads=4", "-o"])
            .arg(&output)
            .arg(&input),
    );
    let megabytes = peak >> 20;
    // not captured, so that every run of the tests shows it
    writeln!(
        std::io::stderr(),
        "peak memory for 631,789 tiles of one polygon: {megabytes} MB"
    )
    .unwrap();
    std::fs::remove_dir_all(&dir).ok();

    // About 95 MB. It was 310 MB when each tile kept about 500 bytes.
    assert!(megabytes < 200, "{megabytes} MB");
}
