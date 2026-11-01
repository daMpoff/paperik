use crate::{
    layout::Layout,
    recovery::{self, Autosave, Recovered},
    render::{self, Renderer, View},
};
use paperik_document::Document;
use paperik_document::editor::{Edit, Editor, Position};
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
        Controls::Dialogs::*, HiDpi::*, Input::Ime::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};

const CLASS: *const u16 = windows_sys::w!("Paperik.MainWindow");
const TITLE: *const u16 = windows_sys::w!("Paperik");

struct State {
    renderer: Option<Renderer>,
    editor: Option<Editor>,
    layout: Option<Layout>,
    filename: String,
    file_path: Option<PathBuf>,
    message: String,
    width: f32,
    height: f32,
    dpi: f32,
    zoom: f32,
    scroll: f32,
    dark: bool,
    recovery_root: Option<PathBuf>,
    autosave: Option<Autosave>,
    autosave_status: &'static str,
    autosave_error: Option<String>,
    confirmed_stamp: Option<u64>,
    last_attempt: std::time::Instant,
    focused: bool,
    dragging: bool,
    drag_point: (f32, f32),
    upstream: bool,
    preferred_x: Option<f32>,
    input: crate::text_input::Utf16Input,
    composing: bool,
}
impl State {
    fn empty(dpi: f32) -> Self {
        Self {
            renderer: None,
            editor: None,
            layout: None,
            filename: String::new(),
            file_path: None,
            message: String::new(),
            width: 1320.0,
            height: 820.0,
            dpi,
            zoom: 1.0,
            scroll: 0.0,
            dark: false,
            recovery_root: None,
            autosave: None,
            autosave_status: "",
            autosave_error: None,
            confirmed_stamp: None,
            last_attempt: std::time::Instant::now(),
            focused: false,
            dragging: false,
            drag_point: (0.0, 0.0),
            upstream: false,
            preferred_x: None,
            input: crate::text_input::Utf16Input::default(),
            composing: false,
        }
    }
    fn view(&self) -> View<'_> {
        View {
            width: self.width,
            height: self.height,
            zoom: self.zoom,
            scroll: self.scroll,
            dark: self.dark,
            document: self.editor.as_ref().map(Editor::document),
            layout: self.layout.as_ref(),
            filename: &self.filename,
            message: &self.message,
            selection: self.editor.as_ref().map(Editor::selection),
            dirty: self.editor.as_ref().is_some_and(Editor::dirty),
            autosave_status: self.autosave_status,
            recovery_enabled: self.recovery_root.is_some(),
        }
    }
    fn clamp(&mut self) {
        self.scroll = self.scroll.clamp(0.0, render::max_scroll(&self.view()));
    }
    fn load(&mut self, path: PathBuf) -> Result<(), String> {
        let editor = Editor::new(Document::open(&path)?);
        let document = editor.document();
        let layout = self
            .renderer
            .as_ref()
            .ok_or("Рендерер недоступен")?
            .document(document)
            .map_err(|e| format!("Не удалось выполнить вёрстку: {e}"))?;
        self.finish_autosave();
        self.filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.file_path = Some(path);
        self.zoom = render::fit_zoom(self.width, self.height, document);
        self.scroll = 0.0;
        self.editor = Some(editor);
        self.layout = Some(layout);
        self.message.clear();
        self.upstream = false;
        self.preferred_x = None;
        self.input.clear();
        self.start_autosave();
        Ok(())
    }
    fn start_autosave(&mut self) {
        if self.recovery_root.is_none() {
            self.update_autosave_status();
            return;
        }
        self.confirmed_stamp = None;
        self.autosave_error = None;
        self.last_attempt = std::time::Instant::now();
        if let Some(root) = &self.recovery_root {
            match Autosave::new(root) {
                Ok(autosave) => {
                    self.autosave = Some(autosave);
                    self.autosave_status = "Автосохранение включено";
                }
                Err(error) => {
                    self.autosave_error = Some(error.clone());
                    self.message = format!("Автосохранение недоступно: {error}");
                    self.autosave_status = "Ошибка автосохранения";
                }
            }
        }
    }
    fn finish_autosave(&mut self) {
        if let Some(autosave) = self.autosave.take()
            && let Err(error) = autosave.finish()
        {
            self.message = format!("Черновик не закрыт: {error}");
        }
    }
    fn clear_autosave(&mut self) {
        self.confirmed_stamp = None;
        if let Some(autosave) = &mut self.autosave {
            if let Err(error) = autosave.clean() {
                self.autosave_error = Some(error.clone());
                self.message = format!("Не удалось завершить черновик: {error}");
            } else {
                self.autosave_error = None;
            }
        }
        self.update_autosave_status();
    }
    fn update_autosave_status(&mut self) {
        self.autosave_status = if self.autosave_error.is_some() {
            "Ошибка автосохранения"
        } else if let (Some(editor), Some(autosave)) = (&self.editor, &self.autosave) {
            if autosave.busy {
                "Автосохранение…"
            } else if !editor.dirty() {
                "Автосохранение включено"
            } else if self.confirmed_stamp == Some(editor.checkpoint_stamp()) {
                "Черновик сохранён"
            } else {
                "Автосохранение ожидается"
            }
        } else {
            ""
        };
    }
    fn autosave_tick(&mut self, force: bool) -> bool {
        let previous = self.autosave_status;
        if let Some(autosave) = &mut self.autosave {
            for event in autosave.poll() {
                match event.result {
                    Ok(()) => {
                        self.confirmed_stamp = Some(event.stamp);
                        self.autosave_error = None;
                    }
                    Err(error) => {
                        self.confirmed_stamp = None;
                        self.message = format!("Не удалось сохранить черновик: {error}");
                        self.autosave_error = Some(error);
                    }
                }
            }
            if let Some(editor) = &self.editor
                && editor.dirty()
                && self.confirmed_stamp != Some(editor.checkpoint_stamp())
                && !autosave.busy
                && (force || self.last_attempt.elapsed() >= std::time::Duration::from_secs(5))
            {
                self.last_attempt = std::time::Instant::now();
                if let Err(error) = autosave.queue(editor, &self.filename) {
                    self.autosave_error = Some(error.clone());
                    self.message = format!("Автосохранение недоступно: {error}");
                }
            }
        }
        self.update_autosave_status();
        self.autosave_status != previous
    }
    fn restore(&mut self, recovered: Recovered) -> Result<(), String> {
        let layout = self
            .renderer
            .as_ref()
            .ok_or("Рендерер недоступен")?
            .document(recovered.editor.document())
            .map_err(|e| e.to_string())?;
        let autosave = Autosave::resume(recovered.session)?;
        self.finish_autosave();
        self.zoom = render::fit_zoom(self.width, self.height, recovered.editor.document());
        self.scroll = 0.0;
        self.confirmed_stamp = Some(recovered.editor.checkpoint_stamp());
        self.editor = Some(recovered.editor);
        self.layout = Some(layout);
        self.filename = recovered.name;
        self.file_path = None;
        self.autosave = Some(autosave);
        self.autosave_error = None;
        self.last_attempt = std::time::Instant::now();
        self.upstream = false;
        self.preferred_x = None;
        self.input.clear();
        self.message = if recovered.previous_generation {
            "Восстановлен предыдущий исправный снимок сессии."
        } else {
            "Несохранённая сессия восстановлена. Сохраните копию DOCX."
        }
        .into();
        self.update_autosave_status();
        self.reveal();
        Ok(())
    }
    fn save_to(&mut self, path: PathBuf) -> Result<(), String> {
        let editor = self.editor.as_mut().ok_or("Сначала откройте документ")?;
        let count = self.layout.as_ref().map(|l| l.pages.len());
        editor.document().save_new(&path, count)?;
        editor.mark_saved();
        self.filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.file_path = Some(path);
        self.message = "Копия сохранена. Исходный документ не изменён.".into();
        self.clear_autosave();
        Ok(())
    }
    fn apply(&mut self, edit: Edit) -> Result<(), String> {
        let was_dirty = self.editor.as_ref().is_some_and(Editor::dirty);
        let Some(editor) = &mut self.editor else {
            return Ok(());
        };
        if let Some(change) = editor.prepare(edit)? {
            let layout = self
                .renderer
                .as_ref()
                .ok_or("Рендерер недоступен")?
                .document(&change.document)
                .map_err(|error| {
                    format!("Изменение отменено: не удалось выполнить вёрстку ({error})")
                })?;
            editor.commit(change)?;
            self.layout = Some(layout);
            self.upstream = false;
            self.preferred_x = None;
        }
        self.message.clear();
        let dirty = self.editor.as_ref().is_some_and(Editor::dirty);
        if was_dirty && !dirty {
            self.clear_autosave();
        }
        if !was_dirty && dirty {
            self.last_attempt = std::time::Instant::now();
        }
        self.update_autosave_status();
        self.reveal();
        Ok(())
    }
    fn caret(&self) -> Option<crate::layout::Caret> {
        let editor = self.editor.as_ref()?;
        self.layout
            .as_ref()?
            .caret(editor.document(), editor.selection().focus, self.upstream)
            .ok()
            .flatten()
    }
    fn hit(&self, x: f32, y: f32) -> Option<(Position, bool)> {
        let editor = self.editor.as_ref()?;
        let doc = editor.document();
        let layout = self.layout.as_ref()?;
        let (left, right, top, _) = render::panes(self.width, self.height);
        let page_x = left + (right - left - doc.page.width * self.zoom) / 2.0;
        let absolute_y = (y - top) / self.zoom + self.scroll - 24.0;
        let index = (absolute_y.max(0.0) / (doc.page.height + 24.0)).floor() as usize;
        let index = index.min(layout.pages.len().saturating_sub(1));
        layout
            .hit_test(
                doc,
                index,
                (x - page_x) / self.zoom,
                absolute_y - index as f32 * (doc.page.height + 24.0),
            )
            .ok()
            .flatten()
    }
    fn reveal(&mut self) {
        if let (Some(caret), Some(editor)) = (self.caret(), self.editor.as_ref()) {
            let (_, _, top, bottom) = render::panes(self.width, self.height);
            let y = 24.0 + caret.page as f32 * (editor.document().page.height + 24.0) + caret.y;
            let visible = (bottom - top) / self.zoom;
            if y < self.scroll + 12.0 {
                self.scroll = (y - 12.0).max(0.0);
            }
            if y + caret.height > self.scroll + visible - 12.0 {
                self.scroll = y + caret.height - visible + 12.0;
            }
            self.clamp();
        }
    }
    fn move_vertical(&mut self, steps: isize, extend: bool) {
        let Some(caret) = self.caret() else {
            return;
        };
        let x = *self.preferred_x.get_or_insert(caret.x);
        if let (Some(editor), Some(layout)) = (&mut self.editor, &self.layout)
            && let Ok(Some((position, upstream))) = layout.vertical(
                editor.document(),
                editor.selection().focus,
                self.upstream,
                x,
                steps,
            )
        {
            editor.set_position(position, extend);
            self.upstream = upstream;
        }
        self.reveal();
    }
    fn move_edge(&mut self, right: bool, whole: bool, extend: bool) {
        if let (Some(editor), Some(layout)) = (&mut self.editor, &self.layout) {
            let position = if whole {
                Some(if right {
                    editor.end()
                } else {
                    Position::default()
                })
            } else {
                layout
                    .line_edge(
                        editor.document(),
                        editor.selection().focus,
                        self.upstream,
                        right,
                    )
                    .ok()
                    .flatten()
            };
            if let Some(position) = position {
                editor.set_position(position, extend);
            }
            self.upstream = right;
            self.preferred_x = None;
        }
        self.reveal();
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
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
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
        let mut state = Box::new(State::empty(dpi));
        if !smoke_test {
            match recovery::default_root() {
                Ok(root) => state.recovery_root = Some(root),
                Err(error) => {
                    state.message = format!("Автосохранение недоступно: {error}");
                    state.autosave_error = Some(error);
                    state.autosave_status = "Ошибка автосохранения";
                }
            }
        }
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
            SetTimer(handle, 2, 500, None);
            restore_session(handle, true);
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
        if !discard_allowed(window) {
            return;
        }
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
        sync_caret(window);
    }
}

