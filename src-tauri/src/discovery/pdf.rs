use super::*;

/// Byte-level gate for PDF downloads: rejects oversized bodies (shared school
/// hosts serve multi-hundred-MB scanned bundles) and non-PDF content served
/// under a `.pdf` URL before paying for text extraction.
pub(super) fn pdf_bytes_usable(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_PDF_BYTES {
        return Err(format!(
            "PDF troppo grande ({} byte, limite {})",
            bytes.len(),
            MAX_PDF_BYTES
        ));
    }
    if !bytes.starts_with(b"%PDF") {
        return Err("contenuto non PDF".to_owned());
    }
    Ok(())
}

/// Glyph advance widths of a simple font (`/FirstChar` + `/Widths`, in thousandths
/// of an em). Composite and unknown fonts fall back to an average width.
pub(super) struct PdfFontWidths {
    first_char: i64,
    widths: Vec<f32>,
    missing: f32,
    two_byte: bool,
}

impl PdfFontWidths {
    fn new(document: &lopdf::Document, font: &lopdf::Dictionary) -> Self {
        let resolve = |key: &[u8]| {
            font.get(key)
                .ok()
                .and_then(|object| document.dereference(object).ok())
                .map(|(_, object)| object.clone())
        };
        let widths = resolve(b"Widths")
            .and_then(|object| object.as_array().ok().cloned())
            .map(|array| {
                array
                    .iter()
                    .map(|width| {
                        document
                            .dereference(width)
                            .ok()
                            .and_then(|(_, width)| width.as_float().ok())
                            .unwrap_or(500.0)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            first_char: resolve(b"FirstChar")
                .and_then(|object| object.as_i64().ok())
                .unwrap_or(0),
            widths,
            missing: 500.0,
            two_byte: resolve(b"Subtype")
                .is_some_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Type0")),
        }
    }

    /// Advance of `bytes` in thousandths of an em, plus the number of glyphs.
    fn advance(&self, bytes: &[u8]) -> (f32, usize) {
        if self.two_byte {
            let glyphs = bytes.len() / 2;
            return (glyphs as f32 * self.missing, glyphs);
        }
        let width = bytes
            .iter()
            .map(|code| {
                usize::try_from(i64::from(*code) - self.first_char)
                    .ok()
                    .and_then(|index| self.widths.get(index))
                    .copied()
                    .unwrap_or(self.missing)
            })
            .sum();
        (width, bytes.len())
    }
}

pub(super) type PdfFonts<'a> =
    std::collections::BTreeMap<Vec<u8>, (lopdf::Encoding<'a>, PdfFontWidths)>;

pub(super) fn pdf_fonts<'a>(
    document: &'a lopdf::Document,
    fonts: impl IntoIterator<Item = (Vec<u8>, &'a lopdf::Dictionary)>,
) -> PdfFonts<'a> {
    fonts
        .into_iter()
        .filter_map(|(name, font)| {
            let encoding = font
                .get_font_encoding_with_limit(document, MAX_PDF_DECOMPRESSED_BYTES)
                .ok()?;
            Some((name, (encoding, PdfFontWidths::new(document, font))))
        })
        .collect()
}

pub(super) fn pdf_dictionary<'a>(
    document: &'a lopdf::Document,
    object: &'a lopdf::Object,
) -> Option<&'a lopdf::Dictionary> {
    document.dereference(object).ok()?.1.as_dict().ok()
}

/// Lays out decoded PDF text by tracking the pen with the fonts' glyph widths.
#[derive(Default)]
pub(super) struct PdfTextLayout {
    text: String,
    size: f32,
    scale: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal: f32,
    line_x: f32,
    line_y: f32,
    next_x: f32,
    /// Pen position after the last drawn glyph, and the baseline it was drawn on.
    pen: Option<(f32, f32)>,
}

impl PdfTextLayout {
    fn run(
        &mut self,
        document: &lopdf::Document,
        data: &[u8],
        fonts: &PdfFonts,
        xobjects: Option<&lopdf::Dictionary>,
        depth: usize,
    ) {
        use lopdf::{Document, Object, content::Content};
        let Ok(content) = Content::decode(data) else {
            return;
        };
        let mut font = None;
        for operation in &content.operations {
            let number = |index: usize| {
                operation
                    .operands
                    .get(index)
                    .and_then(|operand| operand.as_float().ok())
                    .unwrap_or(0.0)
            };
            match operation.operator.as_str() {
                "Tf" => {
                    font = operation
                        .operands
                        .first()
                        .and_then(|name| name.as_name().ok())
                        .and_then(|name| fonts.get(name));
                    self.size = number(1).abs().max(0.1);
                }
                "Tc" => self.char_spacing = number(0),
                "Tw" => self.word_spacing = number(0),
                "Tz" => self.horizontal = number(0) / 100.0,
                "BT" => (self.line_x, self.line_y, self.scale) = (0.0, 0.0, 1.0),
                "Td" | "TD" => {
                    self.line_x += number(0) * self.scale;
                    self.line_y += number(1) * self.scale;
                }
                "Tm" => {
                    self.scale = number(0).abs().max(number(3).abs()).max(0.001);
                    (self.line_x, self.line_y) = (number(4), number(5));
                }
                "T*" | "'" | "\"" => self.line_y -= self.size * 1.2 * self.scale,
                "Do" if depth < 8 => {
                    self.run_form(document, operation.operands.first(), fonts, xobjects, depth);
                    continue;
                }
                _ => {}
            }
            if matches!(
                operation.operator.as_str(),
                "BT" | "Td" | "TD" | "Tm" | "T*" | "'" | "\""
            ) {
                self.next_x = self.line_x;
            }
            let Some((encoding, widths)) = font else {
                continue;
            };
            let items: Vec<&Object> = match operation.operator.as_str() {
                "Tj" | "'" => operation.operands.iter().take(1).collect(),
                "\"" => operation.operands.iter().skip(2).take(1).collect(),
                "TJ" => operation
                    .operands
                    .first()
                    .and_then(|array| array.as_array().ok())
                    .map(|array| array.iter().collect())
                    .unwrap_or_default(),
                _ => continue,
            };
            let em = self.size * self.scale * self.horizontal;
            for item in items {
                let Object::String(bytes, _) = item else {
                    // Negative kerning moves the pen right: a wide one reads as a gap.
                    if let Ok(kerning) = item.as_float() {
                        self.next_x -= kerning / 1000.0 * em;
                    }
                    continue;
                };
                let Ok(decoded) = Document::decode_text(encoding, bytes) else {
                    continue;
                };
                if let Some((pen_x, pen_y)) = self.pen {
                    if (pen_y - self.line_y).abs() > self.size * self.scale * 0.5 {
                        self.text.push('\n');
                    } else if self.next_x - pen_x > em * 0.2 && !self.text.ends_with([' ', '\n']) {
                        self.text.push(' ');
                    }
                }
                self.text.push_str(&decoded);
                let (advance, glyphs) = widths.advance(bytes);
                let spaces = decoded.chars().filter(|c| *c == ' ').count() as f32;
                self.next_x += advance / 1000.0 * em
                    + (glyphs as f32 * self.char_spacing + spaces * self.word_spacing)
                        * self.scale
                        * self.horizontal;
                self.pen = Some((self.next_x, self.line_y));
            }
        }
    }

