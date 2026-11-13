//! The `manby` command, which accepts tippecanoe's options, and its
//! subcommands for tippecanoe's other programs: `manby join` accepts
//! tile-join's, `manby decode`, `manby enumerate` and `manby json-tool` are
//! tippecanoe-rs's tippecanoe-decode, tippecanoe-enumerate and
//! tippecanoe-json-tool, and `manby overzoom` is tippecanoe-overzoom, made
//! with manby's feature filters. Each program's own name also works as the
//! subcommand, and run under that name (a link, say), manby is that program. `manby serve` serves tilesets with
//! a map of them, and `manby stats` reports their tiles' sizes.

use std::ffi::OsString;

#[cfg(all(feature = "allocator", not(any(windows, target_family = "wasm"))))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "allocator", windows))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// jemalloc's settings: a few arenas, so that memory one thread frees is
/// reused by the others, and freed pages go back to the system at once, so
/// that the peak memory is the peak of what is in use.
#[cfg(all(feature = "allocator", not(any(windows, target_family = "wasm"))))]
#[repr(transparent)]
pub struct MallocConf(*const u8);
// SAFETY: the pointer is to a static string that is never written.
#[cfg(all(feature = "allocator", not(any(windows, target_family = "wasm"))))]
unsafe impl Sync for MallocConf {}
#[cfg(all(feature = "allocator", not(any(windows, target_family = "wasm"))))]
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static MALLOC_CONF: MallocConf =
    MallocConf(c"narenas:4,dirty_decay_ms:0,muzzy_decay_ms:0,retain:true".as_ptr() as *const u8);

/// tippecanoe's programs that manby runs as tippecanoe-rs has them, and
/// overzoom, which is manby's: the subcommand, the program's name, and its
/// `main`.
const TOOLS: [(&str, &str, fn()); 4] = [
    ("decode", "tippecanoe-decode", tippecanoe::decode::main),
    (
        "enumerate",
        "tippecanoe-enumerate",
        tippecanoe::enumerate::main,
    ),
    ("overzoom", "tippecanoe-overzoom", overzoom),
    (
        "json-tool",
        "tippecanoe-json-tool",
        tippecanoe::jsontool::main,
    ),
];

fn main() {
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // run_as names the program on Windows
    #[cfg(windows)]
    let run_as_name = std::env::var(RUN_AS).ok();
    #[cfg(unix)]
    let run_as_name: Option<String> = None;
    let name = match &run_as_name {
        Some(program) => program.as_str(),
        None => argv
            .first()
            .map(|a| tippecanoe::platform::program_name(a))
            .unwrap_or("manby"),
    };

    if let Some(&(_, _, tool)) = TOOLS.iter().find(|t| t.1 == name) {
        return tool();
    }
    let subcommand = argv.get(1).map(String::as_str).unwrap_or("");
    if let Some(&(_, program, _)) = TOOLS
        .iter()
        .find(|t| t.0 == subcommand || t.1 == subcommand)
    {
        run_as(program, &argv[0]);
    }

    let status = manby::command::run(&argv);
    if status != 0 {
        std::process::exit(status);
    }
}

/// tippecanoe-overzoom, with manby's feature filters.
fn overzoom() {
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Err(e) = manby::overzoom::parse_args(&argv).and_then(|o| manby::overzoom::run(&o)) {
        if !e.message.is_empty() {
            eprintln!("{}", e.message);
        }
        std::process::exit(e.exit_code);
    }
}

/// The environment variable that names the program on Windows.
#[cfg(windows)]
const RUN_AS: &str = "MANBY_RUN_AS";

/// Runs this program again as `program`, with the arguments after the
/// subcommand. tippecanoe-rs's programs read the process's own arguments, so
/// they are given the arguments, and the name, they would have.
fn run_as(program: &str, argv0: &str) -> ! {
    let args: Vec<OsString> = std::env::args_os().skip(2).collect();
    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!(
            "{}: can't find this program to run {}: {}",
            argv0, program, e
        );
        std::process::exit(tippecanoe::errors::EXIT_OPEN);
    });
    let mut command = std::process::Command::new(exe);
    command.args(args);
    #[cfg(unix)]
    let err = {
        use std::os::unix::process::CommandExt;
        command.arg0(program).exec()
    };
    // Windows can't name a program's process, so the program comes in an
    // environment variable that main reads.
    #[cfg(windows)]
    let err = match command.env(RUN_AS, program).status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => e,
    };
    eprintln!("{}: can't run {}: {}", argv0, program, err);
    std::process::exit(tippecanoe::errors::EXIT_OPEN);
}
