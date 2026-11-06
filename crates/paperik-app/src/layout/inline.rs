//! A raster occupies one shaped cluster. Pixels use the page's bitmap path.
use super::*;
use paperik_document::InlineRaster;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    sync::{Arc, atomic::AtomicBool},
};
use windows::core::{BOOL, IUnknown, Ref, implement};

#[implement(IDWriteInlineObject)]
struct Space {
    width: f32,
    height: f32,
}

impl IDWriteInlineObject_Impl for Space_Impl {
    fn Draw(
        &self,
        _: *const c_void,
        _: Ref<IDWriteTextRenderer>,
        _: f32,
        _: f32,
        _: BOOL,
        _: BOOL,
        _: Ref<IUnknown>,
    ) -> Result<()> {
        Ok(())
    }

    fn GetMetrics(&self) -> Result<DWRITE_INLINE_OBJECT_METRICS> {
        Ok(DWRITE_INLINE_OBJECT_METRICS {
            width: self.width,
            height: self.height,
            baseline: self.height,
            supportsSideways: false.into(),
        })
    }

    fn GetOverhangMetrics(&self) -> Result<DWRITE_OVERHANG_METRICS> {
        Ok(Default::default())
    }

    fn GetBreakConditions(
        &self,
        before: *mut DWRITE_BREAK_CONDITION,
        after: *mut DWRITE_BREAK_CONDITION,
    ) -> Result<()> {
        unsafe {
            *before = DWRITE_BREAK_CONDITION_CAN_BREAK;
            *after = DWRITE_BREAK_CONDITION_CAN_BREAK;
        }
        Ok(())
    }
}

pub(super) struct Item {
    native: u32,
    offset: u32,
    raster: InlineRaster,
}

pub(super) fn attach(
    layout: &IDWriteTextLayout,
    paragraph: &Paragraph,
    start: u32,
    prefix: u32,
    length: u32,
) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for (offset, _, raster) in paragraph.inline_pictures() {
        let position = prefix + offset;
        if !(start..start + length).contains(&position) {
            continue;
        }
        let object: IDWriteInlineObject = Space {
            width: raster.width,
            height: raster.height,
        }
        .into();
        let native = position - start;
        unsafe {
            layout.SetInlineObject(
                &object,
                DWRITE_TEXT_RANGE {
                    startPosition: native,
                    length: 1,
                },
            )?;
        }
        items.push(Item {
            native,
            offset,
            raster: raster.clone(),
        });
    }
    Ok(items)
}

pub(super) fn line_box(
    layout: &TextLayout,
    metric: &DWRITE_LINE_METRICS,
    start: u32,
) -> Result<(f32, f32, f32)> {
    if layout.inline.is_empty() {
        return Ok((metric.height, metric.baseline, 0.0));
    }
    let (height, baseline, exact) = layout.line_spacing;
    let required = layout
        .inline
        .iter()
        .filter(|item| (start..start + metric.length).contains(&item.native))
        .map(|item| item.raster.height)
        .fold(baseline, f32::max);
    if exact && required > baseline + 0.01 {
        return Err(blocks::error(
            "Рисунок не помещается в строку с точным межстрочным интервалом",
        ));
    }
    Ok((
        height + required - baseline,
        required,
        required - metric.baseline,
    ))
}

impl blocks::ImageLayout {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let [left, top, width, height] = self.rect;
        (left..left + width).contains(&x) && (top..top + height).contains(&y)
    }
}

pub(super) fn place(
    pages: &mut [PageLayout],
    document: &Document,
    cancelled: &AtomicBool,
) -> Result<()> {
    const BUDGET: usize = 64 * 1024 * 1024;
    let mut used: usize = pages
        .iter()
        .flat_map(|p| &p.images)
        .map(|p| p.pixels.bytes.len())
        .sum();
    let mut cached: BTreeMap<_, Arc<blocks::Pixels>> = BTreeMap::new();
    for page in pages {
        for line in page
            .lines
            .iter()
            .filter(|line| !line.layout.inline.is_empty())
        {
            blocks::check(cancelled)?;
            let range = line.layout_text_start..line.layout_text_start + line.layout_length;
            for item in line
                .layout
                .inline
                .iter()
                .filter(|item| range.contains(&item.native))
            {
                let mut x = 0.0;
                let mut y = 0.0;
                let mut hit = DWRITE_HIT_TEST_METRICS::default();
                unsafe {
                    line.layout.HitTestTextPosition(
                        item.native,
                        false,
                        &mut x,
                        &mut y,
                        &mut hit,
                    )?;
                }
                let raster = &item.raster;
                let top = line.y + line.baseline - raster.height;
                if line.y + line.height > document.page.height - document.page.bottom + 0.1
                    || hit.left < -0.1
                    || hit.left + raster.width > line.width + 0.1
                {
                    return Err(blocks::error(
                        "Рисунок не помещается в область текста или ячейку",
                    ));
                }
                let address = Arc::as_ptr(&raster.bytes);
                let pixels = match cached.entry(address) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.get().clone(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let pixels = blocks::decode(&raster.bytes, BUDGET.saturating_sub(used))?;
                        used += pixels.bytes.len();
                        entry.insert(Arc::new(pixels)).clone()
                    }
                };
                page.images.push(blocks::ImageLayout {
                    paragraph: line.paragraph,
                    offset: Some(item.offset),
                    rect: [line.x + hit.left, top, raster.width, raster.height],
                    pixels,
                });
            }
        }
    }
    Ok(())
}
