use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_paperik");

struct Fixture {
    directory: std::path::PathBuf,
    file: std::path::PathBuf,
}
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("paperik-fixture-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("formatting.docx");
        std::fs::write(&file, bytes).unwrap();
        Self { directory, file }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[test]
fn docx_can_be_inspected_without_changing_the_file() {
    let original = include_bytes!("../../../tests/fixtures/formatting.docx");
    let fixture = Fixture::new(original);
    let output = Command::new(BINARY)
        .arg("--inspect")
        .arg(&fixture.file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Абзацев: 5"));
    assert_eq!(std::fs::read(&fixture.file).unwrap(), original);
}

#[cfg(target_os = "windows")]
#[test]
fn docx_can_be_laid_out_and_drawn_on_two_pages() {
    let fixture = Fixture::new(include_bytes!("../../../tests/fixtures/formatting.docx"));
    let output = Command::new(BINARY)
        .arg("--smoke-test")
        .arg(&fixture.file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Отображено страниц: 2"));
}

#[test]
fn corrupt_docx_does_not_open_a_modal_or_report_success() {
    let fixture = Fixture::new(b"not a zip");
    let output = Command::new(BINARY)
        .arg("--smoke-test")
        .arg(&fixture.file)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
}

#[test]
fn version_is_available_without_opening_the_editor() {
    let output = Command::new(BINARY).arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("Paperik {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn unknown_arguments_are_rejected() {
    let output = Command::new(BINARY)
        .arg("--not-a-paperik-option")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.is_empty());
}

#[cfg(target_os = "windows")]
#[test]
fn native_window_can_resize_and_close_without_becoming_visible() {
    let mut child = Command::new(BINARY)
        .arg("--smoke-test")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "Проверка окна завершилась с {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Приложение не закрылось за 10 секунд");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
