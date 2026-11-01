use crate::{
    layout::Layout,
    render::{self, Renderer, View},
};
use paperik_document::Document;
use std::{
    io,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::Dialogs::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};

const CLASS: *const u16 = windows_sys::w!("Paperik.MainWindow");
const TITLE: *const u16 = windows_sys::w!("Paperik");

struct State {
    renderer: Option<Renderer>,
    document: Option<Document>,
    layout: Option<Layout>,
    filename: String,
    message: String,
    width: f32,
    height: f32,
    dpi: f32,
    zoom: f32,
    scroll: f32,
    dark: bool,
}
impl State {
    fn view(&self) -> View<'_> {
        View {
            width: self.width,
            height: self.height,
            zoom: self.zoom,
            scroll: self.scroll,
            dark: self.dark,
            document: self.document.as_ref(),
            layout: self.layout.as_ref(),
            filename: &self.filename,
            message: &self.message,
        }
    }
    fn clamp(&mut self) {
        self.scroll = self.scroll.clamp(0.0, render::max_scroll(&self.view()));
    }
    fn load(&mut self, path: PathBuf) -> Result<(), String> {
        let document = Document::open(&path)?;
        let layout = self
            .renderer
            .as_ref()
            .ok_or("Рендерер недоступен")?
            .document(&document)
            .map_err(|e| format!("Не удалось выполнить вёрстку: {e}"))?;
        self.filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.zoom = render::fit_zoom(self.width, self.height, &document);
        self.scroll = 0.0;
        self.document = Some(document);
        self.layout = Some(layout);
        self.message.clear();
        Ok(())
    }
}
struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(self.0) != 0 {
                DestroyWindow(self.0);
            }
        }
    }
}
struct Class(HINSTANCE);
impl Drop for Class {
    fn drop(&mut self) {
        unsafe {
            UnregisterClassW(CLASS, self.0);
        }
    }
}
struct Icon(HICON);
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                DestroyIcon(self.0);
            }
        }
    }
}

fn last_error() -> String {
    io::Error::last_os_error().to_string()
}
fn brand_icon() -> Icon {
    let data = include_bytes!("../../../assets/brand/paperik-app.ico");
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    for index in 0..count {
        let entry = 6 + index * 16;
        if data[entry] != 32 {
            continue;
        }
        let length = u32::from_le_bytes(data[entry + 8..entry + 12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(data[entry + 12..entry + 16].try_into().unwrap()) as usize;
        if let Some(image) = data.get(offset..offset + length) {
            return Icon(unsafe {
                CreateIconFromResourceEx(image.as_ptr(), length as u32, 1, 0x0003_0000, 32, 32, 0)
            });
        }
    }
    Icon(null_mut())
}

pub fn run(smoke_test: bool, path: Option<PathBuf>) -> Result<(), String> {
    // SAFETY: all HWND/COM resources and the borrowed State pointer belong to
    // this thread. The box outlives the window and is never freed by callbacks.
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) {
                return Err(error.to_string());
            }
        }
        let instance = GetModuleHandleW(null());
        if instance.is_null() {
            return Err(last_error());
        }
        let icon = brand_icon();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            hIcon: icon.0,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(last_error());
        }
        let _class = Class(instance);
        let dpi = GetDpiForSystem().max(96) as f32;
        let mut state = Box::new(State {
            renderer: None,
            document: None,
            layout: None,
            filename: String::new(),
            message: String::new(),
            width: 1320.0,
            height: 820.0,
            dpi,
            zoom: 1.0,
            scroll: 0.0,
            dark: false,
        });
        let mut bounds = RECT {
            left: 0,
            top: 0,
            right: (1320.0 * dpi / 96.0) as i32,
            bottom: (820.0 * dpi / 96.0) as i32,
        };
        if AdjustWindowRectExForDpi(&mut bounds, WS_OVERLAPPEDWINDOW, 0, 0, dpi as u32) == 0 {
            return Err(last_error());
        }
        let handle = CreateWindowExW(
            0,
            CLASS,
            TITLE,
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            null_mut(),
            null_mut(),
            instance,
            (&mut *state as *mut State).cast(),
        );
        if handle.is_null() {
            return Err(last_error());
        }
        let window = Window(handle);
        DragAcceptFiles(handle, 1);
        let mut client = RECT::default();
        if GetClientRect(handle, &mut client) == 0 {
            return Err(last_error());
        }
        state.width = client.right as f32 * 96.0 / dpi;
        state.height = client.bottom as f32 * 96.0 / dpi;
        state.renderer = Some(
            Renderer::new(handle, client.right as u32, client.bottom as u32, dpi)
                .map_err(|e| e.to_string())?,
        );
        if let Some(path) = path {
            state.load(path)?;
            let title: Vec<u16> = format!("{} — Paperik", state.filename)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            SetWindowTextW(handle, title.as_ptr());
        }
        if smoke_test {
            if SetWindowPos(
                handle,
                null_mut(),
                0,
                0,
                (1000.0 * dpi / 96.0) as i32,
                (700.0 * dpi / 96.0) as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) == 0
            {
                return Err(last_error());
            }
            state
                .renderer
                .as_ref()
                .unwrap()
                .paint(&state.view())
                .map_err(|e| e.to_string())?;
            if let Some(layout) = &state.layout {
                println!("Отображено страниц: {}", layout.pages.len());
            }
            if PostMessageW(handle, WM_CLOSE, 0, 0) == 0 {
                return Err(last_error());
            }
        } else {
            ShowWindow(handle, SW_SHOW);
            UpdateWindow(handle);
        }
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => return Err(last_error()),
                0 => break,
                _ => {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        drop(window);
        Ok(())
    }
}

