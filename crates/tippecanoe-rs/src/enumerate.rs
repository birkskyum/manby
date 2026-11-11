//! Port of enumerate.cpp, the `tippecanoe-enumerate` program.

use std::ffi::CStr;
use std::io::Write;

use rusqlite::{Connection, OpenFlags, ffi};

use crate::errors::{EXIT_ARGS, EXIT_CLOSE, EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_SQLITE};
use crate::getopt::{Getopt, LongOption};
use crate::usage::{getopt_string, print_usage};

fn enumerate(fname: &str, out: &mut dyn Write) {
    let db = match sqlite3_open(fname) {
        Ok(db) => db,
        Err(msg) => {
            eprintln!("{}: {}", fname, msg);
            std::process::exit(EXIT_OPEN);
        }
    };

    if let Err(e) = db.execute_batch("PRAGMA integrity_check;") {
        eprintln!("{}: integrity_check: {}", fname, e);
        std::process::exit(EXIT_SQLITE);
    }

    let sql = c"SELECT zoom_level, tile_column, tile_row from tiles order by zoom_level, tile_column, tile_row;";

    {
        let mut stmt = match Stmt::prepare(&db, sql) {
            Ok(stmt) => stmt,
            Err(msg) => {
                eprintln!("{}: select failed: {}", fname, msg);
                std::process::exit(EXIT_SQLITE);
            }
        };

        while stmt.step() == ffi::SQLITE_ROW {
            let zoom = stmt.column_int(0) as i64;
            let x = stmt.column_int(1) as i64;
            let mut y = stmt.column_int(2) as i64;

            if !(0..=31).contains(&zoom) {
                eprintln!("Corrupt mbtiles file: impossible zoom level {}", zoom);
                std::process::exit(EXIT_IMPOSSIBLE);
            }

            y = (1i64 << zoom) - 1 - y;
            let _ = writeln!(out, "{} {} {} {}", fname, zoom, x, y);
        }
    }

    if let Err((_, e)) = db.close() {
        eprintln!("{}: could not close database: {}", fname, e);
        std::process::exit(EXIT_CLOSE);
    }
}

// there are no options, but the table is still what the usage message
// and the getopt string are derived from, so that they will keep up
// with any options that are added later
static LONG_OPTIONS: [LongOption; 0] = [];

fn usage(argv: &[String]) -> ! {
    let forms = ["file.mbtiles ..."];

    print_usage(&mut std::io::stderr(), &argv[0], &forms, &LONG_OPTIONS, &[]);
    std::process::exit(EXIT_ARGS);
}

pub fn main() {
    restore_sigpipe();

    let mut argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argc = argv.len();

    let getopt_str = getopt_string(&LONG_OPTIONS);

    let mut g = Getopt::new();
    // usage() exits, so this is the C++ `while` loop
    if g.getopt_long(&mut argv, &getopt_str, &LONG_OPTIONS, None) != -1 {
        usage(&argv);
    }

    let optind = g.optind;
    if optind >= argc {
        usage(&argv);
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for arg in &argv[optind..argc] {
        enumerate(arg, &mut out);
    }
}

/// Restores the default `SIGPIPE` action that a C program starts with, so that
/// writing to a closed pipe ends the program quietly instead of failing the write.
fn restore_sigpipe() {
    crate::platform::sigpipe_default();
}

/// `sqlite3_open(fname, &db)`. On failure, returns `sqlite3_errmsg(db)`, without
/// the file name that rusqlite adds to the message when the file can't be opened.
fn sqlite3_open(fname: &str) -> Result<Connection, String> {
    Connection::open_with_flags(
        fname,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| {
        let msg = e.to_string();
        match e {
            rusqlite::Error::SqliteFailure(err, Some(_))
                if err.code == rusqlite::ErrorCode::CannotOpen =>
            {
                match msg.strip_suffix(&format!(": {}", fname)) {
                    Some(m) => m.to_string(),
                    None => msg,
                }
            }
            _ => msg,
        }
    })
}

/// A prepared statement used through the SQLite C API, as the C++ does, so that
/// the column conversions are SQLite's own.
struct Stmt<'a> {
    stmt: *mut ffi::sqlite3_stmt,
    _db: std::marker::PhantomData<&'a Connection>,
}

impl<'a> Stmt<'a> {
    /// `sqlite3_prepare_v2`. On failure, returns `sqlite3_errmsg(db)`.
    fn prepare(db: &'a Connection, sql: &CStr) -> Result<Stmt<'a>, String> {
        let mut stmt = std::ptr::null_mut();
        // SAFETY: the handle stays valid while `db` is borrowed, and `sql` is NUL-terminated.
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                db.handle(),
                sql.as_ptr(),
                -1,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            // SAFETY: sqlite3_errmsg returns a NUL-terminated string owned by the connection.
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db.handle())) };
            return Err(msg.to_string_lossy().into_owned());
        }
        Ok(Stmt {
            stmt,
            _db: std::marker::PhantomData,
        })
    }

    fn step(&mut self) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_step(self.stmt) }
    }

    fn column_int(&self, i: i32) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_column_int(self.stmt, i) }
    }
}

impl Drop for Stmt<'_> {
    fn drop(&mut self) {
        // SAFETY: the statement was prepared and is finalized once.
        unsafe {
            ffi::sqlite3_finalize(self.stmt);
        }
    }
}
