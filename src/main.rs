use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    if args.next().as_deref() == Some("--version") && args.next().is_none() {
        println!("chainview 0.1.0");
        ExitCode::SUCCESS
    } else {
        eprintln!("Usage: chainview --version");
        ExitCode::from(2)
    }
}
