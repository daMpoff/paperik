//! Small bounded HTTP/1.1 client for the fixed loopback Ollama API. No proxy or redirect handling.
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const MAX_HEADERS: usize = 32 * 1024;
const MAX_WIRE: usize = 8 * 1024 * 1024;

pub(super) fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Запрос отменён".into())
    } else {
        Ok(())
    }
}
struct Reader<'a> {
    socket: TcpStream,
    buffer: [u8; 8192],
    at: usize,
    end: usize,
    cancel: &'a AtomicBool,
    started: Instant,
    last_read: Instant,
    idle: Duration,
    total: Duration,
}
impl Reader<'_> {
    fn byte(&mut self) -> Result<Option<u8>, String> {
        loop {
            check(self.cancel)?;
            if self.started.elapsed() > self.total || self.last_read.elapsed() > self.idle {
                return Err("Ollama не ответила вовремя. Частичный ответ сохранён в чате.".into());
            }
            if self.at < self.end {
                let byte = self.buffer[self.at];
                self.at += 1;
                return Ok(Some(byte));
            }
            match self.socket.read(&mut self.buffer) {
                Ok(0) => return Ok(None),
                Ok(n) => {
                    self.at = 0;
                    self.end = n;
                    self.last_read = Instant::now();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err("Соединение с Ollama прервано".into()),
            }
        }
    }
    fn line(&mut self, max: usize) -> Result<Vec<u8>, String> {
        let mut line = Vec::new();
        loop {
            let byte = self.byte()?.ok_or("Ollama оборвала HTTP-ответ")?;
            line.push(byte);
            if line.len() > max {
                return Err("Слишком длинная строка HTTP".into());
            }
            if byte == b'\n' {
                if line.len() < 2 || line[line.len() - 2] != b'\r' {
                    return Err("Некорректный HTTP-ответ".into());
                }
                line.truncate(line.len() - 2);
                return Ok(line);
            }
        }
    }
}
enum Framing {
    Chunked { left: usize, separator: bool },
    Length(usize),
    Close,
}
pub(super) struct Response<'a> {
    reader: Reader<'a>,
    framing: Framing,
    count: usize,
    finished: bool,
    pub status: u16,
}
impl<'a> Response<'a> {
    pub fn open(
        address: SocketAddr,
        path: &str,
        body: Option<&str>,
        cancel: &'a AtomicBool,
    ) -> Result<Self, String> {
        Self::open_with_timeout(
            address,
            path,
            body,
            cancel,
            Duration::from_secs(120),
            Duration::from_secs(300),
        )
    }
    pub fn open_with_timeout(
        address: SocketAddr,
        path: &str,
        body: Option<&str>,
        cancel: &'a AtomicBool,
        idle: Duration,
        total: Duration,
    ) -> Result<Self, String> {
        check(cancel)?;
        if !address.ip().is_loopback() {
            return Err("Разрешена только локальная Ollama".into());
        }
        let mut socket =
            TcpStream::connect_timeout(&address, Duration::from_secs(2)).map_err(|_| {
                "Ollama недоступна на 127.0.0.1:11434. Запустите Ollama и нажмите «Обновить»."
                    .to_owned()
            })?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        let data = body.unwrap_or("");
        let method = if body.is_some() { "POST" } else { "GET" };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json, application/x-ndjson\r\nContent-Length: {}\r\n\r\n{data}",
            data.len()
        );
        check(cancel)?;
        socket
            .write_all(request.as_bytes())
            .map_err(|_| "Не удалось отправить запрос Ollama")?;
        let mut reader = Reader {
            socket,
            buffer: [0; 8192],
            at: 0,
            end: 0,
            cancel,
            started: Instant::now(),
            last_read: Instant::now(),
            idle,
            total,
        };
        let status = reader.line(4096)?;
        let status = std::str::from_utf8(&status).map_err(|_| "Некорректный HTTP-статус")?;
        let mut fields = status.split_whitespace();
        if !matches!(fields.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
            return Err("Некорректный HTTP-статус".into());
        }
        let status: u16 = fields
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or("Некорректный HTTP-код")?;
        let mut length = None;
        let mut chunked = false;
        let mut bytes = 0;
        loop {
            let line = reader.line(MAX_HEADERS)?;
            bytes += line.len() + 2;
            if bytes > MAX_HEADERS {
                return Err("Слишком большие заголовки HTTP".into());
            }
            if line.is_empty() {
                break;
            }
            let text = std::str::from_utf8(&line).map_err(|_| "Некорректные заголовки HTTP")?;
            let (key, value) = text.split_once(':').ok_or("Некорректный заголовок HTTP")?;
            match key.to_ascii_lowercase().as_str() {
                "content-length" => {
                    if length.is_some() {
                        return Err("Дублирующий Content-Length".into());
                    }
                    let n: usize = value
                        .trim()
                        .parse()
                        .map_err(|_| "Некорректная длина HTTP")?;
                    if n > MAX_WIRE {
                        return Err("Ответ Ollama слишком велик".into());
                    }
                    length = Some(n);
                }
                "transfer-encoding" => {
                    if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                        return Err("Неподдерживаемое HTTP-кодирование".into());
                    }
                    chunked = true;
                }
                "content-encoding" if !value.trim().eq_ignore_ascii_case("identity") => {
                    return Err("Сжатый HTTP-ответ не поддерживается".into());
                }
                _ => {}
            }
        }
        if chunked && length.is_some() {
            return Err("Неоднозначная длина HTTP-ответа".into());
        }
        let framing = if chunked {
            Framing::Chunked {
                left: 0,
                separator: false,
            }
        } else if let Some(n) = length {
            Framing::Length(n)
        } else {
            Framing::Close
        };
        Ok(Self {
            reader,
            framing,
            count: 0,
            finished: false,
            status,
        })
    }
    pub fn byte(&mut self) -> Result<Option<u8>, String> {
        if self.finished {
            return Ok(None);
        }
        match &mut self.framing {
            Framing::Length(0) => {
                self.finished = true;
                return Ok(None);
            }
            Framing::Length(left) => *left -= 1,
            Framing::Chunked { left, separator } => {
                if *left == 0 {
                    if *separator && !self.reader.line(2)?.is_empty() {
                        return Err("Некорректный разделитель HTTP-chunk".into());
                    }
                    let line = self.reader.line(256)?;
                    let size = std::str::from_utf8(&line)
                        .map_err(|_| "Некорректный HTTP-chunk")?
                        .split(';')
                        .next()
                        .unwrap_or("");
                    *left = usize::from_str_radix(size.trim(), 16)
                        .map_err(|_| "Некорректная длина HTTP-chunk")?;
                    if *left > MAX_WIRE - self.count {
                        return Err("Ответ Ollama слишком велик".into());
                    }
                    if *left == 0 {
                        let mut trailers = 0;
                        loop {
                            let tail = self.reader.line(MAX_HEADERS)?;
                            trailers += tail.len() + 2;
                            if trailers > MAX_HEADERS {
                                return Err("Слишком большие HTTP-trailers".into());
                            }
                            if tail.is_empty() {
                                break;
                            }
                        }
                        self.finished = true;
                        return Ok(None);
                    }
                    *separator = true;
                }
                *left -= 1;
            }
            Framing::Close => {}
        }
        let byte = self.reader.byte()?;
        if byte.is_none() {
            if !matches!(self.framing, Framing::Close) {
                return Err("Ollama оборвала HTTP-ответ".into());
            }
            self.finished = true;
            return Ok(None);
        }
        self.count += 1;
        if self.count > MAX_WIRE {
            return Err("Ответ Ollama слишком велик".into());
        }
        Ok(byte)
    }
    pub fn all(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        while let Some(byte) = self.byte()? {
            body.push(byte);
            if body.len() > limit {
                return Err("Ответ Ollama слишком велик".into());
            }
        }
        Ok(body)
    }
}
