//! Writing a report as a PDF — the format for a report that gets *printed*,
//! attached to a ticket, or handed to someone who will never open a
//! spreadsheet.
//!
//! The file is assembled by hand rather than through a PDF crate. PDF's
//! document structure is a header, a list of numbered objects, a cross-
//! reference table of their byte offsets and a trailer; the only genuinely
//! fiddly parts are text metrics and image encoding, and both are solved here
//! with what the tree already carries. That buys the whole format for no new
//! dependency, which matters for a build that deliberately links everything
//! statically from source.
//!
//! Three decisions shape everything below:
//!
//! - **Base-14 fonts, with real widths.** Helvetica and Helvetica-Bold are
//!   built into every PDF viewer, so nothing has to be embedded. But a viewer
//!   only *renders* the text — the wrapping is ours, so the glyph widths have
//!   to be too, or every wrapped cell either overflows its column or wastes a
//!   third of it. The two width tables below are Adobe's.
//! - **Pictures pass through as they arrived where they can.** A JPEG is
//!   already a DCT stream, which is exactly what PDF's `DCTDecode` filter
//!   wants, so a photograph is embedded byte-for-byte. Anything else is
//!   re-encoded to a PNG and its pixel stream reused under `FlateDecode` with
//!   the PNG predictor — the same bytes a PNG file holds, which is why no
//!   deflate dependency is needed to compress them.
//! - **`DETAIL` columns are left out of the table.** Paper has no click, and a
//!   drill-down column is usually a whole JSON body — in a grid it crushes
//!   every other column into a ribbon. The lossless exports (CSV, JSON, xlsx)
//!   still carry them; this one is a summary you can read.

use super::flow::{Header, ImageSpec};
use super::model::{ImageData, OutputColumn, ReportResult};
use super::writer::{
    ImageColumnWidth, ReportWriter, Tint, image_column_px, measured_column_widths, run_cell_tint,
};

/// Writes a report as a printable PDF: a landscape A4 table, the header row
/// repeated on every page, cells tinted like the other visual exports, and an
/// `IMAGE` column's pictures embedded at the size the clause asked for.
pub struct PdfWriter;

// ---------------------------------------------------------------------------
// Page geometry
// ---------------------------------------------------------------------------

/// A4 in PDF points (1/72"), landscape: reports are wide, and portrait A4 fits
/// about four columns before it starts shaving them.
const PAGE_W: f64 = 841.89;
const PAGE_H: f64 = 595.28;

/// Page margin. Generous enough to survive a printer's unprintable edge.
const MARGIN: f64 = 28.0;

/// Body text size, and the header row's. Small, because the alternative to a
/// small table is a table split across pages sideways, which is unreadable in
/// a different way.
const FONT_SIZE: f64 = 7.5;
const TITLE_SIZE: f64 = 13.0;

/// Baseline-to-baseline distance for wrapped cell text.
const LEADING: f64 = FONT_SIZE * 1.25;

/// Space between a cell's text and its column's edge — kept the same on all four
/// sides so a tinted cell looks like a box rather than a smear.
const CELL_PAD: f64 = 2.5;

/// How many wrapped lines one cell may contribute to its row's height. A report
/// cell can hold a whole response body; without a cap a single row is taller
/// than the page and can never be placed at all. What is dropped is marked with
/// an ellipsis, so the reader knows to open a lossless export.
const MAX_CELL_LINES: usize = 24;

/// Points per "character" when converting the shared character-based column
/// measurement (see [`measured_column_widths`]) into page units. Helvetica's
/// digits and lower-case letters are close to half the point size wide.
const PT_PER_CHAR: f64 = FONT_SIZE * 0.55;

/// The narrowest a column may be squeezed to when a table has to be fitted to
/// the page — below this a header is unreadable however few characters it has.
const MIN_COL_W: f64 = 26.0;

/// Pixels are three-quarters of a point (PDF's 72dpi against an image's
/// nominal 96dpi), which is how an `IMAGE(HEIGHT 110)` becomes a box on paper.
const PX_PER_PT: f64 = 96.0 / 72.0;

/// The box a `FIT` picture is drawn in: as wide as its column allows, and no
/// taller than this, so one row can't own a page.
const FIT_IMAGE_MAX_H: f64 = 96.0;

/// Cell tints, matching the spreadsheet and the HTML exactly — a reader who has
/// both open must not have to work out whether the two greens mean the same.
const TINT_GREEN: (f64, f64, f64) = (0.776, 0.937, 0.808);
const TINT_RED: (f64, f64, f64) = (1.0, 0.780, 0.808);
const TINT_AMBER: (f64, f64, f64) = (1.0, 0.922, 0.612);

/// The header row's fill and the grid line, in grey.
const HEAD_FILL: f64 = 0.2;
const GRID_GREY: f64 = 0.72;

