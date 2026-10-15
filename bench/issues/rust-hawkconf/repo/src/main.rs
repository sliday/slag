use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: hawkconf <file.hawk>");
        return ExitCode::from(2);
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(src) => src,
        Err(err) => {
            eprintln!("cannot read {path}: {err}");
            return ExitCode::from(2);
        }
    };
    match hawkconf::parse(&src) {
        Ok(entries) => {
            for entry in entries {
                println!("{} = {}", entry.key, entry.value);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error at line {}: {}", err.line, err.message);
            ExitCode::FAILURE
        }
    }
}