unsafe fn state_pointer(window: HWND) -> *mut State {
    unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut State }
}

fn open_dialog(window: HWND) -> Option<PathBuf> {
    let mut buffer = vec![0_u16; 32768];
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: window,
        lpstrFilter: windows_sys::w!("Документы Word (*.docx)\0*.docx\0\0"),
        lpstrFile: buffer.as_mut_ptr(),
        nMaxFile: buffer.len() as u32,
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // No Rust borrow of State is held while the modal dialog pumps messages.
    if unsafe { GetOpenFileNameW(&mut dialog) } != 0 {
        let end = buffer.iter().position(|v| *v == 0).unwrap_or(buffer.len());
        Some(PathBuf::from(String::from_utf16_lossy(&buffer[..end])))
    } else {
        None
    }
}

unsafe fn open_file(window: HWND, path: PathBuf) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        if let Err(error) = (&mut *pointer).load(path) {
            (&mut *pointer).message = error.clone();
            show_error(&error);
        } else {
            let title: Vec<u16> = format!("{} — Paperik", (&*pointer).filename)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            SetWindowTextW(window, title.as_ptr());
        }
        InvalidateRect(window, null(), 0);
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Windows supplies CREATESTRUCTW/RECT/PAINTSTRUCT synchronously.
    // State is owned by run and borrowed only for this callback's immediate work.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = state_pointer(window);
        match message {
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_ERASEBKGND => 1,
            WM_SIZE if !pointer.is_null() => {
                let state = &mut *pointer;
                let mut client = RECT::default();
                GetClientRect(window, &mut client);
                state.dpi = GetDpiForWindow(window).max(96) as f32;
                state.width = client.right as f32 * 96.0 / state.dpi;
                state.height = client.bottom as f32 * 96.0 / state.dpi;
                if let Some(renderer) = &state.renderer
                    && client.right > 0
                    && client.bottom > 0
                    && let Err(error) =
                        renderer.resize(client.right as u32, client.bottom as u32, state.dpi)
                {
                    state.message = error.to_string();
                }
                state.clamp();
                InvalidateRect(window, null(), 0);
                0
            }
            WM_DPICHANGED if lparam != 0 => {
                let r = &*(lparam as *const RECT);
                SetWindowPos(
                    window,
                    null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                0
            }
            WM_GETMINMAXINFO if lparam != 0 => {
                let dpi = GetDpiForWindow(window).max(96) as f32;
                let info = &mut *(lparam as *mut MINMAXINFO);
                info.ptMinTrackSize.x = (900.0 * dpi / 96.0) as i32;
                info.ptMinTrackSize.y = (600.0 * dpi / 96.0) as i32;
                0
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                BeginPaint(window, &mut paint);
                if !pointer.is_null() {
                    let state = &mut *pointer;
                    if let Some(renderer) = &state.renderer
                        && let Err(error) = renderer.paint(&state.view())
                    {
                        state.message = format!("Ошибка отображения: {error}");
                    }
                }
                EndPaint(window, &paint);
                0
            }
            WM_DROPFILES => {
                let drop = wparam as HDROP;
                let length = DragQueryFileW(drop, 0, null_mut(), 0);
                let mut path = vec![0_u16; length as usize + 1];
                DragQueryFileW(drop, 0, path.as_mut_ptr(), path.len() as u32);
                DragFinish(drop);
                open_file(
                    window,
                    PathBuf::from(String::from_utf16_lossy(&path[..length as usize])),
                );
                0
            }
            WM_MOUSEWHEEL if !pointer.is_null() => {
                let state = &mut *pointer;
                let delta = ((wparam >> 16) as u16 as i16) as f32 / 120.0;
                if GetKeyState(VK_CONTROL as i32) < 0 {
                    state.zoom = (state.zoom * 1.1_f32.powf(delta)).clamp(0.25, 2.0);
                } else {
                    state.scroll -= delta * 80.0 / state.zoom;
                }
                state.clamp();
                InvalidateRect(window, null(), 0);
                0
            }
            WM_KEYDOWN if !pointer.is_null() => {
                let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                if ctrl && wparam == b'O' as usize {
                    if let Some(path) = open_dialog(window) {
                        open_file(window, path);
                    }
                    return 0;
                }
                let state = &mut *pointer;
                let (_, _, top, bottom) = render::panes(state.width, state.height);
                match wparam as u16 {
                    VK_HOME => state.scroll = 0.0,
                    VK_END => state.scroll = render::max_scroll(&state.view()),
                    VK_NEXT => state.scroll += (bottom - top - 48.0) / state.zoom,
                    VK_PRIOR => state.scroll -= (bottom - top - 48.0) / state.zoom,
                    VK_DOWN => state.scroll += 48.0 / state.zoom,
                    VK_UP => state.scroll -= 48.0 / state.zoom,
                    VK_ADD | VK_OEM_PLUS if ctrl => state.zoom = (state.zoom * 1.1).min(2.0),
                    VK_SUBTRACT | VK_OEM_MINUS if ctrl => state.zoom = (state.zoom / 1.1).max(0.25),
                    key if ctrl && key == b'0' as u16 => {
                        if let Some(doc) = &state.document {
                            state.zoom = render::fit_zoom(state.width, state.height, doc);
                        }
                    }
                    _ => return DefWindowProcW(window, message, wparam, lparam),
                }
                state.clamp();
                InvalidateRect(window, null(), 0);
                0
            }
            WM_LBUTTONUP if !pointer.is_null() => {
                let state = &*pointer;
                let x = (lparam as u16 as i16) as f32 * 96.0 / state.dpi;
                let y = ((lparam >> 16) as u16 as i16) as f32 * 96.0 / state.dpi;
                if (14.0..50.0).contains(&y) && x >= state.width - 228.0 && x <= state.width - 96.0
                {
                    if let Some(path) = open_dialog(window) {
                        open_file(window, path);
                    }
                    return 0;
                }
                let state = &mut *pointer;
                if y < 64.0 && x >= state.width - 90.0 {
                    state.dark = !state.dark;
                }
                let (left, right, _, bottom) = render::panes(state.width, state.height);
                if y >= bottom && x > right - 128.0 && x < right {
                    if x < right - 88.0 {
                        state.zoom = (state.zoom / 1.1).max(0.25);
                    } else if x > right - 35.0 {
                        state.zoom = (state.zoom * 1.1).min(2.0);
                    }
                }
                if x < left
                    && y >= 273.0
                    && y < bottom
                    && let (Some(doc), Some(layout)) = (&state.document, &state.layout)
                {
                    let current = (state.scroll / (doc.page.height + 24.0)).floor() as usize;
                    let index = current.saturating_sub(2) + ((y - 273.0) / 34.0) as usize;
                    if index < layout.pages.len() {
                        state.scroll = index as f32 * (doc.page.height + 24.0);
                    }
                }
                state.clamp();
                InvalidateRect(window, null(), 0);
                0
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

pub fn show_error(message: &str) {
    let wide: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(null_mut(), wide.as_ptr(), TITLE, MB_OK | MB_ICONERROR);
    }
}