impl ReportWriter for PdfWriter {
    fn write(&self, result: &ReportResult, header: &Header) -> Result<Vec<u8>, String> {
        let all_columns = result.resolved_columns(header);
        // `DETAIL` columns leave the table (see the module docs); everything
        // else — including the metrics, which are computed over every column —
        // behaves exactly as in the other exports.
        let (columns, _detail) = super::detail::split_columns(&all_columns);
        let widths = column_widths(&columns, result);
        let doc = Layout {
            title: title_of(header),
            columns: &columns,
            widths: &widths,
            result,
            header,
        }
        .paginate();
        Ok(render(&doc))
    }
}

/// The report's title line: its `# name:` if it has one (with any `{time}`-style
/// output tokens left alone — the name is a label here, not a filename), else a
/// neutral one, because a PDF with no heading looks like a fragment of a bigger
/// document.
fn title_of(header: &Header) -> String {
    let name = header.get("name").unwrap_or("").trim();
    if name.is_empty() {
        "PaperTrail report".to_string()
    } else {
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// Font metrics
// ---------------------------------------------------------------------------

/// Adobe's Helvetica advance widths for the printable ASCII range (codes
/// 32..=126), in 1/1000 em. Wrapping is done here rather than by the viewer, so
/// these are what keep a wrapped column inside its own width.
#[rustfmt::skip]
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// The same, for Helvetica-Bold (the header row and the summary footer).
#[rustfmt::skip]
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611,
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556,
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611,
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// The width used for a character outside the table: the tables cover ASCII,
/// and an accented letter or a symbol is close enough to a lower-case `n` that
/// guessing costs a fraction of a millimetre rather than a broken column.
const FALLBACK_WIDTH: u16 = 556;

/// The advance width of one character at `size`, in points.
fn char_width(c: char, bold: bool, size: f64) -> f64 {
    let table = if bold { &HELVETICA_BOLD } else { &HELVETICA };
    let w = match c as u32 {
        32..=126 => table[c as usize - 32],
        _ => FALLBACK_WIDTH,
    };
    w as f64 * size / 1000.0
}

/// The width of a whole string at `size`, in points.
fn text_width(text: &str, bold: bool, size: f64) -> f64 {
    text.chars().map(|c| char_width(c, bold, size)).sum()
}

/// Wrap `text` to `max_w` points, breaking on its own newlines first and then
/// between words, and splitting a word that is longer than the column rather
/// than letting it run into the next one (a base64 blob or a URL is one word).
fn wrap(text: &str, max_w: f64, bold: bool, size: f64) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let para = para.trim_end_matches('\r');
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut line_w = 0.0;
        for word in para.split(' ') {
            let word_w = text_width(word, bold, size);
            let space_w = if line.is_empty() {
                0.0
            } else {
                char_width(' ', bold, size)
            };
            if !line.is_empty() && line_w + space_w + word_w > max_w {
                out.push(std::mem::take(&mut line));
                line_w = 0.0;
            }
            if word_w > max_w {
                // A single word wider than the column: place what fits, break,
                // repeat. Without this the loop above would emit it on a line
                // of its own and it would still overflow.
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                    line_w = 0.0;
                }
                for c in word.chars() {
                    let cw = char_width(c, bold, size);
                    if line_w + cw > max_w && !line.is_empty() {
                        out.push(std::mem::take(&mut line));
                        line_w = 0.0;
                    }
                    line.push(c);
                    line_w += cw;
                }
                continue;
            }
            if !line.is_empty() {
                line.push(' ');
                line_w += space_w;
            }
            line.push_str(word);
            line_w += word_w;
        }
        out.push(line);
    }
    if out.len() > MAX_CELL_LINES {
        out.truncate(MAX_CELL_LINES);
        if let Some(last) = out.last_mut() {
            last.push('…');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Column sizing
// ---------------------------------------------------------------------------

/// Per-column widths in points: the shared character measurement converted to
/// the page, a picture column sized to its pictures, and the whole table then
/// fitted to the printable width.
fn column_widths(columns: &[OutputColumn], result: &ReportResult) -> Vec<f64> {
    let mut widths: Vec<f64> = measured_column_widths(columns, result)
        .into_iter()
        .map(|chars| (chars as f64 * PT_PER_CHAR).max(MIN_COL_W))
        .collect();
    for (i, c) in columns.iter().enumerate() {
        if let Some(w) = image_column_width(c, result) {
            widths[i] = w;
        }
    }
    fit_to_page(widths, PAGE_W - 2.0 * MARGIN)
}

/// The width a picture column wants, in points: its widest picture. `None` for
/// an ordinary column. A `FIT` column, or one whose pictures all failed to
/// resolve, gets a modest fixed width — never the width of the path text
/// underneath, which is the same rule the HTML and xlsx exports follow.
fn image_column_width(column: &OutputColumn, result: &ReportResult) -> Option<f64> {
    Some(match image_column_px(column, result)? {
        ImageColumnWidth::Widest(px) => px / PX_PER_PT + 2.0 * CELL_PAD,
        ImageColumnWidth::Fit => FIT_IMAGE_MAX_H,
    })
}

/// Squeeze a table onto the page by taking from the widest columns first.
///
/// The same rule as the HTML export's budget fit: binary-search the highest
/// ceiling whose clamped total fits, so a column that was already narrow is
/// never touched and only the sprawling ones give anything up. A table that
/// already fits is returned unchanged — columns are never stretched, because
/// widening a column past its content only adds white space.
fn fit_to_page(mut widths: Vec<f64>, budget: f64) -> Vec<f64> {
    let total: f64 = widths.iter().sum();
    if total <= budget || widths.is_empty() {
        return widths;
    }
    let floor = MIN_COL_W.min(budget / widths.len() as f64);
    let (mut lo, mut hi) = (floor, widths.iter().cloned().fold(0.0, f64::max));
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        let fitted: f64 = widths.iter().map(|w| w.min(mid)).sum();
        if fitted > budget { hi = mid } else { lo = mid }
    }
    for w in &mut widths {
        *w = w.min(lo);
    }
    widths
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// One laid-out cell: either wrapped text or a picture, both already sized.
enum Cell<'a> {
    Text(Vec<String>),
    Picture {
        image: &'a ImageData,
        w: f64,
        h: f64,
    },
}

/// One laid-out row of the table.
struct Row<'a> {
    cells: Vec<Cell<'a>>,
    tints: Vec<Option<Tint>>,
    height: f64,
    /// Drawn in the bold face: the repeated column header and the footer's
    /// statistics rows.
    bold: bool,
    /// Reversed out of a dark band. Only the repeated header row is — a bold
    /// footer on the same band would read as a second set of column names.
    band: bool,
}

