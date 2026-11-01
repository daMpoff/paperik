use crate::{layout, render};
use paperik_document::Document;
use std::path::Path;
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::{HSTRING, Result};

const DPI: f32 = 144.0;
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn run(path: &Path, directory: Option<&Path>) -> std::result::Result<String, String> {
    let document = Document::open(path)?;
    if let Some(directory) = directory {
        // A new directory prevents overwriting unrelated user images.
        std::fs::create_dir(directory).map_err(|e| format!("Не создан новый каталог: {e}"))?;
    }
    let (report, glyphs) = render_document(&document, directory)
        .map_err(|e| format!("Диагностика отображения: {e}"))?;
    if let Some(directory) = directory {
        std::fs::write(directory.join("layout.tsv"), &report).map_err(|e| e.to_string())?;
        std::fs::write(directory.join("glyphs.tsv"), &glyphs).map_err(|e| e.to_string())?;
    }
    Ok(report)
}

fn render_document(document: &Document, directory: Option<&Path>) -> Result<(String, String)> {
    // All COM and D2D objects remain on this thread and outlive the page draws.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _com = Com;
        let imaging: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let text: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let width = (document.page.width * DPI / 96.0).ceil() as u32;
        let height = (document.page.height * DPI / 96.0).ceil() as u32;
        let bitmap = imaging.CreateBitmap(
            width,
            height,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapCacheOnLoad,
        )?;
        let target = factory.CreateWicBitmapRenderTarget(
            &bitmap,
            &D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_UNKNOWN,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: DPI,
                dpiY: DPI,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            },
        )?;
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let layout = layout::build(&text, &target, document)?;
        if let Some(directory) = directory {
            for (index, page) in layout.pages.iter().enumerate() {
                target.BeginDraw();
                target.Clear(Some(&layout::color([255, 255, 255])));
                let result = render::draw_page(&target, page, document.page.width);
                let end = target.EndDraw(None, None);
                result.and(end)?;
                save_png(
                    &imaging,
                    &bitmap,
                    &directory.join(format!("page-{:02}.png", index + 1)),
                    width,
                    height,
                )?;
            }
        }
        Ok((
            layout.report(document)?,
            if directory.is_some() {
                layout.glyph_report(document)?
            } else {
                String::new()
            },
        ))
    }
}

unsafe fn save_png(
    factory: &IWICImagingFactory,
    bitmap: &IWICBitmap,
    path: &Path,
    width: u32,
    height: u32,
) -> Result<()> {
    unsafe {
        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.expect("WIC returned a frame after success");
        frame.Initialize(None)?;
        frame.SetSize(width, height)?;
        frame.SetResolution(f64::from(DPI), f64::from(DPI))?;
        let mut pixel_format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut pixel_format)?;
        frame.WriteSource(bitmap, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()
    }
}
