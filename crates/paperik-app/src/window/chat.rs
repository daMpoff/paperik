use super::*;
use crate::chat_ui;
use windows_sys::Win32::UI::Controls::EM_SETSEL;

pub(super) fn is_command(value: WPARAM) -> bool {
    let id = value as u16;
    let notification = (value >> 16) as u16;
    (id == chat_ui::MODEL && notification == CBN_SELCHANGE as u16)
        || (matches!(
            id,
            chat_ui::REFRESH | chat_ui::SEND | chat_ui::STOP | chat_ui::CLEAR | chat_ui::TOGGLE
        ) && notification == BN_CLICKED as u16)
}
pub(super) unsafe fn update(window: HWND, command: Option<u16>, poll: bool) {
    unsafe {
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return;
        }
        // No borrow of State crosses Create/SendMessage/MoveWindow or a native control callback.
        let Some(mut panel) = (&mut *pointer).chat.take() else {
            return;
        };
        let (width, height, dpi) = ((&*pointer).width, (&*pointer).height, (&*pointer).dpi);
        let warnings = (&*pointer)
            .editor
            .as_ref()
            .map_or(0, |e| e.document().warnings.len())
            + (&*pointer)
                .layout
                .as_ref()
                .map_or(0, |l| l.missing_fonts.len());
        if let Some(id) = command {
            panel.command(id);
        }
        let changed = if poll { panel.poll() } else { false };
        panel.layout(width, height, dpi, warnings, (&*pointer).dark);
        (&mut *pointer).chat_show = panel.chat_mode;
        (&mut *pointer).ollama_status = panel.status.clone();
        (&mut *pointer).chat = Some(panel);
        if changed || command.is_some() {
            InvalidateRect(window, null(), 0);
        }
    }
}
pub(super) unsafe fn pretranslate(window: HWND, msg: &MSG) -> bool {
    unsafe {
        if msg.message != WM_KEYDOWN {
            return false;
        }
        let pointer = state_pointer(window);
        if pointer.is_null() {
            return false;
        }
        let focus = GetFocus();
        let Some((owns, input, log)) = (&*pointer)
            .chat
            .as_ref()
            .map(|p| (p.owns(focus), p.input_handle(), p.log_handle()))
        else {
            return false;
        };
        if msg.wParam == VK_F6 as usize {
            if owns {
                SetFocus(window);
            } else if let Some(panel) = (&mut *pointer).chat.take() {
                panel.focus();
                (&mut *pointer).chat = Some(panel);
            }
            return true;
        }
        if !owns {
            return false;
        }
        if msg.wParam == VK_ESCAPE as usize {
            PostMessageW(window, WM_COMMAND, chat_ui::STOP as usize, 0);
            return true;
        }
        if msg.wParam == VK_RETURN as usize && focus == input && GetKeyState(VK_SHIFT as i32) >= 0 {
            PostMessageW(window, WM_COMMAND, chat_ui::SEND as usize, 0);
            return true;
        }
        if msg.wParam == b'A' as usize
            && GetKeyState(VK_CONTROL as i32) < 0
            && (focus == input || focus == log)
        {
            SendMessageW(focus, EM_SETSEL, 0, -1);
            return true;
        }
        msg.wParam == VK_TAB as usize && IsDialogMessageW(window, msg) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{Ipv4Addr, TcpListener, TcpStream},
        time::{Duration, Instant},
    };
    fn read_request(socket: &mut TcpStream) -> Vec<u8> {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        let mut byte = [0];
        while !header.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
            assert!(header.len() < 32768);
        }
        let header = String::from_utf8(header).unwrap();
        let n = header
            .lines()
            .find_map(|s| s.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; n];
        socket.read_exact(&mut body).unwrap();
        body
    }
    fn reply(socket: &mut TcpStream, body: &str) {
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }
    unsafe fn text(handle: HWND) -> String {
        unsafe {
            let n = GetWindowTextLengthW(handle);
            let mut b = vec![0u16; n as usize + 1];
            GetWindowTextW(handle, b.as_mut_ptr(), b.len() as i32);
            String::from_utf16(&b[..n as usize]).unwrap()
        }
    }
    unsafe fn until(window: HWND, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline);
            unsafe {
                update(window, None, true);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn native_chat_selects_streams_cancels_and_never_edits_the_document() {
        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = server.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut tags, _) = server.accept().unwrap();
            read_request(&mut tags);
            reply(&mut tags, r#"{"models":[{"name":"synthetic-model"}]}"#);
            let (mut show, _) = server.accept().unwrap();
            read_request(&mut show);
            reply(&mut show, "{}");
            let (mut socket, _) = server.accept().unwrap();
            let body = String::from_utf8(read_request(&mut socket)).unwrap();
            assert!(body.contains("ui-synthetic"));
            assert!(!body.contains("Контрольный"));
            socket.write_all("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"message\":{\"content\":\"Первый фрагмент 🚀\"},\"done\":false}\n".as_bytes()).unwrap();
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).unwrap(), 0); // Stop closes the local stream.
        });
        unsafe {
            let name = windows_sys::w!("Paperik.ChatTest");
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: name,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let mut state = Box::new(State::empty(96.0));
            let handle = CreateWindowExW(
                0,
                name,
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
            state.editor = Some(Editor::new(
                Document::from_bytes(
                    include_bytes!("../../../../tests/fixtures/formatting.docx").to_vec(),
                )
                .unwrap(),
            ));
            let original = state.editor.as_ref().unwrap().document().paragraphs.clone();
            state.chat = Some(
                chat_ui::Panel::with_client(
                    handle,
                    96.0,
                    crate::ollama::Client::at(address).unwrap(),
                )
                .unwrap(),
            );
            update(handle, Some(chat_ui::REFRESH), false);
            let model = GetDlgItem(handle, chat_ui::MODEL as i32);
            let input = GetDlgItem(handle, 6004);
            let transcript = GetDlgItem(handle, 6003);
            until(handle, || {
                SendMessageW(model, CB_GETCOUNT, 0, 0) == 1 && IsWindowEnabled(model) != 0
            });
            assert_eq!(SendMessageW(model, CB_GETCURSEL, 0, 0), -1);
            SendMessageW(model, CB_SETCURSEL, 0, 0);
            SendMessageW(
                handle,
                WM_COMMAND,
                chat_ui::MODEL as usize | ((CBN_SELCHANGE as usize) << 16),
                0,
            );
            SendMessageW(input, WM_CHAR, b'U' as usize, 0);
            assert_eq!(
                state.editor.as_ref().unwrap().document().paragraphs,
                original
            );
            SetWindowTextW(input, windows_sys::w!("ui-synthetic"));
            SendMessageW(handle, WM_COMMAND, chat_ui::SEND as usize, 0);
            until(handle, || text(transcript).contains("Первый фрагмент 🚀"));
            assert_ne!(IsWindowEnabled(GetDlgItem(handle, chat_ui::STOP as i32)), 0);
            assert_eq!(text(input), "");
            assert_eq!(
                state.editor.as_ref().unwrap().document().paragraphs,
                original
            );
            SendMessageW(handle, WM_COMMAND, chat_ui::STOP as usize, 0);
            until(handle, || text(transcript).contains("Запрос отменён"));
            assert!(text(transcript).contains("Первый фрагмент 🚀"));
            assert!(!state.editor.as_ref().unwrap().dirty());
            SendMessageW(handle, WM_COMMAND, chat_ui::TOGGLE as usize, 0);
            assert!(!state.chat_show);
            SendMessageW(handle, WM_COMMAND, chat_ui::TOGGLE as usize, 0);
            assert!(state.chat_show);
            SendMessageW(handle, WM_COMMAND, chat_ui::CLEAR as usize, 0);
            assert_eq!(text(transcript), "");
            drop(window);
            drop(state);
            UnregisterClassW(name, instance);
        }
        peer.join().unwrap();
    }
}
