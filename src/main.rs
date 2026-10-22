//! The `manby` command, which accepts tippecanoe's options.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let result = manby::options::parse_args(&argv).and_then(|options| manby::run(&options));
    if let Err(e) = result {
        eprintln!("{}: {}", argv[0], e);
        std::process::exit(e.exit_code);
    }
}
