//! The `manby` command, which accepts tippecanoe's options.

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
    let argv: Vec<String> = std::env::args().collect();
    let result = manby::options::parse_args(&argv).and_then(|options| manby::run(&options));
    match result {
        Ok(summary) => {
            if summary.incomplete {
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
