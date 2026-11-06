use crate::layout::{self, Layout};
use paperik_document::Document;
use paperik_document::editor::Selection;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::core::{Result, w};
use windows_numerics::{Matrix3x2, Vector2};

const TOKENS: &str = include_str!("../../../assets/brand/paperik-tokens.json");
const MARK: &str = include_str!("../../../assets/brand/paperik-mark.svg");
const MARK_REVERSE: &str = include_str!("../../../assets/brand/paperik-mark-reverse.svg");
const WORDMARK: &str = include_str!("../../../assets/brand/paperik-wordmark.svg");

struct TextStyle {
    size: f32,
    color: [u8; 3],
    bold: bool,
}

pub struct Renderer {
    factory: ID2D1Factory,
    pub text: IDWriteFactory,
    target: ID2D1HwndRenderTarget,
}
pub struct View<'a> {
    pub width: f32,
    pub height: f32,
    pub zoom: f32,
    pub scroll: f32,
    pub dark: bool,
    pub document: Option<&'a Document>,
    pub layout: Option<&'a Layout>,
    pub filename: &'a str,
    pub message: &'a str,
    pub selection: Option<Selection>,
    pub dirty: bool,
    pub autosave_status: &'a str,
    pub recovery_enabled: bool,
    pub chat_active: bool,
    pub ollama_status: &'a str,
}

pub fn panes(width: f32, height: f32) -> (f32, f32, f32, f32) {
    let left = if width < 1100.0 { 196.0 } else { 224.0 };
    let right = if width < 1000.0 { 0.0 } else { 280.0 };
    (left, width - right, 144.0, (height - 30.0).max(145.0))
}
pub fn fit_zoom(width: f32, height: f32, document: &Document) -> f32 {
    let (left, right, _, _) = panes(width, height);
    ((right - left - 48.0) / document.page.width).clamp(0.25, 1.0)
}
pub fn max_scroll(view: &View<'_>) -> f32 {
    match (view.document, view.layout) {
        (Some(doc), Some(layout)) => {
            let (_, _, top, bottom) = panes(view.width, view.height);
            ((doc.page.height + 24.0) * layout.pages.len() as f32 + 24.0
                - (bottom - top) / view.zoom)
                .max(0.0)
        }
        _ => 0.0,
    }
}
pub(crate) fn rgb(name: &str) -> [u8; 3] {
    let marker = format!("\"{name}\"");
    let value = TOKENS
        .split_once(&marker)
        .and_then(|(_, s)| s.split_once('#'))
        .map(|(_, s)| &s[..6])
        .unwrap_or("182E28");
    let color = u32::from_str_radix(value, 16).unwrap_or(0x182e28);
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}
fn rect(x: f32, y: f32, width: f32, height: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}
fn matrix(scale: f32, x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: scale,
        M12: 0.0,
        M21: 0.0,
        M22: scale,
        M31: x,
        M32: y,
    }
}

