use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_paperik");

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
