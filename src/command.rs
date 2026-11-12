//! The `manby` command line, apart from the programs of tippecanoe-rs's that
//! it runs, for the `manby` program and for R in the browser, which runs
//! manby in its own process.

/// Runs the command line `argv`, whose first argument is the program's name,
/// and gives the exit status, after printing any error.
pub fn run(argv: &[String]) -> i32 {
    match result(argv) {
        Ok(true) => tippecanoe::errors::EXIT_INCOMPLETE,
        Ok(false) => 0,
        Err(e) => {
            // The messages are written out in full, as tippecanoe prints them.
            if !e.message.is_empty() {
                eprintln!("{}", e.message);
            }
            e.exit_code
        }
    }
}

/// Runs the command line, and gives whether the tileset is incomplete.
fn result(argv: &[String]) -> crate::Result<bool> {
    let program = argv.first().map(String::as_str).unwrap_or("manby");
    let subcommand = argv.get(1).map(String::as_str).unwrap_or("");
    // the subcommand's arguments, with the subcommand in the program's name
    let after = |name: &str| {
        let mut v = vec![format!("{} {}", program, name)];
        v.extend_from_slice(&argv[2.min(argv.len())..]);
        v
    };
    if tippecanoe::platform::program_name(program) == "tile-join" {
        // under tile-join's name, manby is tile-join
        let commandline = tippecanoe::text::format_commandline(argv);
        join(crate::join::args::parse_args(argv, commandline))
    } else if subcommand == "stats" {
        crate::stats::parse_args(&after("stats"))
            .and_then(|o| crate::stats::stats(&o))
            .map(|_| false)
    } else if subcommand == "merge" {
        crate::shard::parse_args(&after("merge"))
            .and_then(|o| crate::merge(&o))
            .map(|summary| summary.incomplete)
    } else if subcommand == "serve" {
        crate::serve::parse_args(&after("serve"))
            .and_then(|o| crate::serve::serve(&o))
            .map(|_| false)
    } else if subcommand == "join" || subcommand == "tile-join" {
        let commandline = tippecanoe::text::format_commandline(argv);
        join(crate::join::args::parse_args(&after(subcommand), commandline))
    } else {
        let mut argv = argv.to_vec();
        if subcommand == "tippecanoe" {
            argv.remove(1);
        }
        crate::options::parse_args(&argv)
            .and_then(|options| crate::run(&options))
            .map(|summary| summary.incomplete)
    }
}

fn join(options: crate::Result<crate::JoinOptions>) -> crate::Result<bool> {
    options.and_then(|o| crate::join(&o)).map(|_| false)
}