/// No State reference crosses a modal/reentrant Windows call.
unsafe fn discard_allowed(window: HWND) -> bool {
    unsafe {
        let pointer = state_pointer(window);
        let dirty = !pointer.is_null() && (&*pointer).editor.as_ref().is_some_and(Editor::dirty);
        if !dirty {
            return true;
        }
        match MessageBoxW(
            window,
            windows_sys::w!(
                "В документе есть несохранённые изменения.\n\nСохранить их в новую копию DOCX перед продолжением?"
            ),
            windows_sys::w!("Paperik — несохранённые изменения"),
            MB_YESNOCANCEL | MB_ICONQUESTION | MB_DEFBUTTON3,
        ) {
            IDYES => save_copy(window),
            IDNO => true,
            _ => false,
        }
    }
}

unsafe fn restore_session(window: HWND, startup: bool) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        let Some(root) = (&*pointer).recovery_root.clone() else {
            return;
        };
        let mut skip = std::collections::BTreeSet::new();
        loop {
            let candidate = match recovery::find(&root, &skip) {
                Ok(Some(candidate)) => candidate,
                Ok(None) => {
                    if !startup {
                        (&mut *pointer).message =
                            "Несохранённых сессий для восстановления нет.".into();
                        refresh(window, false);
                    }
                    return;
                }
                Err(error) => {
                    (&mut *pointer).message = error;
                    refresh(window, false);
                    return;
                }
            };
            let text = format!(
                "Найдена несохранённая сессия: {}\n\nВосстановить текст, оформление и Undo/Redo?\n\nДа — восстановить. Нет — показать более раннюю сессию.\nОтмена — оставить на потом, через кнопку «Восстановить сессию».",
                candidate.name
            );
            let wide: Vec<_> = text.encode_utf16().chain(Some(0)).collect();
            match MessageBoxW(
                window,
                wide.as_ptr(),
                windows_sys::w!("Paperik — восстановление сессии"),
                MB_YESNOCANCEL | MB_ICONQUESTION,
            ) {
                IDYES => {
                    if !discard_allowed(window) {
                        return;
                    }
                    if let Err(error) = (&mut *pointer).restore(candidate) {
                        (&mut *pointer).message =
                            format!("Не удалось восстановить сессию: {error}");
                    }
                    refresh(window, true);
                    return;
                }
                IDNO => {
                    skip.insert(candidate.path().to_owned());
                }
                _ => return,
            }
        }
    }
}

