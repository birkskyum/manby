//! The `manby` command, which accepts tippecanoe's options, and `manby join`,
//! which accepts tile-join's. Run as `tile-join`, it is `manby join`.

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// jemalloc's settings: a few arenas, so that memory one thread frees is
/// reused by the others, and freed pages go back to the system at once, so
/// that the peak memory is the peak of what is in use.
#[repr(transparent)]
pub struct MallocConf(*const u8);
// SAFETY: the pointer is to a static string that is never written.
unsafe impl Sync for MallocConf {}
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static MALLOC_CONF: MallocConf =
    MallocConf(c"narenas:4,dirty_decay_ms:0,muzzy_decay_ms:0,retain:true".as_ptr() as *const u8);

fn main() {
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let name = argv
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a))
        .unwrap_or("manby");

    let result = if name == "tile-join" {
        // under tile-join's name, manby is tile-join
        let commandline = tippecanoe::text::format_commandline(&argv);
        join(manby::join::args::parse_args(&argv, commandline))
    } else if argv.get(1).is_some_and(|a| a == "join") {
        let commandline = tippecanoe::text::format_commandline(&argv);
        let mut join_argv = vec![format!("{} join", argv[0])];
        join_argv.extend_from_slice(&argv[2..]);
        join(manby::join::args::parse_args(&join_argv, commandline))
    } else {
        manby::options::parse_args(&argv)
            .and_then(|options| manby::run(&options))
            .map(|summary| summary.incomplete)
    };
    match result {
        Ok(incomplete) => {
            if incomplete {
                std::process::exit(tippecanoe::errors::EXIT_INCOMPLETE);
            }
        }
        Err(e) => {
            // The messages are written out in full, as tippecanoe prints them.
            if !e.message.is_empty() {
                eprintln!("{}", e.message);
            }
            std::process::exit(e.exit_code);
        }
    }
}

fn join(options: manby::Result<manby::JoinOptions>) -> manby::Result<bool> {
    options.and_then(|o| manby::join(&o)).map(|_| false)
}