/// A page's worth of laid-out rows.
struct Page<'a> {
    rows: Vec<Row<'a>>,
    /// The title is drawn on the first page only; later pages carry the table
    /// alone, with the repeated header row saying what the columns are.
    title: bool,
}

/// Everything the laid-out document needs to be rendered.
struct Doc<'a> {
    title: String,
    widths: &'a [f64],
    pages: Vec<Page<'a>>,
}

struct Layout<'a> {
    title: String,
    columns: &'a [OutputColumn],
    widths: &'a [f64],
    result: &'a ReportResult,
    header: &'a Header,
}

impl<'a> Layout<'a> {
    /// Lay every row out and break the table into pages, repeating the header
    /// row at the top of each one.
    ///
    /// Rows are placed in one pass rather than measured and then placed: a
    /// row's height depends on its own wrapped content, so where the page ends
    /// is only known once the row before it has been laid out.
    fn paginate(self) -> Doc<'a> {
        let body_bottom = MARGIN + LEADING; // room for the page number
        let mut pages: Vec<Page<'a>> = Vec::new();
        let mut current: Vec<Row<'a>> = vec![self.header_row()];
        let mut first = true;
        let mut y = self.body_top(first) - current[0].height;

        let mut rows: Vec<Row<'a>> = (0..self.result.rows.len())
            .map(|r| self.data_row(r))
            .collect();
        // The appended statistics and ground-truth figures, exactly as the flat
        // formats carry them: a footer, in bold, in the same columns.
        for srow in self.result.footer_rows(self.columns, self.header) {
            rows.push(self.summary_row(&srow));
        }

        for row in rows {
            // A row taller than a whole page can't be placed anywhere, so it is
            // only worth breaking when the page already holds something more
            // than its repeated header.
            if y - row.height < body_bottom && current.len() > 1 {
                pages.push(Page {
                    rows: std::mem::take(&mut current),
                    title: first,
                });
                first = false;
                current.push(self.header_row());
                y = self.body_top(first) - current[0].height;
            }
            y -= row.height;
            current.push(row);
        }
        pages.push(Page {
            rows: current,
            title: first,
        });

        Doc {
            title: self.title,
            widths: self.widths,
            pages,
        }
    }

    /// Where the table starts on a page. The first page gives up a line to the
    /// title; the others keep the same top edge so the tables line up when the
    /// pages are laid side by side.
    fn body_top(&self, first: bool) -> f64 {
        let _ = first;
        PAGE_H - MARGIN - TITLE_SIZE * 1.6
    }

    /// The repeated header row.
    fn header_row(&self) -> Row<'a> {
        let mut row = self.text_row(self.columns.iter().map(|c| c.header.clone()).collect());
        row.band = true;
        row
    }

    /// A bold footer row (statistics, ground-truth figures).
    fn summary_row(&self, srow: &super::model::SummaryRow) -> Row<'a> {
        self.text_row((0..self.columns.len()).map(|c| srow.text_cell(c)).collect())
    }

    /// A row of bold text cells — the header band and the footer share one
    /// shape, so they can't drift apart in width or padding.
    fn text_row(&self, values: Vec<String>) -> Row<'a> {
        let cells: Vec<Cell<'a>> = values
            .iter()
            .zip(self.widths)
            .map(|(v, w)| Cell::Text(wrap(v, w - 2.0 * CELL_PAD, true, FONT_SIZE)))
            .collect();
        let height = row_height(&cells);
        Row {
            tints: vec![None; cells.len()],
            cells,
            height,
            bold: true,
            band: false,
        }
    }

    /// One data row, with its pictures and tints resolved.
    fn data_row(&self, r: usize) -> Row<'a> {
        let row = &self.result.rows[r];
        let mut cells = Vec::with_capacity(self.columns.len());
        let mut tints = Vec::with_capacity(self.columns.len());
        for (c, w) in self.columns.iter().zip(self.widths) {
            let value = c.value(row, &self.result.no_match_marker);
            tints.push(run_cell_tint(self.result, r, &c.header, &value));
            match self.result.images.get(&(r, c.header.clone())) {
                Some(img) => {
                    let (pw, ph) = picture_box(c.image, img, *w);
                    cells.push(Cell::Picture {
                        image: img,
                        w: pw,
                        h: ph,
                    });
                }
                None => cells.push(Cell::Text(wrap(
                    &value,
                    w - 2.0 * CELL_PAD,
                    false,
                    FONT_SIZE,
                ))),
            }
        }
        let height = row_height(&cells);
        Row {
            cells,
            tints,
            height,
            bold: false,
            band: false,
        }
    }
}