fn save_dialog(window: HWND, suggested: &std::path::Path) -> Result<Option<PathBuf>, String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let units: Vec<_> = suggested.as_os_str().encode_wide().collect();
    if units.len() >= 32767 {
        return Err("Слишком длинный путь для диалога сохранения".into());
    }
    let mut buffer = vec![0u16; 32768];
    buffer[..units.len()].copy_from_slice(&units);
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: window,
        lpstrTitle: windows_sys::w!(
            "Сохранить новую копию DOCX — существующие файлы не заменяются"
        ),
        lpstrFilter: windows_sys::w!("Документы Word (*.docx)\0*.docx\0\0"),
        lpstrDefExt: windows_sys::w!("docx"),
        lpstrFile: buffer.as_mut_ptr(),
        nMaxFile: buffer.len() as u32,
        Flags: OFN_PATHMUSTEXIST | OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_NOREADONLYRETURN,
        ..Default::default()
    };
    unsafe {
        if GetSaveFileNameW(&mut dialog) != 0 {
            let end = buffer.iter().position(|u| *u == 0).unwrap_or(buffer.len());
            Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..end],
            ))))
        } else {
            let code = CommDlgExtendedError();
            if code == 0 {
                Ok(None)
            } else {
                Err(format!("Не удалось открыть диалог сохранения: {code:#x}"))
            }
        }
    }
}
unsafe fn save_copy(window: HWND) -> bool {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() || (&*pointer).editor.is_none() {
            return false;
        }
        // All values owned here: the modal dialog is allowed to pump window messages.
        let current = (&*pointer)
            .file_path
            .clone()
            .unwrap_or_else(|| PathBuf::from((&*pointer).filename.clone()));
        let stem = current
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let parent = current.parent().unwrap_or(std::path::Path::new("."));
        let mut suggested = parent.join(format!("{stem} — копия.docx"));
        for index in 2..10000 {
            if !suggested.exists() {
                break;
            }
            suggested = parent.join(format!("{stem} — копия {index}.docx"));
        }
        let result = match save_dialog(window, &suggested) {
            Ok(Some(path)) => (&mut *pointer).save_to(path),
            Ok(None) => return false,
            Err(error) => Err(error),
        };
        let success = result.is_ok();
        if let Err(error) = result {
            (&mut *pointer).message = error.clone();
            let text: Vec<_> = error.encode_utf16().chain(Some(0)).collect();
            MessageBoxW(
                window,
                text.as_ptr(),
                windows_sys::w!("Paperik — копия не сохранена"),
                MB_OK | MB_ICONERROR,
            );
        }
        refresh(window, false);
        success
    }
}
unsafe fn sync_caret(window: HWND) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        let geometry = {
            let state = &*pointer;
            if !state.focused {
                return;
            }
            state.editor.as_ref().and_then(|editor| {
                let caret = state.caret()?;
                let doc = editor.document();
                let (left, right, top, bottom) = render::panes(state.width, state.height);
                let x = left
                    + (right - left - doc.page.width * state.zoom) / 2.0
                    + caret.x * state.zoom;
                let y = top
                    + (24.0 + caret.page as f32 * (doc.page.height + 24.0) + caret.y
                        - state.scroll)
                        * state.zoom;
                let height = caret.height * state.zoom;
                let scale = state.dpi / 96.0;
                Some((
                    x,
                    y,
                    height,
                    left,
                    right,
                    top,
                    bottom,
                    scale,
                    editor.selection().collapsed(),
                ))
            })
        };
        DestroyCaret();
        if let Some((x, y, height, left, right, top, bottom, scale, collapsed)) = geometry {
            if collapsed && x >= left && x <= right && y >= top && y + height <= bottom {
                CreateCaret(
                    window,
                    null_mut(),
                    scale.round().max(1.0) as i32,
                    (height * scale).round().max(1.0) as i32,
                );
                SetCaretPos((x * scale).round() as i32, (y * scale).round() as i32);
                ShowCaret(window);
            }
            let context = ImmGetContext(window);
            if !context.is_null() {
                let point = POINT {
                    x: (x * scale).round() as i32,
                    y: (y * scale).round() as i32,
                };
                let composition = COMPOSITIONFORM {
                    dwStyle: CFS_POINT,
                    ptCurrentPos: point,
                    ..Default::default()
                };
                ImmSetCompositionWindow(context, &composition);
                let candidate = CANDIDATEFORM {
                    dwStyle: CFS_CANDIDATEPOS,
                    ptCurrentPos: POINT {
                        y: point.y + (height * scale) as i32,
                        ..point
                    },
                    ..Default::default()
                };
                ImmSetCandidateWindow(context, &candidate);
                ImmReleaseContext(window, context);
            }
        }
    }
}
unsafe fn refresh(window: HWND, reveal: bool) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        let title = {
            let state = &mut *pointer;
            if reveal {
                state.reveal();
            }
            state.clamp();
            format!(
                "{}{} — Paperik",
                if state.editor.as_ref().is_some_and(Editor::dirty) {
                    "* "
                } else {
                    ""
                },
                state.filename
            )
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>()
        };
        SetWindowTextW(window, title.as_ptr());
        sync_caret(window);
        InvalidateRect(window, null(), 0);
    }
}
unsafe fn edit(window: HWND, command: Edit) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        if let Err(error) = (&mut *pointer).apply(command) {
            (&mut *pointer).message = error;
        }
        refresh(window, true);
    }
}
unsafe fn drag(window: HWND) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        let state = &mut *pointer;
        if !state.dragging {
            return;
        }
        let (x, y) = state.drag_point;
        let (_, _, top, bottom) = render::panes(state.width, state.height);
        if y < top {
            state.scroll -= ((top - y) * 0.3).clamp(4.0, 48.0) / state.zoom;
        }
        if y > bottom {
            state.scroll += ((y - bottom) * 0.3).clamp(4.0, 48.0) / state.zoom;
        }
        state.clamp();
        if let Some((position, upstream)) = state.hit(x, y.clamp(top, bottom)) {
            if let Some(editor) = &mut state.editor {
                editor.set_position(position, true);
            }
            state.upstream = upstream;
        }
        refresh(window, false);
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
            WM_CLOSE => {
                if discard_allowed(window) {
                    if !pointer.is_null() {
                        (&mut *pointer).finish_autosave();
                    }
                    KillTimer(window, 2);
                    DestroyWindow(window);
                }
                0
            }
            WM_SETFOCUS if !pointer.is_null() => {
                (&mut *pointer).focused = true;
                sync_caret(window);
                0
            }
            WM_KILLFOCUS if !pointer.is_null() => {
                (&mut *pointer).focused = false;
                (&mut *pointer).input.clear();
                DestroyCaret();
                0
            }
            WM_DESTROY => {
                KillTimer(window, 2);
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
                sync_caret(window);
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
                HideCaret(window);
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
                ShowCaret(window);
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
                sync_caret(window);
                0
            }
            WM_KEYDOWN if !pointer.is_null() => {
                let ctrl = GetKeyState(VK_CONTROL as i32) < 0 && GetKeyState(VK_MENU as i32) >= 0;
                let shift = GetKeyState(VK_SHIFT as i32) < 0;
                if (&*pointer).composing {
                    return DefWindowProcW(window, message, wparam, lparam);
                }
                // VK_PACKET may occur before EACH surrogate of a Unicode input.
                // Only an actual navigation/control command cancels a pending pair.
                if ctrl
                    || matches!(
                        wparam as u16,
                        VK_LEFT
                            | VK_RIGHT
                            | VK_UP
                            | VK_DOWN
                            | VK_HOME
                            | VK_END
                            | VK_NEXT
                            | VK_PRIOR
                            | VK_BACK
                            | VK_DELETE
                            | VK_RETURN
                            | VK_TAB
                    )
                {
                    (&mut *pointer).input.clear();
                }
                if ctrl && wparam == b'O' as usize {
                    if let Some(path) = open_dialog(window) {
                        open_file(window, path);
                    }
                    return 0;
                }
                if ctrl && matches!(wparam as u8, b'C' | b'X') {
                    let text = (&*pointer)
                        .editor
                        .as_ref()
                        .map(Editor::selected_text)
                        .unwrap_or_default();
                    if !text.is_empty() {
                        match crate::clipboard::write(window, &text) {
                            Ok(()) if wparam == b'X' as usize => {
                                edit(window, Edit::Insert(String::new()))
                            }
                            Ok(()) => {}
                            Err(error) => (&mut *pointer).message = error,
                        }
                    }
                    refresh(window, false);
                    return 0;
                }
                if ctrl && wparam == b'V' as usize {
                    if (&*pointer).editor.is_some() {
                        match crate::clipboard::read(window) {
                            Ok(text) => edit(window, Edit::Insert(text)),
                            Err(error) => (&mut *pointer).message = error,
                        }
                    }
                    refresh(window, true);
                    return 0;
                }
                if ctrl && (wparam == b'Z' as usize || wparam == b'Y' as usize) {
                    edit(
                        window,
                        if shift || wparam == b'Y' as usize {
                            Edit::Redo
                        } else {
                            Edit::Undo
                        },
                    );
                    return 0;
                }
                if ctrl && wparam == b'S' as usize {
                    save_copy(window);
                    return 0;
                }
                if wparam == VK_BACK as usize || wparam == VK_DELETE as usize {
                    edit(
                        window,
                        if wparam == VK_BACK as usize {
                            Edit::Backspace { word: ctrl }
                        } else {
                            Edit::Delete { word: ctrl }
                        },
                    );
                    return 0;
                }
                if wparam == VK_RETURN as usize {
                    edit(
                        window,
                        if shift {
                            Edit::SoftBreak
                        } else {
                            Edit::Insert("\n".into())
                        },
                    );
                    return 0;
                }
                if wparam == VK_TAB as usize {
                    edit(window, Edit::Insert("\t".into()));
                    return 0;
                }
                let state = &mut *pointer;
                let (_, _, top, bottom) = render::panes(state.width, state.height);
                match wparam as u16 {
                    VK_HOME => state.move_edge(false, ctrl, shift),
                    VK_END => state.move_edge(true, ctrl, shift),
                    VK_NEXT | VK_PRIOR => {
                        let lines = ((bottom - top - 48.0) / state.zoom / 24.0).max(1.0) as isize;
                        state.move_vertical(
                            if wparam == VK_NEXT as usize {
                                lines
                            } else {
                                -lines
                            },
                            shift,
                        );
                    }
                    VK_DOWN => state.move_vertical(1, shift),
                    VK_UP => state.move_vertical(-1, shift),
                    VK_LEFT | VK_RIGHT => {
                        if let Some(editor) = &mut state.editor {
                            editor.move_horizontal(wparam == VK_RIGHT as usize, shift, ctrl);
                        }
                        state.upstream = false;
                        state.preferred_x = None;
                        state.reveal();
                    }
                    key if ctrl && key == b'A' as u16 => {
                        if let Some(editor) = &mut state.editor {
                            editor.select_all();
                        }
                    }
                    VK_ADD | VK_OEM_PLUS if ctrl => state.zoom = (state.zoom * 1.1).min(2.0),
                    VK_SUBTRACT | VK_OEM_MINUS if ctrl => state.zoom = (state.zoom / 1.1).max(0.25),
                    key if ctrl && key == b'0' as u16 => {
                        if let Some(editor) = &state.editor {
                            state.zoom =
                                render::fit_zoom(state.width, state.height, editor.document());
                        }
                    }
                    _ => return DefWindowProcW(window, message, wparam, lparam),
                }
                refresh(window, false);
                0
            }
            WM_CHAR if !pointer.is_null() => {
                let unit = wparam as u16;
                if unit >= 32 && unit != 127 && !(&*pointer).composing {
                    match (&mut *pointer).input.push(unit) {
                        Ok(Some(ch)) => edit(window, Edit::Insert(ch.to_string())),
                        Ok(None) => {}
                        Err(error) => {
                            (&mut *pointer).message = error.into();
                            refresh(window, false);
                        }
                    }
                }
                0
            }
            WM_UNICHAR if !pointer.is_null() => {
                if wparam == 0xffff {
                    return 1;
                }
                if let Some(ch) = char::from_u32(wparam as u32).filter(|ch| !ch.is_control()) {
                    edit(window, Edit::Insert(ch.to_string()));
                }
                0
            }
            WM_IME_STARTCOMPOSITION if !pointer.is_null() => {
                (&mut *pointer).composing = true;
                (&mut *pointer).input.clear();
                sync_caret(window);
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_IME_ENDCOMPOSITION if !pointer.is_null() => {
                (&mut *pointer).composing = false;
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_IME_COMPOSITION if !pointer.is_null() && (lparam as u32 & GCS_RESULTSTR) != 0 => {
                let context = ImmGetContext(window);
                if !context.is_null() {
                    let size = ImmGetCompositionStringW(context, GCS_RESULTSTR, null_mut(), 0);
                    let result = if (0..=2_000_000).contains(&size) && size % 2 == 0 {
                        let mut units = vec![0u16; size as usize / 2];
                        let count = ImmGetCompositionStringW(
                            context,
                            GCS_RESULTSTR,
                            units.as_mut_ptr().cast(),
                            size as u32,
                        );
                        if count == size {
                            String::from_utf16(&units).map_err(|_| "Повреждённый Unicode от IME")
                        } else {
                            Err("Не удалось получить результат IME")
                        }
                    } else {
                        Err("Неподдерживаемый размер результата IME")
                    };
                    ImmReleaseContext(window, context);
                    match result {
                        Ok(text) => edit(window, Edit::Insert(text)),
                        Err(error) => {
                            (&mut *pointer).message = error.into();
                            refresh(window, false);
                        }
                    }
                }
                // Do not let DefWindowProc generate a second WM_IME_CHAR for this result.
                let rest = lparam & !(GCS_RESULTSTR as isize);
                if rest != 0 {
                    DefWindowProcW(window, message, wparam, rest)
                } else {
                    0
                }
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK if !pointer.is_null() => {
                let state = &*pointer;
                let x = (lparam as u16 as i16) as f32 * 96.0 / state.dpi;
                let y = ((lparam >> 16) as u16 as i16) as f32 * 96.0 / state.dpi;
                let (left, right, top, bottom) = render::panes(state.width, state.height);
                if x > left && x < right && y >= top && y < bottom {
                    SetFocus(window);
                    if let Some((position, upstream)) = (&*pointer).hit(x, y) {
                        let state = &mut *pointer;
                        if let Some(editor) = &mut state.editor {
                            if message == WM_LBUTTONDBLCLK {
                                editor.select_word(position);
                            } else {
                                editor.set_position(position, GetKeyState(VK_SHIFT as i32) < 0);
                            }
                        }
                        state.upstream = upstream;
                        state.preferred_x = None;
                        state.input.clear();
                        state.dragging = message != WM_LBUTTONDBLCLK;
                        state.drag_point = (x, y);
                        if state.dragging {
                            SetCapture(window);
                            SetTimer(window, 1, 40, None);
                        }
                        refresh(window, false);
                    }
                }
                0
            }
            WM_MOUSEMOVE if !pointer.is_null() && (&*pointer).dragging => {
                let dpi = (&*pointer).dpi;
                (&mut *pointer).drag_point = (
                    (lparam as u16 as i16) as f32 * 96.0 / dpi,
                    ((lparam >> 16) as u16 as i16) as f32 * 96.0 / dpi,
                );
                drag(window);
                0
            }
            WM_TIMER if wparam == 1 && !pointer.is_null() => {
                drag(window);
                0
            }
            WM_TIMER if wparam == 2 && !pointer.is_null() => {
                if (&mut *pointer).autosave_tick(false) {
                    InvalidateRect(window, null(), 0);
                }
                0
            }
            WM_ACTIVATEAPP if wparam == 0 && !pointer.is_null() => {
                if (&mut *pointer).autosave_tick(true) {
                    InvalidateRect(window, null(), 0);
                }
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_CAPTURECHANGED if !pointer.is_null() => {
                (&mut *pointer).dragging = false;
                KillTimer(window, 1);
                0
            }
            WM_SETCURSOR if !pointer.is_null() && lparam as u16 == HTCLIENT as u16 => {
                let mut point = POINT::default();
                GetCursorPos(&mut point);
                ScreenToClient(window, &mut point);
                let state = &*pointer;
                let (left, right, top, bottom) = render::panes(state.width, state.height);
                let x = point.x as f32 * 96.0 / state.dpi;
                let y = point.y as f32 * 96.0 / state.dpi;
                let text =
                    state.editor.is_some() && x > left && x < right && y >= top && y < bottom;
                SetCursor(LoadCursorW(
                    null_mut(),
                    if text { IDC_IBEAM } else { IDC_ARROW },
                ));
                1
            }
            WM_LBUTTONUP if !pointer.is_null() => {
                if (&*pointer).dragging {
                    drag(window);
                    (&mut *pointer).dragging = false;
                    KillTimer(window, 1);
                    ReleaseCapture();
                    refresh(window, false);
                    return 0;
                }
                let state = &*pointer;
                let x = (lparam as u16 as i16) as f32 * 96.0 / state.dpi;
                let y = ((lparam >> 16) as u16 as i16) as f32 * 96.0 / state.dpi;
                let (left, _, _, _) = render::panes(state.width, state.height);
                if x >= 12.0 && x < left - 12.0 && (221.0..257.0).contains(&y) {
                    restore_session(window, false);
                    return 0;
                }
                if (14.0..50.0).contains(&y) && x >= state.width - 404.0 && x <= state.width - 256.0
                {
                    save_copy(window);
                    return 0;
                }
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
                    && y >= 317.0
                    && y < bottom
                    && let (Some(editor), Some(layout)) = (&state.editor, &state.layout)
                {
                    let doc = editor.document();
                    let current = (state.scroll / (doc.page.height + 24.0)).floor() as usize;
                    let index = current.saturating_sub(2) + ((y - 317.0) / 34.0) as usize;
                    if index < layout.pages.len() {
                        state.scroll = index as f32 * (doc.page.height + 24.0);
                    }
                }
                refresh(window, false);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_input_dispatch_edits_once_reflows_and_never_writes_source() {
        // Exercise our callback, not an input injector or any external window.
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: CLASS,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let _class = Class(instance);
            let mut state = Box::new(State::empty(96.0));
            let handle = CreateWindowExW(
                0,
                CLASS,
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
            let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/formatting.docx");
            let recovery_root = std::env::temp_dir()
                .join(format!("paperik-window-recovery-{}", std::process::id()));
            std::fs::create_dir(&recovery_root).unwrap();
            state.recovery_root = Some(recovery_root.clone());
            state.load(fixture.clone()).unwrap();
            let source = std::fs::read(&fixture).unwrap();
            let paragraphs = state.editor.as_ref().unwrap().document().paragraphs.clone();
            procedure(handle, WM_KEYDOWN, VK_PACKET as usize, 0);
            procedure(handle, WM_CHAR, 0xd83d, 0);
            procedure(handle, WM_KEYDOWN, VK_PACKET as usize, 0);
            procedure(handle, WM_CHAR, 0xde80, 0);
            assert!(
                state.editor.as_ref().unwrap().document().paragraphs[0]
                    .text()
                    .starts_with('🚀')
            );
            procedure(handle, WM_KEYDOWN, VK_BACK as usize, 0);
            procedure(handle, WM_CHAR, 8, 0);
            assert_eq!(
                state.editor.as_ref().unwrap().document().paragraphs,
                paragraphs
            );
            let count = paragraphs.len();
            procedure(handle, WM_KEYDOWN, VK_RETURN as usize, 0);
            procedure(handle, WM_CHAR, 13, 0);
            assert_eq!(
                state.editor.as_ref().unwrap().document().paragraphs.len(),
                count + 1
            );
            edit(handle, Edit::Undo);
            assert_eq!(
                state.editor.as_ref().unwrap().document().paragraphs,
                paragraphs
            );
            edit(handle, Edit::Redo);
            assert_eq!(
                state.editor.as_ref().unwrap().document().paragraphs.len(),
                count + 1
            );
            state
                .renderer
                .as_ref()
                .unwrap()
                .paint(&state.view())
                .unwrap();
            let geometry = state
                .layout
                .as_ref()
                .unwrap()
                .report(state.editor.as_ref().unwrap().document())
                .unwrap();
            state.autosave_tick(false);
            assert!(!state.autosave.as_ref().unwrap().busy);
            state.last_attempt = std::time::Instant::now() - std::time::Duration::from_secs(6);
            procedure(handle, WM_TIMER, 2, 0);
            assert!(state.autosave.as_ref().unwrap().busy);
            // Disconnecting the worker retains the draft, as an interrupted window would.
            drop(state.autosave.take());
            let recovered = recovery::find(&recovery_root, &std::collections::BTreeSet::new())
                .unwrap()
                .unwrap();
            state.restore(recovered).unwrap();
            assert_eq!(
                geometry,
                state
                    .layout
                    .as_ref()
                    .unwrap()
                    .report(state.editor.as_ref().unwrap().document())
                    .unwrap()
            );
            assert!(state.editor.as_ref().unwrap().dirty());
            let directory =
                std::env::temp_dir().join(format!("paperik-window-save-{}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            let copy = directory.join("edited.docx");
            state.save_to(copy.clone()).unwrap();
            assert!(!state.editor.as_ref().unwrap().dirty());
            let reopened = Document::open(&copy).unwrap();
            let layout = state
                .renderer
                .as_ref()
                .unwrap()
                .document(&reopened)
                .unwrap();
            assert_eq!(geometry, layout.report(&reopened).unwrap());
            state.apply(Edit::Insert("extra".into())).unwrap();
            assert!(state.save_to(copy.clone()).is_err());
            assert!(state.editor.as_ref().unwrap().dirty());
            state.apply(Edit::Undo).unwrap();
            assert!(!state.editor.as_ref().unwrap().dirty());
            std::fs::remove_file(copy).unwrap();
            std::fs::remove_dir(directory).unwrap();
            state.finish_autosave();
            assert!(
                recovery::find(&recovery_root, &std::collections::BTreeSet::new())
                    .unwrap()
                    .is_none()
            );
            std::fs::remove_dir(recovery_root).unwrap();
            assert_eq!(std::fs::read(&fixture).unwrap(), source);
            drop(window);
        }
    }
}
