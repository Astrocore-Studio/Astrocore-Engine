use astrocore_client::{USAGE, build_app, parse_args};

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("ошибка: {msg}\n");
            }
            eprint!("{USAGE}");
            std::process::exit(if msg.is_empty() { 0 } else { 2 });
        }
    };
    build_app(args).run();
}
