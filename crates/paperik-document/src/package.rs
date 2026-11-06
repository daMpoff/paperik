use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
type Pictures = Arc<Mutex<BTreeMap<String, Arc<Vec<u8>>>>>;

const MAX_FILE: usize = 32 * 1024 * 1024;
pub(crate) const MAX_PART: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Entry {
    method: u16,
    flags: u16,
    crc: u32,
    packed: usize,
    unpacked: usize,
    offset: usize,
}

/// The source archive is retained byte-for-byte. No part is extracted to disk.
#[derive(Debug, Clone)]
pub struct Package {
    pub original: Arc<[u8]>,
    entries: Arc<BTreeMap<String, Entry>>,
    pub(crate) main: Option<Arc<MainPart>>,
    pictures: Pictures,
}

pub(crate) type PictureParts = BTreeMap<String, Arc<Vec<u8>>>;

#[derive(Debug)]
pub(crate) struct MainPart {
    pub name: String,
    pub xml: String,
    pub parts: PictureParts,
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or("Повреждён заголовок ZIP")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or("Повреждён заголовок ZIP")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

impl Package {
    pub fn parse(data: Vec<u8>) -> Result<Self, String> {
        if data.len() > MAX_FILE || data.len() < 22 {
            return Err("Файл не является поддерживаемым DOCX или превышает 32 МиБ".into());
        }
        let end = (data.len().saturating_sub(65_557)..=data.len() - 22)
            .rev()
            .find(|offset| {
                data.get(*offset..*offset + 4) == Some(b"PK\x05\x06")
                    && u16_at(&data, *offset + 20)
                        .is_ok_and(|length| *offset + 22 + usize::from(length) == data.len())
            })
            .ok_or("Не найден каталог ZIP")?;
        let count = usize::from(u16_at(&data, end + 10)?);
        if u16_at(&data, end + 4)? != 0
            || u16_at(&data, end + 6)? != 0
            || usize::from(u16_at(&data, end + 8)?) != count
            || count == 0
            || count > 2048
        {
            return Err(
                "Многотомный ZIP, ZIP64 или слишком большой каталог не поддерживается".into(),
            );
        }
        let central_size = u32_at(&data, end + 12)? as usize;
        let mut position = u32_at(&data, end + 16)? as usize;
        let central_end = position
            .checked_add(central_size)
            .ok_or("Некорректный каталог ZIP")?;
        if central_end != end {
            return Err("Границы каталога ZIP не совпадают".into());
        }
        let mut entries = BTreeMap::new();
        let mut names = std::collections::BTreeSet::new();
        let mut total = 0_usize;
        for _ in 0..count {
            if data.get(position..position + 4) != Some(b"PK\x01\x02") {
                return Err("Повреждена запись каталога ZIP".into());
            }
            let name_length = usize::from(u16_at(&data, position + 28)?);
            let extra_length = usize::from(u16_at(&data, position + 30)?);
            let comment_length = usize::from(u16_at(&data, position + 32)?);
            let name_start = position + 46;
            let name = std::str::from_utf8(
                data.get(name_start..name_start + name_length)
                    .ok_or("Повреждено имя части")?,
            )
            .map_err(|_| "Имя части ZIP не в UTF-8")?
            .to_owned();
            if name.starts_with('/')
                || name.contains('\\')
                || name.contains('\0')
                || name.split('/').any(|part| part == ".." || part == ".")
                || !names.insert(name.to_ascii_lowercase())
            {
                return Err("Небезопасное или повторяющееся имя части ZIP".into());
            }
            let entry = Entry {
                flags: u16_at(&data, position + 8)?,
                method: u16_at(&data, position + 10)?,
                crc: u32_at(&data, position + 16)?,
                packed: u32_at(&data, position + 20)? as usize,
                unpacked: u32_at(&data, position + 24)? as usize,
                offset: u32_at(&data, position + 42)? as usize,
            };
            total = total
                .checked_add(entry.unpacked)
                .ok_or("Слишком большой пакет")?;
            if entry.flags & 1 != 0
                || !matches!(entry.method, 0 | 8)
                || entry.unpacked > MAX_PART
                || total > MAX_TOTAL
                || entry.offset >= central_end
            {
                return Err(
                    "Зашифрованная часть, неподдерживаемое сжатие или превышен лимит пакета".into(),
                );
            }
            position = name_start + name_length + extra_length + comment_length;
            if position > central_end {
                return Err("Часть выходит за каталог ZIP".into());
            }
            entries.insert(name, entry);
        }
        if position != central_end {
            return Err("Некорректный размер каталога ZIP".into());
        }
        Ok(Self {
            original: Arc::from(data),
            entries: Arc::new(entries),
            main: None,
            pictures: Pictures::default(),
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.extra(name).is_some() || self.entries.contains_key(name)
    }

    pub fn text(&self, name: &str) -> Result<String, String> {
        String::from_utf8(self.bytes(name)?).map_err(|_| "XML части не в UTF-8".into())
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str).chain(
            self.main
                .iter()
                .flat_map(|p| p.parts.keys())
                .filter(|name| !self.entries.contains_key(*name))
                .map(String::as_str),
        )
    }
    pub(crate) fn picture_bytes(&self, name: &str) -> Result<Arc<Vec<u8>>, String> {
        if let Some(bytes) = self.extra(name) {
            return Ok(bytes.clone());
        }
        let mut pictures = self
            .pictures
            .lock()
            .map_err(|_| "Недоступен кэш рисунков")?;
        if let Some(bytes) = pictures.get(name) {
            return Ok(bytes.clone());
        }
        let bytes = Arc::new(self.original_bytes(name)?);
        pictures.insert(name.to_owned(), bytes.clone());
        Ok(bytes)
    }
    fn extra(&self, name: &str) -> Option<&Arc<Vec<u8>>> {
        self.main.as_ref()?.parts.get(name)
    }
    pub(crate) fn with_main(&self, name: String, xml: String) -> Result<Self, String> {
        let parts = self
            .main
            .as_ref()
            .map_or_else(BTreeMap::new, |p| p.parts.clone());
        self.with_picture_parts(name, xml, parts)
    }
    pub(crate) fn with_picture_parts(
        &self,
        name: String,
        xml: String,
        parts: PictureParts,
    ) -> Result<Self, String> {
        if self
            .names()
            .any(|n| n.to_ascii_lowercase().starts_with("_xmlsignatures/"))
        {
            return Err("Изменение DOCX с цифровой подписью пока не поддерживается".into());
        }
        let previous = self.entries.get(&name).ok_or("Нет основной части DOCX")?;
        let relationships = crate::part_relationships(&name);
        let mut total = self
            .entries
            .values()
            .map(|entry| entry.unpacked)
            .sum::<usize>()
            - previous.unpacked
            + xml.len();
        let mut count = self.entries.len();
        for (part, bytes) in &parts {
            if (part != &relationships
                && part != "[Content_Types].xml"
                && !owned_picture_name(part))
                || bytes.len() > MAX_PART
            {
                return Err("Некорректная часть рисунка DOCX".into());
            }
            total += bytes.len();
            if let Some(entry) = self.entries.get(part) {
                if owned_picture_name(part) {
                    return Err("Имя нового рисунка совпадает с исходной частью".into());
                }
                total -= entry.unpacked;
            } else {
                if self
                    .entries
                    .keys()
                    .any(|existing| existing.eq_ignore_ascii_case(part))
                {
                    return Err("Имя части совпадает с исходной частью".into());
                }
                count += 1;
            }
        }
        if xml.len() > MAX_PART || total > MAX_TOTAL || count > 2048 {
            return Err("Превышен предел размера изменённого DOCX".into());
        }
        let mut result = self.clone();
        result.main = Some(Arc::new(MainPart { name, xml, parts }));
        Ok(result)
    }

    pub fn bytes(&self, name: &str) -> Result<Vec<u8>, String> {
        if let Some(part) = &self.main
            && part.name == name
        {
            return Ok(part.xml.as_bytes().to_vec());
        }
        if let Some(bytes) = self.extra(name) {
            return Ok(bytes.as_ref().clone());
        }
        self.original_bytes(name)
    }
    fn original_bytes(&self, name: &str) -> Result<Vec<u8>, String> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| format!("Не найдена часть {name}"))?;
        let bytes = &self.original;
        let offset = entry.offset;
        if bytes.get(offset..offset + 4) != Some(b"PK\x03\x04")
            || u16_at(bytes, offset + 6)? != entry.flags
            || u16_at(bytes, offset + 8)? != entry.method
        {
            return Err("Локальный заголовок ZIP не совпадает с каталогом".into());
        }
        let name_start = offset + 30;
        let name_length = usize::from(u16_at(bytes, offset + 26)?);
        if bytes.get(name_start..name_start + name_length) != Some(name.as_bytes()) {
            return Err("Имя части не совпадает с каталогом".into());
        }
        let start = name_start + name_length + usize::from(u16_at(bytes, offset + 28)?);
        let compressed = bytes
            .get(start..start + entry.packed)
            .ok_or("Повреждено содержимое части")?;
        let mut result = Vec::with_capacity(entry.unpacked);
        match entry.method {
            0 => result.extend_from_slice(compressed),
            8 => {
                flate2::read::DeflateDecoder::new(compressed)
                    .take((MAX_PART + 1) as u64)
                    .read_to_end(&mut result)
                    .map_err(|error| format!("Ошибка распаковки: {error}"))?;
            }
            _ => unreachable!(),
        }
        if result.len() != entry.unpacked || crc32(&result) != entry.crc {
            return Err("Размер или контрольная сумма части не совпадают".into());
        }
        Ok(result)
    }
    /// Rebuild only the ZIP directory/headers. Untouched compressed payloads are copied.
    pub(crate) fn replace(&self, changes: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
        if changes.keys().any(|name| !self.contains(name)) {
            return Err("Нельзя добавить неизвестную часть в пакет".into());
        }
        let mut output = Vec::new();
        let mut central = Vec::new();
        let mut count = 0u16;
        for name in self.names() {
            count += 1;
            let entry = self.entries.get(name);
            // Validate even opaque binary parts; corrupt data must not be republished silently.
            if entry.is_some() {
                self.original_bytes(name)?;
            }
            let replacement = changes
                .get(name)
                .map(Vec::as_slice)
                .or_else(|| {
                    self.main
                        .as_ref()
                        .filter(|part| part.name == name)
                        .map(|part| part.xml.as_bytes())
                })
                .or_else(|| self.extra(name).map(|bytes| bytes.as_slice()));
            let (encoded, method, crc, unpacked) = if let Some(bytes) = replacement {
                if bytes.len() > MAX_PART {
                    return Err("Изменённая часть DOCX превышает 16 МиБ".into());
                }
                let mut encoder =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(bytes).map_err(|e| e.to_string())?;
                (
                    encoder.finish().map_err(|e| e.to_string())?,
                    8,
                    crc32(bytes),
                    bytes.len(),
                )
            } else {
                let entry = entry.ok_or("Не найдена часть изменённого пакета")?;
                let start = entry.offset
                    + 30
                    + usize::from(u16_at(&self.original, entry.offset + 26)?)
                    + usize::from(u16_at(&self.original, entry.offset + 28)?);
                (
                    self.original[start..start + entry.packed].to_vec(),
                    entry.method,
                    entry.crc,
                    entry.unpacked,
                )
            };
            let offset = output.len() as u32;
            output.extend_from_slice(b"PK\x03\x04");
            output.extend(20u16.to_le_bytes());
            output.extend(0x0800u16.to_le_bytes());
            output.extend(method.to_le_bytes());
            output.extend(0u16.to_le_bytes());
            output.extend(0x0021u16.to_le_bytes());
            output.extend(crc.to_le_bytes());
            output.extend((encoded.len() as u32).to_le_bytes());
            output.extend((unpacked as u32).to_le_bytes());
            output.extend((name.len() as u16).to_le_bytes());
            output.extend(0u16.to_le_bytes());
            output.extend(name.as_bytes());
            output.extend(&encoded);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend(20u16.to_le_bytes());
            central.extend(20u16.to_le_bytes());
            central.extend(0x0800u16.to_le_bytes());
            central.extend(method.to_le_bytes());
            central.extend(0u16.to_le_bytes());
            central.extend(0x0021u16.to_le_bytes());
            central.extend(crc.to_le_bytes());
            central.extend((encoded.len() as u32).to_le_bytes());
            central.extend((unpacked as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0u8; 12]);
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
            if output.len() + central.len() > MAX_FILE {
                return Err("Сохранённый пакет превышает 32 МиБ".into());
            }
        }
        let offset = output.len() as u32;
        let size = central.len() as u32;
        output.extend(central);
        output.extend_from_slice(b"PK\x05\x06");
        output.extend([0u8; 4]);
        output.extend(count.to_le_bytes());
        output.extend(count.to_le_bytes());
        output.extend(size.to_le_bytes());
        output.extend(offset.to_le_bytes());
        output.extend(0u16.to_le_bytes());
        if output.len() > MAX_FILE {
            return Err("Сохранённый пакет превышает 32 МиБ".into());
        }
        Self::parse(output.clone())?;
        Ok(output)
    }
}

pub(crate) fn owned_picture_name(name: &str) -> bool {
    name.strip_prefix("word/media/paperik-image-")
        .and_then(|name| name.split_once('.'))
        .is_some_and(|(number, extension)| {
            !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
                && matches!(extension, "png" | "jpg")
        })
}
