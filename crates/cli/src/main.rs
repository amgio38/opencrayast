//! Process entry point. Everything is in the library so tests can drive the CLI in-process.

use opencrayast::out;

fn main() {
    let cli = match opencrayast::parse_args() {
        Ok(c) => c,
        Err((code, e)) => {
            let _ = e.print();
            std::process::exit(code);
        }
    };
    let mut sink = out::Stdout;
    std::process::exit(opencrayast::run(&cli, &mut sink));
}
