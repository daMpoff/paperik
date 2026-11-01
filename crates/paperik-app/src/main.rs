#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod clipboard;
#[cfg(target_os = "windows")]
mod export;
#[cfg(target_os = "windows")]
mod layout;
#[cfg(target_os = "windows")]
mod recovery;
#[cfg(target_os = "windows")]
mod render;
#[cfg(target_os = "windows")]
mod text_input;
#[cfg(target_os = "windows")]
mod window;

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => launch(false, None),
        [argument] if argument == "--smoke-test" => launch(true, None),
        [argument, path] if argument == "--smoke-test" => {
            launch(true, Some(std::path::PathBuf::from(path)))
        }
        #[cfg(target_os = "windows")]
        [argument, path] if argument == "--layout-report" => diagnostic(path, None),
        #[cfg(target_os = "windows")]
        [argument, path, directory] if argument == "--render-pages" => {
            diagnostic(path, Some(directory))
        }
        [argument, path] if argument == "--inspect" => {
            match paperik_document::Document::open(std::path::Path::new(path)) {
                Ok(document) => {
                    println!(
                        "Абзацев: {}\nФрагментов: {}\nСтраница: {:.2} × {:.2} DIP\nПоля: {:.2} / {:.2} / {:.2} / {:.2} DIP\nПредупреждений: {}",
                        document.paragraphs.len(),
                        document
                            .paragraphs
                            .iter()
                            .map(|p| p.runs.len())
                            .sum::<usize>(),
                        document.page.width,
                        document.page.height,
                        document.page.left,
                        document.page.right,
                        document.page.top,
                        document.page.bottom,
                        document.warnings.len()
                    );
                }
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
        }
        [argument] if argument == "--version" => {
            println!("Paperik {}", env!("CARGO_PKG_VERSION"));
        }
        [argument] if argument == "--help" => {
            println!(
                "Paperik\n\npaperik.exe файл.docx\n\n--version                 Версия приложения\n--smoke-test [файл]        Проверка скрытого окна и отображения\n--inspect файл            Структурная диагностика DOCX\n--layout-report файл      Координаты строк без содержимого\n--render-pages файл папка PNG страниц (144 dpi) в новый каталог\n--help                    Эта справка"
            );
        }
        [path] if !path.to_string_lossy().starts_with('-') => {
            launch(false, Some(std::path::PathBuf::from(path)))
        }
        _ => {
            eprintln!("Неизвестные аргументы. Используйте --help.");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "windows")]
fn diagnostic(path: &std::ffi::OsStr, directory: Option<&std::ffi::OsString>) {
    match export::run(
        std::path::Path::new(path),
        directory.map(std::path::Path::new),
    ) {
        Ok(report) => print!("{report}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "windows")]
fn launch(smoke_test: bool, path: Option<std::path::PathBuf>) {
    if let Err(error) = window::run(smoke_test, path) {
        let message = format!("Не удалось запустить Paperik: {error}");
        eprintln!("{message}");
        if !smoke_test {
            window::show_error(&message);
        }
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn launch(_smoke_test: bool, _path: Option<std::path::PathBuf>) {
    eprintln!("Первая версия Paperik предназначена для Windows.");
    std::process::exit(1);
}