/// The `(width, height)` in points a cell's picture is drawn at: the size the
/// `IMAGE` clause asked for, scaled down if it would overflow its column, and
/// for a `FIT` column the column's own width capped to a sensible height.
fn picture_box(spec: Option<ImageSpec>, img: &ImageData, col_w: f64) -> (f64, f64) {
    let avail = (col_w - 2.0 * CELL_PAD).max(1.0);
    let (nw, nh) = (img.natural.0.max(1) as f64, img.natural.1.max(1) as f64);
    let (mut w, mut h) = match spec.and_then(|s| s.scaled_size(img.natural)) {
        Some((w, h)) => (w / PX_PER_PT, h / PX_PER_PT),
        None => (avail, avail * nh / nw),
    };
    if w > avail {
        h *= avail / w;
        w = avail;
    }
    if h > FIT_IMAGE_MAX_H && spec.is_none_or(|s| s.fit) {
        w *= FIT_IMAGE_MAX_H / h;
        h = FIT_IMAGE_MAX_H;
    }
    (w, h)
}

/// How tall a row has to be to hold its tallest cell.
fn row_height(cells: &[Cell<'_>]) -> f64 {
    let content = cells
        .iter()
        .map(|c| match c {
            Cell::Text(lines) => lines.len().max(1) as f64 * LEADING,
            Cell::Picture { h, .. } => *h,
        })
        .fold(0.0_f64, f64::max);
    content + 2.0 * CELL_PAD
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Turn the laid-out document into PDF bytes.
fn render(doc: &Doc<'_>) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let catalog = pdf.reserve();
    let pages_id = pdf.reserve();
    let font = pdf.add(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    let font_bold = pdf.add(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );

    // Every distinct picture becomes one XObject, shared by every cell that
    // shows it: a report that repeats a baseline thumbnail down a column must
    // not carry it once per row.
    let mut xobjects: Vec<(String, usize)> = Vec::new();
    let mut by_bytes: std::collections::HashMap<*const u8, String> =
        std::collections::HashMap::new();
    for page in &doc.pages {
        for row in &page.rows {
            for cell in &row.cells {
                if let Cell::Picture { image, .. } = cell {
                    let key = image.bytes.as_ptr();
                    if by_bytes.contains_key(&key) {
                        continue;
                    }
                    let Some(x) = encode_image(image) else {
                        continue;
                    };
                    let name = format!("Im{}", xobjects.len() + 1);
                    let id = pdf.add_stream(&x.dict, &x.data);
                    by_bytes.insert(key, name.clone());
                    xobjects.push((name, id));
                }
            }
        }
    }

    let total = doc.pages.len().max(1);
    let mut page_ids = Vec::new();
    for (i, page) in doc.pages.iter().enumerate() {
        let content = page_content(doc, page, i + 1, total, &by_bytes);
        let content_id = pdf.add_stream("", content.as_bytes());
        let mut xo = String::new();
        for (name, id) in &xobjects {
            xo.push_str(&format!("/{name} {id} 0 R "));
        }
        let page_id = pdf.add(
            format!(
                "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PAGE_W:.2} {PAGE_H:.2}] \
                 /Resources << /Font << /F1 {font} 0 R /F2 {font_bold} 0 R >> \
                 /XObject << {xo}>> >> /Contents {content_id} 0 R >>"
            )
            .into_bytes(),
        );
        page_ids.push(page_id);
    }
    let kids: String = page_ids
        .iter()
        .map(|id| format!("{id} 0 R "))
        .collect::<String>();
    pdf.set(
        pages_id,
        format!(
            "<< /Type /Pages /Count {} /Kids [ {kids}] >>",
            page_ids.len()
        )
        .into_bytes(),
    );
    pdf.set(
        catalog,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    );
    pdf.finish(catalog)
}

/// The content stream for one page: the title (first page only), the table, and
/// the page number.
fn page_content(
    doc: &Doc<'_>,
    page: &Page<'_>,
    number: usize,
    total: usize,
    names: &std::collections::HashMap<*const u8, String>,
) -> String {
    let mut c = String::new();
    let mut y = PAGE_H - MARGIN;
    if page.title {
        y -= TITLE_SIZE;
        c.push_str(&format!(
            "BT /F2 {TITLE_SIZE} Tf {MARGIN:.2} {y:.2} Td {} Tj ET\n",
            pdf_string(&doc.title)
        ));
        y -= TITLE_SIZE * 0.6;
    } else {
        y -= TITLE_SIZE * 2.0;
    }

    for row in &page.rows {
        let top = y;
        y -= row.height;
        let mut x = MARGIN;
        for (i, cell) in row.cells.iter().enumerate() {
            let w = doc.widths.get(i).copied().unwrap_or(MIN_COL_W);
            // Fills first, so nothing is painted over text: the header band,
            // then a tinted cell's own background.
            if row.band {
                c.push_str(&format!(
                    "{HEAD_FILL} {HEAD_FILL} {HEAD_FILL} rg {x:.2} {:.2} {w:.2} {:.2} re f\n",
                    y, row.height
                ));
            } else if let Some(t) = row.tints.get(i).copied().flatten() {
                let (r, g, b) = match t {
                    Tint::Green => TINT_GREEN,
                    Tint::Red => TINT_RED,
                    Tint::Amber => TINT_AMBER,
                };
                c.push_str(&format!(
                    "{r:.3} {g:.3} {b:.3} rg {x:.2} {:.2} {w:.2} {:.2} re f\n",
                    y, row.height
                ));
            }
            match cell {
                Cell::Text(lines) => {
                    let font = if row.bold { "F2" } else { "F1" };
                    let grey = if row.band { 1.0 } else { 0.0 };
                    c.push_str(&format!(
                        "{grey} {grey} {grey} rg BT /{font} {FONT_SIZE} Tf\n"
                    ));
                    let mut ty = top - CELL_PAD - FONT_SIZE;
                    for line in lines {
                        c.push_str(&format!(
                            "1 0 0 1 {:.2} {ty:.2} Tm {} Tj\n",
                            x + CELL_PAD,
                            pdf_string(line)
                        ));
                        ty -= LEADING;
                    }
                    c.push_str("ET\n");
                }
                Cell::Picture {
                    image,
                    w: pw,
                    h: ph,
                } => {
                    if let Some(name) = names.get(&image.bytes.as_ptr()) {
                        let iy = top - CELL_PAD - ph;
                        c.push_str(&format!(
                            "q {pw:.2} 0 0 {ph:.2} {:.2} {iy:.2} cm /{name} Do Q\n",
                            x + CELL_PAD
                        ));
                    }
                }
            }
            x += w;
        }
        // One line under every row, and one down each column edge: enough rule
        // to follow a wide row across the page without the table becoming a
        // cage of ink.
        c.push_str(&format!("{GRID_GREY} G 0.4 w\n"));
        c.push_str(&format!(
            "{MARGIN:.2} {y:.2} m {:.2} {y:.2} l S\n",
            MARGIN + doc.widths.iter().sum::<f64>()
        ));
        let mut vx = MARGIN;
        for w in doc.widths {
            c.push_str(&format!("{vx:.2} {top:.2} m {vx:.2} {y:.2} l S\n"));
            vx += w;
        }
        c.push_str(&format!("{vx:.2} {top:.2} m {vx:.2} {y:.2} l S\n"));
    }

    c.push_str(&format!(
        "0.45 0.45 0.45 rg BT /F1 {FONT_SIZE} Tf {MARGIN:.2} {:.2} Td {} Tj ET\n",
        MARGIN * 0.5,
        pdf_string(&format!("{number} / {total}"))
    ));
    c
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// An image ready to be written as a PDF XObject.
struct XImage {
    dict: String,
    data: Vec<u8>,
}

/// Encode a picture as a PDF image XObject.
///
/// A JPEG is handed to the viewer untouched under `DCTDecode` — PDF speaks JPEG
/// natively, and re-encoding a photograph as raw samples would multiply a
/// report's size by ten. Everything else is decoded and re-encoded as a PNG,
/// whose IDAT stream is exactly a zlib-compressed, PNG-predicted sample stream:
/// `FlateDecode` with `/Predictor 15` reads it as-is. That is what lets this
/// module compress images without a deflate dependency of its own.
///
/// Anything that fails to decode returns `None`, and the cell falls back to its
/// text — a broken picture must never fail a report.
fn encode_image(img: &ImageData) -> Option<XImage> {
    let (w, h) = img.natural;
    if img.mime == "image/jpeg" {
        let decoded =
            image::load_from_memory_with_format(&img.bytes, image::ImageFormat::Jpeg).ok()?;
        let space = match decoded.color().channel_count() {
            1 => "DeviceGray",
            _ => "DeviceRGB",
        };
        return Some(XImage {
            dict: format!(
                "/Type /XObject /Subtype /Image /Width {w} /Height {h} \
                 /ColorSpace /{space} /BitsPerComponent 8 /Filter /DCTDecode"
            ),
            data: img.bytes.clone(),
        });
    }
    let decoded = image::load_from_memory(&img.bytes).ok()?;
    let rgb = flatten_to_rgb(decoded);
    let (w, h) = (rgb.width(), rgb.height());
    let mut png = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let idat = png_idat(&png)?;
    Some(XImage {
        dict: format!(
            "/Type /XObject /Subtype /Image /Width {w} /Height {h} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode \
             /DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns {w} >>"
        ),
        data: idat,
    })
}

/// Composite a picture onto white and drop its alpha channel. PDF can carry
/// transparency as a soft mask, but a report's pictures are photographs and
/// screenshots printed on paper, which is white anyway.
fn flatten_to_rgb(img: image::DynamicImage) -> image::RgbImage {
    let rgba = img.to_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as f32 / 255.0;
        let mix = |c: u8| (c as f32 * a + 255.0 * (1.0 - a)).round() as u8;
        out.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    out
}

/// Concatenate a PNG's IDAT chunks — one zlib stream, split across chunks by
/// the format rather than by anything meaningful.
fn png_idat(png: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 8; // skip the signature
    while i + 8 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().ok()?) as usize;
        let kind = &png[i + 4..i + 8];
        let start = i + 8;
        let end = start.checked_add(len)?;
        if end > png.len() {
            return None;
        }
        if kind == b"IDAT" {
            out.extend_from_slice(&png[start..end]);
        }
        if kind == b"IEND" {
            break;
        }
        i = end + 4; // + CRC
    }
    (!out.is_empty()).then_some(out)
}

// ---------------------------------------------------------------------------
// The PDF container
// ---------------------------------------------------------------------------

/// A PDF string literal, WinAnsi-encoded.
///
/// The base-14 fonts are declared `/WinAnsiEncoding`, which is Latin-1 over the
/// range that matters, so a character up to U+00FF is written as its own byte.
/// Anything beyond becomes `?`: embedding a Unicode font to render one
/// character correctly would multiply the file's size, and the lossless exports
/// carry the exact text.
fn pdf_string(text: &str) -> String {
    let mut out = String::from("(");
    for c in text.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '…' => out.push_str("..."),
            c if (c as u32) < 32 => out.push(' '),
            c if (c as u32) <= 255 => {
                // Written as an octal escape so the file stays 7-bit ASCII and
                // survives being opened in any text editor.
                let b = c as u32;
                if b < 128 {
                    out.push(c);
                } else {
                    out.push_str(&format!("\\{b:03o}"));
                }
            }
            _ => out.push('?'),
        }
    }
    out.push(')');
    out
}

