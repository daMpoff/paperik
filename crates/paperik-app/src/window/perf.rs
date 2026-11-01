//! Optional reproducible latency probe on synthetic data; no document text is logged.
use super::*;
use std::time::{Duration, Instant};

unsafe fn drain(window: HWND, samples: &mut Vec<f64>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let pointer = unsafe { state_pointer(window) };
    while unsafe { (&*pointer).busy() } {
        assert!(Instant::now() < deadline);
        let started = Instant::now();
        unsafe {
            procedure(window, WM_SIZE, 0, 0);
            InvalidateRect(window, null(), 0);
            procedure(window, WM_PAINT, 0, 0);
            poll_background(window);
        }
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(unsafe { (&*pointer).operation_error.is_none() });
}
fn report(label: &str, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    println!(
        "ASYNC {label}: n={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
        samples.len(),
        samples[(samples.len() * 50).div_ceil(100) - 1],
        samples[(samples.len() * 95).div_ceil(100) - 1],
        samples.last().unwrap()
    );
}
#[test]
#[ignore = "manual release latency measurement on synthetic documents"]
fn async_latency_report() {
    unsafe {
        let class_name = windows_sys::w!("Paperik.AsyncLatencyProbe");
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class_name,
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&class), 0);
        let mut state = Box::new(State::empty(96.0));
        let handle = CreateWindowExW(
            0,
            class_name,
            TITLE,
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            1320,
            820,
            null_mut(),
            null_mut(),
            instance,
            (&mut *state as *mut State).cast(),
        );
        assert!(!handle.is_null());
        let window = Window(handle);
        state.renderer = Some(Renderer::new(handle, 1320, 820, 96.0).unwrap());
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/formatting.docx");
        let directory = std::env::temp_dir().join(format!(
            "paperik-latency-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut submissions = Vec::new();
        let mut heartbeat = Vec::new();
        let mut load_times = Vec::new();
        let mut layout_times = Vec::new();
        let mut save_times = Vec::new();
        let payload = "Проверка фоновой вёрстки. Сохраняются интервалы и оформление.\n".repeat(200);
        for index in 0..12 {
            let started = Instant::now();
            state.load(fixture.clone()).unwrap();
            submissions.push(started.elapsed().as_secs_f64() * 1000.0);
            drain(handle, &mut heartbeat);
            load_times.push(started.elapsed().as_secs_f64() * 1000.0);
            state.editor.as_mut().unwrap().select_all();
            let started = Instant::now();
            state.apply(Edit::Insert(payload.clone())).unwrap();
            submissions.push(started.elapsed().as_secs_f64() * 1000.0);
            drain(handle, &mut heartbeat);
            layout_times.push(started.elapsed().as_secs_f64() * 1000.0);
            let path = directory.join(format!("copy-{index}.docx"));
            let started = Instant::now();
            state.save_to(path.clone()).unwrap();
            submissions.push(started.elapsed().as_secs_f64() * 1000.0);
            drain(handle, &mut heartbeat);
            save_times.push(started.elapsed().as_secs_f64() * 1000.0);
            assert!(!state.editor.as_ref().unwrap().dirty());
            assert_eq!(Document::open(&path).unwrap().paragraphs.len(), 201);
            std::fs::remove_file(path).unwrap();
        }
        report("submission", submissions);
        report("UI resize+paint+poll", heartbeat);
        report("load total", load_times);
        report("200-paragraph edit total", layout_times);
        report("save total", save_times);
        std::fs::remove_dir(directory).unwrap();
        drop(window);
        drop(state);
        UnregisterClassW(class_name, instance);
    }
}
