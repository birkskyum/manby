//! The `manby` command, which accepts tippecanoe's options.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