pub fn draw_page(target: &ID2D1RenderTarget, page: &layout::PageLayout, width: f32) -> Result<()> {
    // The screen and diagnostic PNGs share this exact drawing path.
    unsafe {
        let black = target.CreateSolidColorBrush(&layout::color([0, 0, 0]), None)?;
        for cell in &page.cells {
            let [x, y, w, h] = cell.rect;
            if let Some(fill) = cell.fill {
                let brush = target.CreateSolidColorBrush(&layout::color(fill), None)?;
                target.FillRectangle(&rect(x, y, w, h), &brush);
            }
            let edges = [
                (x, y, x + w, y),
                (x + w, y, x + w, y + h),
                (x, y + h, x + w, y + h),
                (x, y, x, y + h),
            ];
            for (border, (x1, y1, x2, y2)) in cell.borders.iter().zip(edges) {
                if let Some(border) = border {
                    let brush = target.CreateSolidColorBrush(&layout::color(border.color), None)?;
                    target.DrawLine(
                        Vector2 { X: x1, Y: y1 },
                        Vector2 { X: x2, Y: y2 },
                        &brush,
                        border.width,
                        None,
                    );
                }
            }
        }
        for image in &page.images {
            let pixels = &image.pixels;
            let bitmap = target.CreateBitmap(
                D2D_SIZE_U {
                    width: pixels.width,
                    height: pixels.height,
                },
                Some(pixels.bytes.as_ptr().cast()),
                pixels.width * 4,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                },
            )?;
            let [x, y, w, h] = image.rect;
            target.DrawBitmap(
                &bitmap,
                Some(&rect(x, y, w, h)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        for line in page.lines.iter().chain(page.footer.iter()) {
            // Italic overhangs may extend beyond the text column; the page is
            // the horizontal clipping boundary, not the nominal advance box.
            let mut clip = if let Some([x, y, w, h]) = line.clip {
                rect(x, y, w, h)
            } else {
                rect(0.0, line.y, width, line.height)
            };
            if line.layout.has_inline() {
                // DrawTextLayout paints every native line. Clip to this line after moving its baseline.
                let top = line.y + line.draw_offset;
                clip.top = clip.top.max(line.y).max(top);
                clip.bottom = clip
                    .bottom
                    .min(line.y + line.height)
                    .min(top + line.native_height);
                if clip.bottom <= clip.top {
                    continue;
                }
            }
            target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            let start = line.layout_text_start;
            let end = start + line.layout_length;
            let colors = &line.layout.colors;
            let first =
                colors.partition_point(|(range, _)| range.startPosition + range.length <= start);
            for (range, rgb) in colors[first..]
                .iter()
                .take_while(|(range, _)| range.startPosition < end)
            {
                let lo = range.startPosition.max(start);
                let hi = (range.startPosition + range.length).min(end);
                let visible = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
                    startPosition: lo,
                    length: hi - lo,
                };
                let brush = target.CreateSolidColorBrush(&layout::color(*rgb), None)?;
                line.layout.native.SetDrawingEffect(&brush, visible)?;
            }
            target.DrawTextLayout(
                Vector2 {
                    X: line.x,
                    Y: line.y - line.source_y + line.draw_offset,
                },
                &line.layout.native,
                &black,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            target.PopAxisAlignedClip();
        }
    }
    Ok(())
}

impl Renderer {
    pub fn new(handle: *mut core::ffi::c_void, width: u32, height: u32, dpi: f32) -> Result<Self> {
        // The current text UI paints on demand. Avoid per-process GPU-device startup
        // and waiting for vertical refresh in the window's paint handler.
        Self::with_options(
            handle,
            width,
            height,
            dpi,
            D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            D2D1_PRESENT_OPTIONS_IMMEDIATELY,
        )
    }
    pub(super) fn with_options(
        handle: *mut core::ffi::c_void,
        width: u32,
        height: u32,
        dpi: f32,
        kind: D2D1_RENDER_TARGET_TYPE,
        present: D2D1_PRESENT_OPTIONS,
    ) -> Result<Self> {
        // SAFETY: called on the owning UI thread with a live HWND.
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let text: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let target = factory.CreateHwndRenderTarget(
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: kind,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_UNKNOWN,
                        alphaMode: D2D1_ALPHA_MODE_UNKNOWN,
                    },
                    dpiX: dpi,
                    dpiY: dpi,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                },
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: HWND(handle),
                    pixelSize: D2D_SIZE_U {
                        width: width.max(1),
                        height: height.max(1),
                    },
                    presentOptions: present,
                },
            )?;
            Ok(Self {
                factory,
                text,
                target,
            })
        }
    }
    pub fn resize(&self, width: u32, height: u32, dpi: f32) -> Result<()> {
        // SAFETY: only the window's render target is resized, with nonzero dimensions.
        unsafe {
            self.target.SetDpi(dpi, dpi);
            self.target.Resize(&D2D_SIZE_U {
                width: width.max(1),
                height: height.max(1),
            })
        }
    }
    #[cfg(test)]
    pub fn document(&self, document: &Document) -> Result<Layout> {
        layout::build(&self.text, &self.target, document)
    }
    fn fill(&self, rectangle: D2D_RECT_F, color: [u8; 3]) -> Result<()> {
        // SAFETY: temporary brushes remain alive through synchronous drawing.
        unsafe {
            let brush = self
                .target
                .CreateSolidColorBrush(&layout::color(color), None)?;
            self.target.FillRectangle(&rectangle, &brush);
        }
        Ok(())
    }
    fn rounded(&self, rectangle: D2D_RECT_F, color: [u8; 3]) -> Result<()> {
        // SAFETY: the local brush remains alive for this synchronous draw.
        unsafe {
            let brush = self
                .target
                .CreateSolidColorBrush(&layout::color(color), None)?;
            self.target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rectangle,
                    radiusX: 6.0,
                    radiusY: 6.0,
                },
                &brush,
            );
        }
        Ok(())
    }
    fn text(&self, text: &str, x: f32, y: f32, width: f32, style: TextStyle) -> Result<()> {
        let TextStyle { size, color, bold } = style;
        // SAFETY: text and format are retained until the draw call returns.
        unsafe {
            let format = self.text.CreateTextFormat(
                w!("Segoe UI"),
                None,
                if bold {
                    DWRITE_FONT_WEIGHT_SEMI_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_NORMAL
                },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("ru-RU"),
            )?;
            let utf16: Vec<_> = text.encode_utf16().collect();
            let layout = self
                .text
                .CreateTextLayout(&utf16, &format, width.max(1.0), 1000.0)?;
            let brush = self
                .target
                .CreateSolidColorBrush(&layout::color(color), None)?;
            self.target.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );
        }
        Ok(())
    }
    pub fn paint(&self, view: &View<'_>) -> Result<()> {
        self.paint_with_phase_mark(view, || {})
    }
    fn paint_with_phase_mark(
        &self,
        view: &View<'_>,
        mut scene_complete: impl FnMut(),
    ) -> Result<()> {
        // SAFETY: all drawing happens between BeginDraw and EndDraw on one thread.
        unsafe {
            self.target.BeginDraw();
            self.target.SetTransform(&matrix(1.0, 0.0, 0.0));
        }
        let result = self.scene(view);
        scene_complete();
        // Always close the draw even if allocating a resource failed.
        let end = unsafe { self.target.EndDraw(None, None) };
        result.and(end)
    }
    #[cfg(test)]
    pub fn measure_paint(&self, view: &View<'_>) -> Result<(f64, f64)> {
        let start = std::time::Instant::now();
        let mut scene_ms = 0.0;
        let mut present_start = start;
        self.paint_with_phase_mark(view, || {
            scene_ms = start.elapsed().as_secs_f64() * 1000.0;
            present_start = std::time::Instant::now();
        })?;
        Ok((scene_ms, present_start.elapsed().as_secs_f64() * 1000.0))
    }
    fn scene(&self, v: &View<'_>) -> Result<()> {
        let background = if v.dark {
            rgb("background")
        } else {
            rgb("paper")
        };
        let surface = if v.dark { [36, 46, 40] } else { rgb("surface") };
        let ink = if v.dark {
            rgb("foreground")
        } else {
            rgb("ink")
        };
        let muted = if v.dark {
            rgb("secondary")
        } else {
            rgb("slate")
        };
        let stroke = if v.dark { [60, 73, 63] } else { rgb("stroke") };
        let panel = if v.dark {
            rgb("panel")
        } else {
            [249, 250, 246]
        };
        unsafe {
            self.target.Clear(Some(&layout::color(background)));
        }
        let (left, right, top, bottom) = panes(v.width, v.height);
        self.fill(rect(0.0, 0.0, v.width, 64.0), surface)?;
        self.fill(rect(0.0, 64.0, left, bottom - 64.0), panel)?;
        self.fill(rect(left, 64.0, right - left, 80.0), surface)?;
        self.fill(rect(right, 64.0, v.width - right, bottom - 64.0), surface)?;
        for r in [
            rect(0.0, 63.0, v.width, 1.0),
            rect(left, 64.0, 1.0, bottom - 64.0),
            rect(right, 64.0, 1.0, bottom - 64.0),
            rect(left, 103.0, right - left, 1.0),
            rect(left, 143.0, right - left, 1.0),
        ] {
            self.fill(r, stroke)?;
        }
        self.logo(
            if v.dark { MARK_REVERSE } else { MARK },
            22.0,
            16.0,
            0.5,
            ink,
            false,
        )?;
        self.logo(WORDMARK, 64.0, 14.0, 0.4, ink, true)?;
        self.text(
            "Документы",
            left + 18.0,
            23.0,
            180.0,
            TextStyle {
                size: 13.0,
                color: muted,
                bold: false,
            },
        )?;
        self.rounded(rect(v.width - 228.0, 14.0, 132.0, 36.0), rgb("forest"))?;
        self.rounded(
            rect(v.width - 404.0, 14.0, 148.0, 36.0),
            if v.document.is_some() {
                rgb("sage")
            } else {
                panel
            },
        )?;
        self.text(
            "Сохранить копию",
            v.width - 393.0,
            23.0,
            130.0,
            TextStyle {
                size: 13.0,
                color: if v.document.is_some() { ink } else { muted },
                bold: false,
            },
        )?;
        self.text(
            "Открыть DOCX",
            v.width - 215.0,
            22.0,
            120.0,
            TextStyle {
                size: 14.0,
                color: [255, 255, 255],
                bold: true,
            },
        )?;
        self.text(
            if v.dark {
                "Светлая"
            } else {
                "Тёмная"
            },
            v.width - 80.0,
            23.0,
            76.0,
            TextStyle {
                size: 12.0,
                color: ink,
                bold: false,
            },
        )?;
        self.text(
            "ПРОЕКТ",
            20.0,
            88.0,
            left - 40.0,
            TextStyle {
                size: 12.0,
                color: muted,
                bold: false,
            },
        )?;
        self.text(
            "Документы",
            20.0,
            116.0,
            left - 40.0,
            TextStyle {
                size: 14.0,
                color: ink,
                bold: true,
            },
        )?;
        if v.document.is_some() {
            self.rounded(
                rect(12.0, 153.0, left - 24.0, 58.0),
                if v.dark { [53, 70, 58] } else { rgb("sage") },
            )?;
            self.text(
                v.filename,
                22.0,
                163.0,
                left - 44.0,
                TextStyle {
                    size: 12.0,
                    color: ink,
                    bold: false,
                },
            )?;
        } else {
            self.text(
                "Откройте документ",
                20.0,
                162.0,
                left - 40.0,
                TextStyle {
                    size: 12.0,
                    color: muted,
                    bold: false,
                },
            )?;
        }
        self.rounded(
            rect(12.0, 221.0, left - 24.0, 36.0),
            if v.recovery_enabled {
                rgb("sage")
            } else {
                panel
            },
        )?;
        self.text(
            "Восстановить сессию",
            22.0,
            231.0,
            left - 44.0,
            TextStyle {
                size: 12.0,
                color: if v.recovery_enabled { ink } else { muted },
                bold: false,
            },
        )?;
        self.text(
            "СТРАНИЦЫ",
            20.0,
            286.0,
            left - 40.0,
            TextStyle {
                size: 12.0,
                color: muted,
                bold: false,
            },
        )?;
        if let (Some(doc), Some(layout)) = (v.document, v.layout) {
            let current = (v.scroll / (doc.page.height + 24.0)).floor() as usize;
            let first = current.saturating_sub(2);
            let available = ((bottom - 334.0) / 34.0).max(1.0) as usize;
            for index in first..layout.pages.len().min(first + available) {
                let y = 320.0 + (index - first) as f32 * 34.0;
                if index == current {
                    self.fill(rect(16.0, y - 3.0, 2.0, 28.0), rgb("forest"))?;
                }
                self.text(
                    &format!("Страница {}", index + 1),
                    28.0,
                    y,
                    left - 44.0,
                    TextStyle {
                        size: 13.0,
                        color: if index == current { ink } else { muted },
                        bold: index == current,
                    },
                )?;
            }
        }
        self.text(
            if v.document.is_some() {
                v.filename
            } else {
                "Документ не открыт"
            },
            left + 18.0,
            76.0,
            (right - left - 36.0).max(1.0),
            TextStyle {
                size: 13.0,
                color: ink,
                bold: true,
            },
        )?;
        self.text(
            if v.document.is_some() {
                "Таблицы и рисунки — правый щелчок"
            } else {
                "Оформление из DOCX"
            },
            left + 18.0,
            116.0,
            230.0,
            TextStyle {
                size: 12.0,
                color: ink,
                bold: false,
            },
        )?;
        if right - left > 500.0 {
            self.text(
                if v.dirty {
                    "Есть изменения"
                } else {
                    "Без изменений"
                },
                right - 140.0,
                116.0,
                130.0,
                TextStyle {
                    size: 12.0,
                    color: muted,
                    bold: false,
                },
            )?;
        }
        if right < v.width {
            self.text(
                "Помощник",
                right + 20.0,
                88.0,
                220.0,
                TextStyle {
                    size: 16.0,
                    color: ink,
                    bold: true,
                },
            )?;
            self.fill(rect(right + 20.0, 124.0, 100.0, 2.0), rgb("forest"))?;
            if !v.chat_active {
                self.text(
                    &v.ollama_status.chars().take(28).collect::<String>(),
                    right + 20.0,
                    150.0,
                    240.0,
                    TextStyle {
                        size: 14.0,
                        color: muted,
                        bold: false,
                    },
                )?;
                self.text(
                    v.autosave_status,
                    right + 20.0,
                    180.0,
                    240.0,
                    TextStyle {
                        size: 12.0,
                        color: muted,
                        bold: false,
                    },
                )?;
                if let (Some(doc), Some(layout)) = (v.document, v.layout) {
                    self.text(
                        "Документ",
                        right + 20.0,
                        211.0,
                        240.0,
                        TextStyle {
                            size: 14.0,
                            color: ink,
                            bold: true,
                        },
                    )?;
                    self.text(&format!("{} страниц\n{} абзацев\n\nСтраница: {:.0} × {:.0} мм\nПоля: {:.0} / {:.0} / {:.0} / {:.0} мм",layout.pages.len(),doc.paragraphs.len(),doc.page.width*25.4/96.0,doc.page.height*25.4/96.0,doc.page.left*25.4/96.0,doc.page.right*25.4/96.0,doc.page.top*25.4/96.0,doc.page.bottom*25.4/96.0), right+20.0, 246.0, 240.0, TextStyle { size: 13.0, color: muted, bold: false })?;
                    let mut warnings = doc
                        .warnings
                        .iter()
                        .cloned()
                        .chain(
                            layout
                                .missing_fonts
                                .iter()
                                .map(|f| format!("Не найден шрифт: {f}")),
                        )
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if let Some(saved) = doc
                        .saved_page_count
                        .filter(|_| !v.dirty)
                        .filter(|saved| *saved != layout.pages.len())
                    {
                        if !warnings.is_empty() {
                            warnings.push_str("\n\n");
                        }
                        warnings.push_str(&format!("Разбиение отличается:\nPaperik — {} страниц,\nметаданные файла — {saved}.\nТочность требует проверки.",layout.pages.len()));
                    }
                    if !warnings.is_empty() {
                        self.rounded(
                            rect(right + 12.0, 420.0, 256.0, 116.0),
                            rgb("warningBackground"),
                        )?;
                        self.text(
                            &warnings,
                            right + 20.0,
                            430.0,
                            240.0,
                            TextStyle {
                                size: 12.0,
                                color: rgb("warningForeground"),
                                bold: false,
                            },
                        )?;
                    }
                }
                self.text("Ctrl+S — сохранить новую копию\nCtrl+Z / Ctrl+Y — отмена / повтор\nCtrl+C / X / V — буфер обмена\nShift+стрелки — выделение\nCtrl+колесо — масштаб\nCtrl+O — открыть", right+20.0, bottom-150.0, 240.0, TextStyle { size: 12.0, color: muted, bold: false })?;
            }
        }
        // Clip both pages and document text to the central workspace.
        unsafe {
            self.target.PushAxisAlignedClip(
                &rect(left + 1.0, top, right - left - 2.0, bottom - top),
                D2D1_ANTIALIAS_MODE_ALIASED,
            );
        }
        if let (Some(doc), Some(layout)) = (v.document, v.layout) {
            let x = left + (right - left - doc.page.width * v.zoom) / 2.0;
            for (index, page) in layout.pages.iter().enumerate() {
                let y = top + (24.0 + index as f32 * (doc.page.height + 24.0) - v.scroll) * v.zoom;
                if y > bottom || y + doc.page.height * v.zoom < top {
                    continue;
                }
                self.fill(
                    rect(
                        x - 1.0,
                        y - 1.0,
                        doc.page.width * v.zoom + 2.0,
                        doc.page.height * v.zoom + 2.0,
                    ),
                    stroke,
                )?;
                self.fill(
                    rect(x, y, doc.page.width * v.zoom, doc.page.height * v.zoom),
                    [255, 255, 255],
                )?;
                unsafe {
                    self.target.SetTransform(&matrix(v.zoom, x, y));
                }
                if let Some(selection) = v.selection {
                    for highlight in layout.highlights(doc, index, selection)? {
                        self.fill(
                            rect(highlight.x, highlight.y, highlight.width, highlight.height),
                            [190, 218, 203],
                        )?;
                    }
                }
                draw_page(&self.target, page, doc.page.width)?;
                if let Some(selection) = v.selection.filter(|selection| selection.collapsed())
                    && let Some(image) = page.images.iter().find(|p| {
                        p.paragraph == selection.focus.paragraph
                            && p.offset.unwrap_or(0) == selection.focus.offset
                    })
                {
                    let [x, y, width, height] = image.rect;
                    unsafe {
                        let brush = self
                            .target
                            .CreateSolidColorBrush(&layout::color(rgb("forest")), None)?;
                        self.target.DrawRectangle(
                            &rect(x, y, width, height),
                            &brush,
                            1.5 / v.zoom,
                            None,
                        );
                    }
                }
                unsafe {
                    self.target.SetTransform(&matrix(1.0, 0.0, 0.0));
                }
            }
        } else {
            self.text(
                if v.message.is_empty() {
                    "Откройте DOCX, чтобы начать работу"
                } else {
                    v.message
                },
                left + 40.0,
                top + 60.0,
                (right - left - 80.0).max(1.0),
                TextStyle {
                    size: 18.0,
                    color: ink,
                    bold: false,
                },
            )?;
        }
        unsafe {
            self.target.PopAxisAlignedClip();
        }
        self.fill(rect(0.0, bottom, v.width, 30.0), surface)?;
        self.fill(rect(0.0, bottom, v.width, 1.0), stroke)?;
        if let (Some(doc), Some(layout)) = (v.document, v.layout) {
            let page = ((v.scroll / (doc.page.height + 24.0)).floor() as usize + 1)
                .min(layout.pages.len());
            self.text(
                &if !v.message.is_empty() {
                    v.message.to_owned()
                } else {
                    format!(
                        "Страница {page} из {} · {}",
                        layout.pages.len(),
                        if (v.dirty && !v.autosave_status.is_empty())
                            || v.autosave_status == "Ошибка автосохранения"
                        {
                            v.autosave_status
                        } else if v.dirty {
                            "Есть несохранённые правки"
                        } else {
                            "Нет несохранённых правок"
                        }
                    )
                },
                left + 16.0,
                bottom + 7.0,
                (right - left - 150.0).max(1.0),
                TextStyle {
                    size: 12.0,
                    color: muted,
                    bold: false,
                },
            )?;
        }
        self.text(
            &format!("−   {:.0}%   +", v.zoom * 100.0),
            right - 128.0,
            bottom + 7.0,
            120.0,
            TextStyle {
                size: 12.0,
                color: ink,
                bold: false,
            },
        )?;
        Ok(())
    }

    fn logo(
        &self,
        source: &str,
        x: f32,
        y: f32,
        scale: f32,
        ink: [u8; 3],
        stroke: bool,
    ) -> Result<()> {
        let xml = roxmltree::Document::parse(source).expect("Проверенный SVG бренда");
        unsafe {
            self.target.SetTransform(&matrix(scale, x, y));
        }
        for node in xml.descendants().filter(|n| n.is_element()) {
            if node.tag_name().name() == "path" {
                if let Some(data) = node.attribute("d") {
                    let geometry = self.factory_geometry(data)?;
                    let fill = node
                        .attribute("fill")
                        .filter(|v| v.starts_with('#'))
                        .and_then(|v| u32::from_str_radix(&v[1..], 16).ok())
                        .map(|v| [(v >> 16) as u8, (v >> 8) as u8, v as u8])
                        .unwrap_or(ink);
                    unsafe {
                        let brush = self
                            .target
                            .CreateSolidColorBrush(&layout::color(fill), None)?;
                        if stroke {
                            let style = self.factory.CreateStrokeStyle(
                                &D2D1_STROKE_STYLE_PROPERTIES {
                                    startCap: D2D1_CAP_STYLE_ROUND,
                                    endCap: D2D1_CAP_STYLE_ROUND,
                                    lineJoin: D2D1_LINE_JOIN_ROUND,
                                    ..Default::default()
                                },
                                None,
                            )?;
                            self.target.DrawGeometry(&geometry, &brush, 7.5, &style);
                        } else {
                            self.target.FillGeometry(&geometry, &brush, None);
                        }
                    }
                }
            } else if node.tag_name().name() == "circle" {
                let value = |name| {
                    node.attribute(name)
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.0)
                };
                unsafe {
                    let brush = self
                        .target
                        .CreateSolidColorBrush(&layout::color(ink), None)?;
                    self.target.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: Vector2 {
                                X: value("cx"),
                                Y: value("cy"),
                            },
                            radiusX: value("r"),
                            radiusY: value("r"),
                        },
                        &brush,
                    );
                }
            }
        }
        unsafe {
            self.target.SetTransform(&matrix(1.0, 0.0, 0.0));
        }
        Ok(())
    }
    fn factory_geometry(&self, data: &str) -> Result<ID2D1PathGeometry> {
        // This bounded parser supports only the M/L/H/V/Q/C/Z commands present
        // in our bundled brand vectors; it is not an imported-document SVG engine.
        let mut tokens = Vec::new();
        let mut number = String::new();
        for character in data.chars().chain(std::iter::once(' ')) {
            if character.is_ascii_digit() || character == '.' || character == '-' {
                number.push(character);
            } else {
                if !number.is_empty() {
                    tokens.push(number.clone());
                    number.clear();
                }
                if character.is_ascii_alphabetic() {
                    tokens.push(character.to_string());
                }
            }
        }
        unsafe {
            let geometry = self.factory.CreatePathGeometry()?;
            let sink = geometry.Open()?;
            sink.SetFillMode(D2D1_FILL_MODE_ALTERNATE);
            let mut index = 0;
            let mut current = Vector2::default();
            let mut open = false;
            while index < tokens.len() {
                let command = tokens[index].as_str();
                index += 1;
                let mut next = || {
                    let value = tokens[index]
                        .parse::<f32>()
                        .expect("Проверенный контур бренда");
                    index += 1;
                    value
                };
                match command {
                    "M" => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        current = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        sink.BeginFigure(current, D2D1_FIGURE_BEGIN_FILLED);
                        open = true;
                    }
                    "L" => {
                        current = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        sink.AddLine(current);
                    }
                    "H" => {
                        current.X = next();
                        sink.AddLine(current);
                    }
                    "V" => {
                        current.Y = next();
                        sink.AddLine(current);
                    }
                    "Q" => {
                        let control = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        current = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                            point1: control,
                            point2: current,
                        });
                    }
                    "C" => {
                        let first = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        let second = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        current = Vector2 {
                            X: next(),
                            Y: next(),
                        };
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                            point1: first,
                            point2: second,
                            point3: current,
                        });
                    }
                    "Z" => {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                    _ => unreachable!("Команда не входит в наши ресурсы"),
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close()?;
            Ok(geometry)
        }
    }
}
