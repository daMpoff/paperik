#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod window;

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => launch(false),
        [argument] if argument == "--smoke-test" => launch(true),
        [argument] if argument == "--version" => {
            println!("Paperik {}", env!("CARGO_PKG_VERSION"));
        }
        [argument] if argument == "--help" => {
            println!(
                "Paperik\n\n--version     Версия приложения\n--smoke-test  Проверка скрытого окна и завершения\n--help        Эта справка"
            );
        }
        _ => {
            eprintln!("Неизвестные аргументы. Используйте --help.");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "windows")]
fn launch(smoke_test: bool) {
    if let Err(error) = window::run(smoke_test) {
        let message = format!("Не удалось запустить Paperik: {error}");
        eprintln!("{message}");
        if !smoke_test {
            window::show_error(&message);
        }
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn launch(_smoke_test: bool) {
    eprintln!("Первая версия Paperik предназначена для Windows.");
    std::process::exit(1);
}
