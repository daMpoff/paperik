use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColorBrush};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GetClientRect, GetMessageW, IDC_ARROW, IsWindow, LoadCursorW, MB_ICONERROR,
    MB_OK, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow, TranslateMessage, UnregisterClassW,
    WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

const WINDOW_CLASS: *const u16 = windows_sys::w!("Paperik.MainWindow");
const WINDOW_TITLE: *const u16 = windows_sys::w!("Paperik — первая сборка");

struct RegisteredClass {
    instance: windows_sys::Win32::Foundation::HINSTANCE,
}

impl Drop for RegisteredClass {
    fn drop(&mut self) {
        // SAFETY: the class name is static and the window is dropped first.
        unsafe { UnregisterClassW(WINDOW_CLASS, self.instance) };
    }
}

struct OwnedWindow(HWND);

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        // SAFETY: this handle belongs to the current UI thread. A closed window
        // has already been destroyed by the message handler.
        unsafe {
            if IsWindow(self.0) != 0 {
                DestroyWindow(self.0);
            }
        }
    }
}

pub fn run(smoke_test: bool) -> io::Result<()> {
    // SAFETY: setup and the entire message loop run on this thread. Constant
    // strings are NUL-terminated; pointer arguments remain valid during calls.
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
            let error = io::Error::last_os_error();
            // A compatibility setting may already have configured the process.
            if error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) {
                return Err(error);
            }
        }

        let instance = GetModuleHandleW(null());
        if instance.is_null() {
            return Err(io::Error::last_os_error());
        }
        let cursor = LoadCursorW(null_mut(), IDC_ARROW);
        if cursor.is_null() {
            return Err(io::Error::last_os_error());
        }
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: cursor,
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(io::Error::last_os_error());
        }
        let _registered_class = RegisteredClass { instance };

        let dpi = GetDpiForSystem().max(96);
        let mut rectangle = RECT {
            left: 0,
            top: 0,
            right: (1120 * dpi / 96) as i32,
            bottom: (720 * dpi / 96) as i32,
        };
        if AdjustWindowRectExForDpi(&mut rectangle, WS_OVERLAPPEDWINDOW, 0, 0, dpi) == 0 {
            return Err(io::Error::last_os_error());
        }
        let handle = CreateWindowExW(
            0,
            WINDOW_CLASS,
            WINDOW_TITLE,
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rectangle.right - rectangle.left,
            rectangle.bottom - rectangle.top,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let window = OwnedWindow(handle);

        if smoke_test {
            // Exercise real window creation, resizing and message-loop shutdown
            // without showing a window or requiring a running AI provider.
            if SetWindowPos(
                window.0,
                null_mut(),
                0,
                0,
                960,
                640,
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut client = RECT::default();
            if GetClientRect(window.0, &mut client) == 0 {
                return Err(io::Error::last_os_error());
            }
            if client.right <= client.left || client.bottom <= client.top {
                return Err(io::Error::other("Окно не получило рабочую область"));
            }
            if PostMessageW(window.0, WM_CLOSE, 0, 0) == 0 {
                return Err(io::Error::last_os_error());
            }
        } else {
            ShowWindow(window.0, SW_SHOW);
        }

        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ => {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DESTROY => {
            // SAFETY: this callback runs on the owning message-loop thread.
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_DPICHANGED if lparam != 0 => {
            // SAFETY: Windows supplies this RECT for the duration of WM_DPICHANGED.
            // No pointer is retained beyond this callback.
            unsafe {
                let suggested = &*(lparam as *const RECT);
                SetWindowPos(
                    window,
                    null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            0
        }
        // SAFETY: unhandled messages and their original parameters are forwarded.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

pub fn show_error(message: &str) {
    let wide_message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the message buffer stays alive for this synchronous call.
    unsafe {
        MessageBoxW(
            null_mut(),
            wide_message.as_ptr(),
            WINDOW_TITLE,
            MB_OK | MB_ICONERROR,
        )
    };
}
