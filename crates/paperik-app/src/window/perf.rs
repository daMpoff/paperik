//! Optional reproducible latency probe on synthetic data; no document text is logged.
use super::*;
use std::time::{Duration, Instant};

#[test]
#[ignore = "manual first-frame phase measurement in a fresh release test process"]
fn first_frame_report() {
    use windows::Win32::Graphics::Direct2D::*;
    unsafe {
        let started = Instant::now();
        let dpi: u32 = std::env::var("PAPERIK_QA_RENDER_DPI")
            .unwrap_or_else(|_| "96".into())
            .parse()
            .unwrap();
        assert!(matches!(dpi, 96 | 192));
        let class_name = windows_sys::w!("Paperik.FirstFrameProbe");
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
            1320 * dpi as i32 / 96,
            820 * dpi as i32 / 96,
            null_mut(),
            null_mut(),
            instance,
            (&mut *state as *mut State).cast(),
        );
        assert!(!handle.is_null());
        let window = Window(handle);
        let mut client = RECT::default();
        GetClientRect(handle, &mut client);
        state.dpi = dpi as f32;
        state.width = client.right as f32 * 96.0 / state.dpi;
        state.height = client.bottom as f32 * 96.0 / state.dpi;
        let mode = std::env::var("PAPERIK_QA_RENDER_MODE").unwrap_or_else(|_| "production".into());
        assert!(matches!(
            mode.as_str(),
            "production" | "default" | "default-immediate" | "software" | "software-immediate"
        ));
        let kind = if mode.starts_with("software") {
            D2D1_RENDER_TARGET_TYPE_SOFTWARE
        } else {
            D2D1_RENDER_TARGET_TYPE_DEFAULT
        };
        let present = if mode.ends_with("immediate") {
            D2D1_PRESENT_OPTIONS_IMMEDIATELY
        } else {
            D2D1_PRESENT_OPTIONS_NONE
        };
        let setup = started.elapsed().as_secs_f64() * 1000.0;
        let timer = Instant::now();
        let renderer = if mode == "production" {
            Renderer::new(handle, client.right as u32, client.bottom as u32, state.dpi)
        } else {
            Renderer::with_options(
                handle,
                client.right as u32,
                client.bottom as u32,
                state.dpi,
                kind,
                present,
            )
        }
        .unwrap();
        let create_ms = timer.elapsed().as_secs_f64() * 1000.0;
        let timer = Instant::now();
        renderer
            .resize(client.right as u32, client.bottom as u32, state.dpi)
            .unwrap();
        let resize_ms = timer.elapsed().as_secs_f64() * 1000.0;
        let (scene, present) = renderer.measure_paint(&state.view()).unwrap();
        let first_total = started.elapsed().as_secs_f64() * 1000.0;
        let (second_scene, second_present) = renderer.measure_paint(&state.view()).unwrap();
        let editor = Editor::new(
            Document::from_bytes(
                include_bytes!("../../../../tests/fixtures/formatting.docx").to_vec(),
            )
            .unwrap(),
        );
        state.layout = Some(renderer.document(editor.document()).unwrap());
        state.zoom = render::fit_zoom(state.width, state.height, editor.document());
        state.editor = Some(editor);
        let (doc_scene, doc_present) = renderer.measure_paint(&state.view()).unwrap();
        println!(
            "FIRST_FRAME mode={mode} dpi={dpi} setup_ms={setup:.3} create_ms={create_ms:.3} resize_ms={resize_ms:.3} scene_ms={scene:.3} present_ms={present:.3} total_ms={first_total:.3} second_scene_ms={second_scene:.3} second_present_ms={second_present:.3} doc_scene_ms={doc_scene:.3} doc_present_ms={doc_present:.3}"
        );
        drop(renderer);
        drop(window);
        drop(state);
        UnregisterClassW(class_name, instance);
    }
}

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
