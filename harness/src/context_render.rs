//! Context renders: pdf.js and WebKit (Apple's PDFKit, which Safari shows PDFs with) drawn next
//! to Safe-PDF and the reference.
//!
//! The reference alone decides whether a page passes. A context render is only scored against
//! Safe-PDF's image, so a reader can tell when the reference is the odd one out. pdf.js renders
//! in the run's headless Chrome. PDFKit runs only on macOS, so its renders are made beforehand
//! by `pdfkit/render.swift` and read from a directory.

use crate::{
    compare,
    model::{ContextRenderer, ContextScore},
    process::STAGE_MARKER,
    web_render::{self, WebEndpoints},
};
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{RgbaImage, imageops::FilterType};
use serde_json::json;
use std::path::Path;

/// Worker stage while pdf.js renders the page in the browser.
pub const PDFJS_STAGE: &str = "pdfjs-render";

/// Pixels a PDFKit render may differ from the reference size on each side before it is
/// reported instead of scaled: PDFKit rounds page boxes on its own.
const SIZE_SLACK: u32 = 2;

/// Where a page worker gets context renders from.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextSources<'a> {
    /// Browser and harness server in which pdf.js renders.
    pub pdfjs: Option<&'a WebEndpoints>,
    /// PDFKit renders of this document, one `pN.png` per page.
    pub webkit: Option<&'a Path>,
    /// Whether the renders include annotations and form fields, as the reference's does.
    pub annotations: bool,
}

/// One page to draw with the context renderers and the Safe-PDF image to score them against.
pub struct ContextPage<'a> {
    /// The PDF file's bytes.
    pub bytes: &'a [u8],
    /// Document password.
    pub password: Option<&'a str>,
    /// Zero-based page index.
    pub page: usize,
    /// Safe-PDF's render, whose size every context render is drawn at.
    pub safe: &'a RgbaImage,
    /// Pixel boxes of Safe-PDF's glyphs, where rasterization differences are tolerated.
    pub glyphs: &'a [[u32; 4]],
    /// Mismatch fraction still counted as agreeing.
    pub tolerance: f64,
}

impl ContextSources<'_> {
    /// Returns the context renderers this worker draws with.
    fn renderers(&self) -> impl Iterator<Item = ContextRenderer> {
        [
            self.pdfjs.map(|_| ContextRenderer::Pdfjs),
            self.webkit.map(|_| ContextRenderer::Webkit),
        ]
        .into_iter()
        .flatten()
    }

    /// Draws `page` with every context renderer and scores Safe-PDF's image against each.
    /// Returns each score with the image it was taken from.
    pub fn score(&self, page: &ContextPage<'_>) -> Vec<(ContextScore, Option<RgbaImage>)> {
        self.renderers()
            .map(|renderer| {
                let drawn = self
                    .draw(renderer, page)
                    .and_then(|image| Ok((page.mismatch(&image)?, image)));
                match drawn {
                    Ok((mismatch, image)) => (
                        ContextScore {
                            renderer,
                            mismatch: Some(mismatch),
                            matches: mismatch <= page.tolerance,
                            error: None,
                            file: None,
                        },
                        Some(image),
                    ),
                    Err(error) => (
                        ContextScore {
                            renderer,
                            mismatch: None,
                            matches: false,
                            error: Some(format!("{error:#}")),
                            file: None,
                        },
                        None,
                    ),
                }
            })
            .collect()
    }

    /// Returns `renderer`'s image of the page at the size of Safe-PDF's render.
    fn draw(&self, renderer: ContextRenderer, page: &ContextPage<'_>) -> Result<RgbaImage> {
        let size = page.safe.dimensions();
        match renderer {
            ContextRenderer::Pdfjs => {
                let endpoints = self
                    .pdfjs
                    .ok_or_else(|| anyhow!("pdf.js is not available"))?;
                let render = format!(
                    "conformanceRender({}, {}, {}, {}, {}, {})",
                    json!(BASE64.encode(page.bytes)),
                    json!(page.password),
                    page.page,
                    size.0,
                    size.1,
                    self.annotations
                );
                let (image, _) = web_render::screenshot(
                    endpoints,
                    &endpoints.pdfjs_page(),
                    &render,
                    PDFJS_STAGE,
                    size,
                )?;
                Ok(image)
            }
            ContextRenderer::Webkit => {
                let dir = self
                    .webkit
                    .ok_or_else(|| anyhow!("WebKit renders are not available"))?;
                eprintln!("{STAGE_MARKER}webkit-load");
                let path = dir.join(format!("p{}.png", page.page));
                let image = image::open(&path)
                    .map_err(|error| anyhow!("no WebKit render {}: {error}", path.display()))?
                    .to_rgba8();
                fit_to(image, size)
            }
        }
    }
}

impl ContextPage<'_> {
    /// Returns the fraction of pixels in which `image` differs from Safe-PDF's render.
    fn mismatch(&self, image: &RgbaImage) -> Result<f64> {
        eprintln!("{STAGE_MARKER}context-compare");
        let mut comparison = compare::compare(image, self.safe, &[])?;
        if comparison.metrics.mismatch > self.tolerance {
            comparison.tolerate_glyph_rasterization(image, self.safe, self.glyphs);
        }
        Ok(comparison.metrics.mismatch)
    }
}

/// Scales a render that is off by rounding to exactly `size`; a larger difference means the
/// renderers disagree on the page box, which is reported.
fn fit_to(image: RgbaImage, (width, height): (u32, u32)) -> Result<RgbaImage> {
    if image.dimensions() == (width, height) {
        return Ok(image);
    }
    if image.width().abs_diff(width) > SIZE_SLACK || image.height().abs_diff(height) > SIZE_SLACK {
        bail!(
            "WebKit rendered {:?}, expected {:?}",
            image.dimensions(),
            (width, height)
        );
    }
    Ok(image::imageops::resize(
        &image,
        width,
        height,
        FilterType::Triangle,
    ))
}