/// A PDF file under construction: numbered objects, written out with a
/// cross-reference table of their byte offsets.
struct Pdf {
    objects: Vec<Option<Vec<u8>>>,
    streams: Vec<Option<(String, Vec<u8>)>>,
}

impl Pdf {
    fn new() -> Self {
        Pdf {
            objects: Vec::new(),
            streams: Vec::new(),
        }
    }

    /// Reserve an object number to be filled in later (a catalog has to name
    /// the page tree, which has to name pages that don't exist yet).
    fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.streams.push(None);
        self.objects.len()
    }

    fn set(&mut self, id: usize, body: Vec<u8>) {
        self.objects[id - 1] = Some(body);
    }

    fn add(&mut self, body: Vec<u8>) -> usize {
        let id = self.reserve();
        self.set(id, body);
        id
    }

    /// Add a stream object: `extra` is any dictionary entries beyond `/Length`.
    fn add_stream(&mut self, extra: &str, data: &[u8]) -> usize {
        let id = self.reserve();
        self.streams[id - 1] = Some((extra.to_string(), data.to_vec()));
        id
    }

    fn finish(self, root: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = vec![0usize; self.objects.len()];
        for i in 0..self.objects.len() {
            offsets[i] = out.len();
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            match (&self.objects[i], &self.streams[i]) {
                (_, Some((extra, data))) => {
                    out.extend_from_slice(
                        format!("<< {extra} /Length {} >>\nstream\n", data.len()).as_bytes(),
                    );
                    out.extend_from_slice(data);
                    out.extend_from_slice(b"\nendstream\n");
                }
                (Some(body), None) => {
                    out.extend_from_slice(body);
                    out.push(b'\n');
                }
                // A reserved object nobody filled in: the file still has to
                // account for its number, so it is written as null.
                (None, None) => out.extend_from_slice(b"null\n"),
            }
            out.extend_from_slice(b"endobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", self.objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                self.objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::{ImageData, ReportRow};
    use std::collections::HashMap;

    fn row(cells: &[(&str, &str)]) -> ReportRow {
        ReportRow {
            cells: cells
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            vars: HashMap::new(),
            key: vec![],
            path: Vec::new(),
            target: None,
        }
    }

    fn result(rows: usize) -> ReportResult {
        ReportResult {
            column_order: vec!["Name".into(), "Result".into()],
            rows: (0..rows)
                .map(|i| {
                    row(&[
                        ("Name", &format!("case {i}")),
                        ("Result", if i % 2 == 0 { "MATCH" } else { "pass -> fail" }),
                    ])
                })
                .collect(),
            ..Default::default()
        }
    }

    fn pdf(result: &ReportResult, header: &Header) -> Vec<u8> {
        PdfWriter.write(result, header).unwrap()
    }

    /// The bare minimum a PDF reader needs to open the file at all: the version
    /// header, a cross-reference table, a trailer naming the catalog, and the
    /// end-of-file marker.
    #[test]
    fn the_output_is_a_structurally_complete_pdf() {
        let bytes = pdf(&result(3), &Header::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("%PDF-1.7"), "{}", &text[..20]);
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Type /Pages"));
        assert!(text.contains("/Type /Page "));
        assert!(text.contains("\nxref\n"));
        assert!(text.contains("/Root "));
        assert!(text.trim_end().ends_with("%%EOF"));
    }

    /// Every offset in the cross-reference table has to be the exact byte at
    /// which that object starts, or a reader rejects the file. This is the one
    /// part of a hand-built PDF with no margin for error at all.
    #[test]
    fn the_cross_reference_offsets_point_at_their_objects() {
        let bytes = pdf(&result(2), &Header::default());
        // Byte offsets, over a document with a binary comment in its second
        // line -- so the table is read out of the bytes, not out of a lossy
        // string whose indices would no longer be the file's.
        let xref = bytes
            .windows(6)
            .rposition(|w| w == b"\nxref\n")
            .expect("an xref table")
            + 6;
        let text = String::from_utf8_lossy(&bytes[xref..]).to_string();
        let mut lines = text.lines();
        let count: usize = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        lines.next(); // the free head entry
        for n in 1..count {
            let entry = lines.next().expect("an entry per object");
            let off: usize = entry.split_whitespace().next().unwrap().parse().unwrap();
            assert!(
                bytes[off..].starts_with(format!("{n} 0 obj").as_bytes()),
                "object {n} is not at offset {off}"
            );
        }
    }

    /// A long report has to break into pages, and every page after the first
    /// has to repeat the column headers — a table whose headings are on a page
    /// you are no longer holding is unreadable.
    #[test]
    fn a_long_report_pages_and_repeats_its_headers() {
        let bytes = pdf(&result(400), &Header::default());
        let text = String::from_utf8_lossy(&bytes);
        let pages = text.matches("/Type /Page ").count();
        assert!(pages > 1, "expected several pages, got {pages}");
        assert_eq!(
            text.matches("(Name) Tj").count(),
            pages,
            "the header row is drawn once per page"
        );
    }

    /// A tinted cell is a filled rectangle behind its text, in the same colours
    /// the spreadsheet and the HTML use.
    #[test]
    fn a_changed_cell_is_tinted() {
        let bytes = pdf(&result(2), &Header::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("1.000 0.922 0.612 rg"),
            "a difference in the Result column is amber, as everywhere else"
        );
        assert!(
            !text.contains("0.776 0.937 0.808 rg"),
            "and a row that merely matched its baseline is not green"
        );
    }

    /// The report's name is its heading. A PDF with no title reads like a page
    /// torn out of something else.
    #[test]
    fn the_report_name_is_the_heading() {
        let mut header = Header::default();
        header
            .lines
            .push(crate::report::flow::HeaderLine::Directive {
                key: "name".into(),
                value: "Nightly face check".into(),
            });
        let bytes = pdf(&result(1), &header);
        assert!(String::from_utf8_lossy(&bytes).contains("(Nightly face check) Tj"));
    }

    /// Wrapping is done here, not by the viewer, so it has to respect the real
    /// glyph widths: every line of a wrapped cell must fit the width it was
    /// given.
    #[test]
    fn wrapped_lines_fit_the_width_they_were_given() {
        let text = "the quick brown fox jumps over the lazy dog and keeps on running";
        for w in [40.0, 80.0, 160.0] {
            for line in wrap(text, w, false, FONT_SIZE) {
                assert!(
                    text_width(&line, false, FONT_SIZE) <= w + 0.01,
                    "{line:?} is wider than {w}"
                );
            }
        }
    }

    /// A single word longer than the column is broken rather than allowed to
    /// run into the next column — a base64 blob or a URL is one word.
    #[test]
    fn an_unbreakable_word_is_split_not_overflowed() {
        let blob = "A".repeat(400);
        let lines = wrap(&blob, 50.0, false, FONT_SIZE);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(text_width(line.trim_end_matches('…'), false, FONT_SIZE) <= 50.01);
        }
    }

    /// A cell holding a whole response body must not produce a row taller than
    /// the page, which could never be placed at all.
    #[test]
    fn a_huge_cell_is_capped_and_marked() {
        let lines = wrap(&"word ".repeat(2000), 60.0, false, FONT_SIZE);
        assert_eq!(lines.len(), MAX_CELL_LINES);
        assert!(lines.last().unwrap().ends_with('…'), "the cut is shown");
    }

    /// A table wider than the page is squeezed by taking from the widest
    /// columns first: a column that already fits comfortably is left alone.
    #[test]
    fn a_wide_table_is_fitted_by_taking_from_the_widest() {
        let fitted = fit_to_page(vec![30.0, 40.0, 600.0, 500.0], 400.0);
        assert_eq!(fitted[0], 30.0, "a narrow column is untouched");
        assert_eq!(fitted[1], 40.0);
        assert!(fitted.iter().sum::<f64>() <= 400.5);
        assert!(fitted[2] > 100.0 && fitted[3] > 100.0);
    }

    /// A table that already fits is never stretched — widening a column past
    /// its content only adds white space.
    #[test]
    fn a_narrow_table_is_left_alone() {
        let widths = vec![40.0, 60.0, 80.0];
        assert_eq!(fit_to_page(widths.clone(), 700.0), widths);
    }

    /// A real, decodable 4x2 PNG. The shared `png_1x1` fixture is a header
    /// with no pixel data -- enough for the sniffers that only read a size, but
    /// this module actually has to decode the picture to embed it.
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbImage::from_fn(4, 2, |x, _| image::Rgb([(x * 60) as u8, 10, 200]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn image_result() -> ReportResult {
        let mut res = ReportResult {
            column_order: vec!["Name".into(), "Frame".into()],
            rows: vec![row(&[
                ("Name", "a"),
                (
                    "Frame",
                    "/home/somebody/Development/sample_images/trimmed/Real/image-real-6/Front-39.jpg",
                ),
            ])],
            ..Default::default()
        };
        res.column_images.insert(
            "Frame".to_string(),
            ImageSpec {
                height: Some(60),
                ..Default::default()
            },
        );
        res.images.insert(
            (0, "Frame".to_string()),
            ImageData {
                bytes: tiny_png(),
                mime: "image/png".to_string(),
                natural: (4, 2),
            },
        );
        res
    }

    /// A picture becomes an image XObject the page draws, not a line of text
    /// showing the path it came from.
    #[test]
    fn a_picture_is_embedded_as_an_image_xobject() {
        let bytes = pdf(&image_result(), &Header::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/Filter /FlateDecode"));
        assert!(text.contains("/Predictor 15"));
        assert!(text.contains("/Im1 Do"));
        assert!(
            !text.contains("Front-39.jpg"),
            "the cell shows the picture, not its path"
        );
    }

    /// A picture column is as wide as its picture, not as wide as the file path
    /// underneath it — the same rule the HTML and spreadsheet exports follow.
    #[test]
    fn a_picture_column_is_sized_to_the_picture() {
        let res = image_result();
        let columns = res.resolved_columns(&Header::default());
        let widths = column_widths(&columns, &res);
        let ci = columns.iter().position(|c| c.header == "Frame").unwrap();
        assert!(widths[ci] < 100.0, "sized to the picture: {}", widths[ci]);
    }

    /// A picture that couldn't be decoded is skipped, not written as a broken
    /// XObject: a bad illustration must never cost the report.
    #[test]
    fn an_undecodable_picture_is_skipped() {
        assert!(
            encode_image(&ImageData {
                bytes: b"not an image at all".to_vec(),
                mime: "image/png".to_string(),
                natural: (10, 10),
            })
            .is_none()
        );
    }

    /// Text is written as a PDF string literal, so the two characters that
    /// close one have to be escaped or the whole content stream is corrupt.
    #[test]
    fn string_literals_escape_what_would_end_them() {
        assert_eq!(pdf_string("a(b)c\\d"), "(a\\(b\\)c\\\\d)");
        assert_eq!(
            pdf_string("café"),
            "(caf\\351)",
            "WinAnsi, as an octal escape"
        );
        assert_eq!(
            pdf_string("日本"),
            "(??)",
            "outside WinAnsi, but not corrupt"
        );
    }

    /// The statistics and ground-truth figures the flat exports put in a footer
    /// belong in this one too — a printed report that omits them is missing the
    /// only part a reader looks at first.
    #[test]
    fn the_footer_carries_the_summary_rows() {
        let mut res = ReportResult {
            column_order: vec!["Time".into()],
            rows: vec![row(&[("Time", "100")]), row(&[("Time", "300")])],
            ..Default::default()
        };
        res.no_match_marker = String::new();
        let mut header = Header::default();
        header
            .lines
            .push(crate::report::flow::HeaderLine::Directive {
                key: "columns".into(),
                value: "Time STATISTICS(MEAN)".into(),
            });
        let bytes = pdf(&res, &header);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("(200) Tj"), "the mean is in the document");
    }

    /// A `DETAIL` column leaves the table: paper has no click, and a drill-down
    /// column is usually a whole JSON body that would crush every other column.
    #[test]
    fn a_detail_column_is_left_out_of_the_table() {
        let mut res = ReportResult {
            column_order: vec!["Name".into(), "Body".into()],
            rows: vec![row(&[("Name", "a"), ("Body", "a very long json body")])],
            ..Default::default()
        };
        res.column_details.insert("Body".to_string());
        let text = String::from_utf8_lossy(&pdf(&res, &Header::default())).to_string();
        assert!(text.contains("(Name) Tj"));
        assert!(!text.contains("(Body) Tj"));
    }
}