    /// Form XObjects (`Do`) carry whole pages in stamped or registered documents.
    fn run_form(
        &mut self,
        document: &lopdf::Document,
        name: Option<&lopdf::Object>,
        fonts: &PdfFonts,
        xobjects: Option<&lopdf::Dictionary>,
        depth: usize,
    ) {
        let Some(stream) = name
            .and_then(|name| name.as_name().ok())
            .and_then(|name| xobjects?.get(name).ok())
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_stream().ok())
        else {
            return;
        };
        if !stream
            .dict
            .get(b"Subtype")
            .and_then(|subtype| subtype.as_name())
            .is_ok_and(|subtype| subtype == b"Form")
        {
            return;
        }
        let Ok(data) = stream.decompressed_content_with_limit(MAX_PDF_DECOMPRESSED_BYTES) else {
            return;
        };
        let resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| pdf_dictionary(document, object));
        let form_fonts = resources
            .and_then(|resources| resources.get(b"Font").ok())
            .and_then(|object| pdf_dictionary(document, object))
            .map(|dictionary| {
                pdf_fonts(
                    document,
                    dictionary.iter().filter_map(|(name, font)| {
                        Some((name.clone(), pdf_dictionary(document, font)?))
                    }),
                )
            });
        let form_xobjects = resources
            .and_then(|resources| resources.get(b"XObject").ok())
            .and_then(|object| pdf_dictionary(document, object));
        self.run(
            document,
            &data,
            form_fonts.as_ref().unwrap_or(fonts),
            form_xobjects.or(xobjects),
            depth + 1,
        );
    }
}

/// Text of a PDF for email discovery, laid out from glyph positions: a new line
/// when the baseline moves and a space only for a visible gap, so an address drawn
/// in several styled or kerned pieces stays whole. Pages that fail to decode are
/// skipped, and every stream is bounded by `MAX_PDF_DECOMPRESSED_BYTES` against
/// decompression bombs.
pub(super) fn pdf_text(bytes: &[u8]) -> Result<String, String> {
    let document = lopdf::Document::load_mem(bytes).map_err(|error| error.to_string())?;
    let mut layout = PdfTextLayout::default();
    for page_id in document.get_pages().into_values() {
        let fonts = document
            .get_page_fonts(page_id)
            .map(|fonts| pdf_fonts(&document, fonts))
            .unwrap_or_default();
        let (resources, inherited) = document.get_page_resources(page_id).unwrap_or_default();
        let xobjects = resources
            .into_iter()
            .chain(
                inherited
                    .iter()
                    .filter_map(|id| document.get_dictionary(*id).ok()),
            )
            .find_map(|resources| pdf_dictionary(&document, resources.get(b"XObject").ok()?));
        let Ok(data) = document.get_page_content_with_limit(page_id, MAX_PDF_DECOMPRESSED_BYTES)
        else {
            continue;
        };
        (layout.size, layout.scale, layout.horizontal) = (12.0, 1.0, 1.0);
        layout.run(&document, &data, &fonts, xobjects, 0);
        if !layout.text.ends_with('\n') {
            layout.text.push('\n');
        }
    }
    Ok(layout.text)
}
