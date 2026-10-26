//! Prepress imposition (a new PDF; the source document is never modified).
//!
//! [`plan`] is the placement math: press-sheet margins and gripper, an n-up grid
//! (sequential or step-and-repeat), saddle-stitch booklet order with optional creep,
//! and work-and-turn / work-and-tumble backs. [`impose`] writes that plan as a PDF
//! with the source pages placed on the trim, clipped to the bleed, and printer marks.
//! Grid, snap and fold marks are not part of this crate.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod write;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Document, Object};
use serde::{Deserialize, Serialize};

pub use write::{Imposed, impose};

/// The most source pages one job will impose.
pub const MAX_PAGES: usize = 2_000;
/// The most sheets (fronts and backs each count) one job will write.
pub const MAX_SHEETS: usize = 2_000;
/// Columns times rows.
const MAX_CELLS: usize = 64;

/// What can go wrong while planning or writing an imposition.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ImposeError {
    #[error("there are no pages to impose")]
    NoPages,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Cos(#[from] pdfcraft_cos::CosError),
}

/// A named press sheet, in PDF points, short side then long side.
pub const SHEETS: &[(&str, f64, f64)] = &[
    ("Letter", 612.0, 792.0),
    ("11x17", 792.0, 1224.0),
    ("12x18", 864.0, 1296.0),
    ("13x19", 936.0, 1368.0),
    ("SRA3", 320.0 * 72.0 / 25.4, 450.0 * 72.0 / 25.4),
];

/// Which way the sheet is fed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

/// The edge that takes the extra gripper margin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Edge {
    Left,
    #[default]
    Bottom,
    Right,
    Top,
}

/// How source pages fill the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Flow {
    /// One page, then the next, across the sheet and onto the next sheet.
    #[default]
    Sequential,
    /// Each source page is repeated until its sheet is full (step-and-repeat).
    Repeat,
}

/// How large the trim is drawn in its cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "kebab-case")]
pub enum Scale {
    /// The largest uniform scale that fits the trim in the cell.
    #[default]
    Auto,
    /// 100%. Refused when the trim does not fit in the cell.
    Actual,
    /// Percent of the trim size (100 is actual size).
    Percent(f64),
}

/// How the back of a sheet lines up with the front.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Duplex {
    /// Fronts only.
    #[default]
    Simplex,
    /// The back uses the same cells as the front (a duplex printer flips the sheet).
    Sheetwise,
    /// The back is mirrored left to right, so turning the sheet over the side edge
    /// puts each back on its front. The gripper edge stays the same.
    WorkAndTurn,
    /// The back is mirrored top to bottom, so tumbling the sheet puts each back on
    /// its front. The gripper edge changes.
    WorkAndTumble,
}

/// Where the bleed comes from.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", content = "amount", rename_all = "kebab-case")]
pub enum Bleed {
    /// The page's bleed box (it defaults to the crop box, which is no extra bleed).
    FromBox,
    /// This many points past the trim, on every side, and never outside the media box.
    Amount(f64),
}

impl Default for Bleed {
    fn default() -> Self {
        Self::Amount(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Binding {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Layout {
    Grid {
        cols: u32,
        rows: u32,
        flow: Flow,
        scale: Scale,
    },
    /// Saddle stitch, two pages per side. The sheet is turned so its long side is horizontal.
    Booklet {
        binding: Binding,
        /// Points of shift toward the spine, per sheet, from the inside of the signature out.
        creep: f64,
    },
}

/// The press sheet and the room around the pages.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub width: f64,
    pub height: f64,
    /// Margin on every edge, in points.
    pub margin: f64,
    pub gutter_x: f64,
    pub gutter_y: f64,
    /// Extra margin on [`Self::gripper_edge`].
    pub gripper: f64,
    pub gripper_edge: Edge,
}

/// Crop, bleed and registration marks, and the slug line.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marks {
    pub crop: bool,
    pub bleed: bool,
    pub registration: bool,
    pub slug: bool,
    /// Gap between the trim (or the bleed edge) and the start of a mark, in points.
    pub offset: f64,
    pub length: f64,
}

impl Default for Marks {
    fn default() -> Self {
        Self { crop: true, bleed: false, registration: false, slug: true, offset: 6.0, length: 12.0 }
    }
}

/// One imposition, without the page list (a saved preset is one of these).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub sheet: Sheet,
    pub layout: Layout,
    pub duplex: Duplex,
    pub bleed: Bleed,
    pub marks: Marks,
    /// Centre each page in its cell. Otherwise the trim sits at the cell's top-left.
    pub center: bool,
    /// Turn every page 90° when that fits the cell better. The whole sheet turns the same way.
    pub auto_rotate: bool,
}

/// Text for the slug. The date is already formatted (`YYYY-MM-DD`); this crate does not read a clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slug {
    pub filename: String,
    pub date: String,
}

/// One source page's trim and clip, in display space (origin at the bottom-left of the
/// displayed crop, y up, after `/Rotate` and `/UserUnit`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeom {
    pub trim: [f64; 4],
    pub clip: [f64; 4],
}

/// One page on a sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub page: usize,
    /// Display space to sheet space.
    pub matrix: [f64; 6],
    /// Clip in display space.
    pub clip: [f64; 4],
    /// The trim on the sheet.
    pub trim: [f64; 4],
    /// Bleed past the trim on the sheet: left, bottom, right, top.
    pub bleed: [f64; 4],
}

/// One output page (a front or a back).
#[derive(Clone, Debug, PartialEq)]
pub struct SheetPlan {
    pub width: f64,
    pub height: f64,
    pub placed: Vec<Placed>,
    /// Axis-aligned mark lines, in sheet space.
    pub lines: Vec<[f64; 4]>,
    /// Registration circles `(cx, cy, radius)`.
    pub circles: Vec<(f64, f64, f64)>,
    pub slug: Option<String>,
}

/// Built-in presets. Names are matched without regard to case.
pub fn presets() -> Vec<(&'static str, Job)> {
    vec![
        ("Business cards 10-up 12x18", business_cards()),
        ("Letter 2-up on 11x17", letter_two_up()),
        ("Saddle-stitch booklet", booklet_preset()),
        ("SRA3 2-up", sra3_two_up()),
    ]
}

pub fn preset(name: &str) -> Option<Job> {
    let want = name.trim();
    presets().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(want)).map(|(_, job)| job)
}

fn inch(v: f64) -> f64 {
    v * 72.0
}

fn business_cards() -> Job {
    Job {
        sheet: Sheet {
            width: inch(18.0),
            height: inch(12.0),
            margin: inch(0.5),
            gutter_x: inch(0.125),
            gutter_y: inch(0.125),
            gripper: 0.0,
            gripper_edge: Edge::Bottom,
        },
        layout: Layout::Grid { cols: 5, rows: 2, flow: Flow::Repeat, scale: Scale::Auto },
        duplex: Duplex::Simplex,
        bleed: Bleed::Amount(inch(0.125)),
        marks: Marks { crop: true, bleed: true, registration: true, slug: true, offset: 6.0, length: 12.0 },
        center: true,
        auto_rotate: true,
    }
}

fn letter_two_up() -> Job {
    Job {
        sheet: Sheet {
            width: inch(17.0),
            height: inch(11.0),
            margin: inch(0.5),
            gutter_x: inch(0.25),
            gutter_y: 0.0,
            gripper: 0.0,
            gripper_edge: Edge::Bottom,
        },
        layout: Layout::Grid { cols: 2, rows: 1, flow: Flow::Sequential, scale: Scale::Auto },
        duplex: Duplex::Simplex,
        bleed: Bleed::FromBox,
        marks: Marks { crop: true, bleed: false, registration: false, slug: true, offset: 6.0, length: 12.0 },
        center: true,
        auto_rotate: true,
    }
}

fn booklet_preset() -> Job {
    Job {
        sheet: Sheet {
            width: inch(17.0),
            height: inch(11.0),
            margin: inch(0.5),
            gutter_x: 0.0,
            gutter_y: 0.0,
            gripper: 0.0,
            gripper_edge: Edge::Bottom,
        },
        layout: Layout::Booklet { binding: Binding::Left, creep: 0.0 },
        duplex: Duplex::Sheetwise,
        bleed: Bleed::FromBox,
        marks: Marks::default(),
        center: true,
        auto_rotate: true,
    }
}

fn sra3_two_up() -> Job {
    let (_, short, long) = SHEETS[4];
    Job {
        sheet: Sheet { width: long, height: short, margin: inch(0.4), gutter_x: inch(0.2), gutter_y: 0.0, gripper: 0.0, gripper_edge: Edge::Bottom },
        layout: Layout::Grid { cols: 2, rows: 1, flow: Flow::Sequential, scale: Scale::Auto },
        duplex: Duplex::Simplex,
        bleed: Bleed::FromBox,
        marks: Marks::default(),
        center: true,
        auto_rotate: true,
    }
}

impl Default for Job {
    fn default() -> Self {
        letter_two_up()
    }
}

/// The output page size, in points. A booklet is turned so its long side is horizontal.
pub fn output_size(job: &Job) -> (f64, f64) {
    let (w, h) = (job.sheet.width, job.sheet.height);
    if matches!(job.layout, Layout::Booklet { .. }) && w < h { (h, w) } else { (w, h) }
}

/// Apply portrait or landscape to a short×long sheet. Equal sides stay as they are.
pub fn orient(short: f64, long: f64, orientation: Orientation) -> (f64, f64) {
    let (w, h) = (short.min(long), short.max(long));
    match orientation {
        Orientation::Portrait => (w, h),
        Orientation::Landscape => (h, w),
    }
}

/// Trim and clip for every page, in display space.
pub fn page_geoms(doc: &Document, bleed: Bleed) -> Result<Vec<PageGeom>, ImposeError> {
    let pages = pdfcraft_model::pages(doc);
    let mut out = Vec::with_capacity(pages.len());
    for (index, page) in pages.iter().enumerate() {
        out.push(geom_of(doc, page, bleed).map_err(|e| ImposeError::Invalid(format!("page {}: {e}", index + 1)))?);
    }
    Ok(out)
}

fn geom_of(doc: &Document, page: &pdfcraft_model::Page, bleed: Bleed) -> Result<PageGeom, String> {
    let media = rect_of(doc, page.dict.get(b"MediaBox")).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = page.crop(doc);
    let trim_user = rect_of(doc, page.dict.get(b"TrimBox")).map(|r| intersect(r, media)).filter(|r| span(*r)).unwrap_or(crop);
    let bleed_user = rect_of(doc, page.dict.get(b"BleedBox")).map(|r| intersect(r, media)).filter(|r| span(*r)).unwrap_or(crop);
    let to_display = Matrix(page.view_matrix(doc)).invert().ok_or_else(|| "the page box cannot be placed".to_string())?;
    let trim = to_display.bbox(trim_user);
    let media_d = to_display.bbox(media);
    if !finite_rect(trim) || !span(trim) {
        return Err("the trim box has no area".into());
    }
    let mut clip = match bleed {
        Bleed::FromBox => union(to_display.bbox(bleed_user), trim),
        Bleed::Amount(amount) => {
            if !amount.is_finite() || amount < 0.0 {
                return Err("the bleed must be zero or more".into());
            }
            intersect(expand(trim, amount), media_d)
        }
    };
    if !span(clip) {
        clip = trim;
    }
    clip = union(clip, trim);
    Ok(PageGeom { trim, clip })
}

/// Lay `pages` (0-based indexes into `geoms`) out on sheets.
pub fn plan(geoms: &[PageGeom], pages: &[usize], job: &Job, slug: &Slug) -> Result<Vec<SheetPlan>, ImposeError> {
    validate(job)?;
    if pages.is_empty() {
        return Err(ImposeError::NoPages);
    }
    if pages.len() > MAX_PAGES {
        return Err(ImposeError::Invalid(format!("at most {MAX_PAGES} pages can be imposed at once")));
    }
    for &page in pages {
        if geoms.get(page).is_none() {
            return Err(ImposeError::Invalid(format!("page {} is out of range", page.saturating_add(1))));
        }
    }
    let mut sheets = match job.layout {
        Layout::Grid { cols, rows, flow, scale } => grid(geoms, pages, job, cols as usize, rows as usize, flow, scale)?,
        Layout::Booklet { binding, creep } => booklet(geoms, pages, job, binding, creep)?,
    };
    if sheets.len() > MAX_SHEETS {
        return Err(ImposeError::Invalid(format!("at most {MAX_SHEETS} sheets can be imposed at once")));
    }
    if job.marks.slug {
        let count = sheets.len();
        for (index, sheet) in sheets.iter_mut().enumerate() {
            sheet.slug = Some(slug_line(slug, index, count));
        }
    }
    Ok(sheets)
}

fn validate(job: &Job) -> Result<(), ImposeError> {
    let s = &job.sheet;
    let bad = |m: &str| ImposeError::Invalid(m.to_string());
    if !(s.width.is_finite() && s.height.is_finite()) || s.width < 72.0 || s.height < 72.0 || s.width > inch(200.0) || s.height > inch(200.0) {
        return Err(bad("the sheet must be between 1 and 200 inches on each side"));
    }
    for (name, v) in [("margin", s.margin), ("gutter", s.gutter_x), ("gutter", s.gutter_y), ("gripper", s.gripper)] {
        if !v.is_finite() || v < 0.0 || v > s.width.min(s.height) {
            return Err(bad(&format!("the {name} must be zero or more and fit on the sheet")));
        }
    }
    if !job.marks.offset.is_finite() || !(0.0..=72.0).contains(&job.marks.offset) {
        return Err(bad("the mark offset must be from 0 to 72 points"));
    }
    if !job.marks.length.is_finite() || !(0.5..=72.0).contains(&job.marks.length) {
        return Err(bad("the mark length must be from 0.5 to 72 points"));
    }
    if let Layout::Grid { cols, rows, scale, .. } = job.layout {
        let cells = (cols as usize).checked_mul(rows as usize).filter(|n| (1..=MAX_CELLS).contains(n));
        if cells.is_none() || cols == 0 || rows == 0 {
            return Err(bad("the grid must be from 1 by 1 up to 64 cells"));
        }
        match scale {
            Scale::Percent(p) if p.is_finite() && p > 0.0 && p <= 1000.0 => {}
            Scale::Percent(_) => return Err(bad("the scale must be from just above 0% to 1000%")),
            Scale::Auto | Scale::Actual => {}
        }
    }
    if let Layout::Booklet { creep, .. } = job.layout
        && (!creep.is_finite() || !(0.0..=72.0).contains(&creep))
    {
        return Err(bad("creep must be from 0 to 72 points per sheet"));
    }
    if let Bleed::Amount(a) = job.bleed
        && (!a.is_finite() || !(0.0..=144.0).contains(&a))
    {
        return Err(bad("the bleed must be from 0 to 144 points"));
    }
    Ok(())
}

fn grid(geoms: &[PageGeom], pages: &[usize], job: &Job, cols: usize, rows: usize, flow: Flow, scale: Scale) -> Result<Vec<SheetPlan>, ImposeError> {
    let (size, area) = printable(job, false)?;
    let cells = cols * rows;
    let cell = cell_size(area, cols, rows, job.sheet.gutter_x, job.sheet.gutter_y)?;
    let quarter = rotation(geoms, pages, cell, scale, job.auto_rotate);
    let mut sides: Vec<Vec<Option<usize>>> = Vec::new();
    match flow {
        Flow::Sequential => {
            let per = if job.duplex == Duplex::Simplex { cells } else { cells.saturating_mul(2) };
            if per == 0 {
                return Err(ImposeError::Invalid("the grid is empty".into()));
            }
            for chunk in pages.chunks(per) {
                let mut front = vec![None; cells];
                let mut back = vec![None; cells];
                for (i, &page) in chunk.iter().enumerate() {
                    if i < cells {
                        if let Some(slot) = front.get_mut(i) {
                            *slot = Some(page);
                        }
                    } else if let Some(slot) = back.get_mut(i - cells) {
                        *slot = Some(page);
                    }
                }
                sides.push(front);
                if job.duplex != Duplex::Simplex && back.iter().any(|p| p.is_some()) {
                    sides.push(mirror_back(back, cols, rows, job.duplex));
                }
            }
        }
        Flow::Repeat => {
            let step = if job.duplex == Duplex::Simplex { 1 } else { 2 };
            for pair in pages.chunks(step) {
                let front = pair.first().copied().map(|page| vec![Some(page); cells]).unwrap_or_default();
                sides.push(front);
                if job.duplex != Duplex::Simplex
                    && let Some(&page) = pair.get(1)
                {
                    sides.push(mirror_back(vec![Some(page); cells], cols, rows, job.duplex));
                }
            }
        }
    }
    let mut sheets = Vec::new();
    for side in sides {
        sheets.push(place_side(geoms, job, size, area, cols, rows, cell, quarter, &side, &vec![0.0; cells])?);
    }
    Ok(sheets)
}

fn booklet(geoms: &[PageGeom], pages: &[usize], job: &Job, binding: Binding, creep: f64) -> Result<Vec<SheetPlan>, ImposeError> {
    let (size, area) = printable(job, true)?;
    let cell = cell_size(area, 2, 1, job.sheet.gutter_x, 0.0)?;
    let n4 = pages.len().div_ceil(4).saturating_mul(4);
    if n4 / 4 > MAX_SHEETS {
        return Err(ImposeError::Invalid(format!("at most {MAX_SHEETS} sheets can be imposed at once")));
    }
    let at = |i: usize| pages.get(i).copied();
    let sheets_n = n4 / 4;
    let mut sheets = Vec::new();
    // The first page decides the turn, from a cell of one half of the sheet.
    let sample: Vec<usize> = pages.iter().copied().take(1).collect();
    let quarter = rotation(geoms, &sample, cell, Scale::Auto, job.auto_rotate);
    for k in 0..sheets_n {
        let amount = creep * (sheets_n - 1 - k) as f64;
        let front = (at(n4 - 1 - 2 * k), at(2 * k));
        let back = (at(2 * k + 1), at(n4 - 2 - 2 * k));
        for (left, right) in [front, back] {
            let (left, right) = if binding == Binding::Right { (right, left) } else { (left, right) };
            let side = vec![left, right];
            // Toward the spine: the left page moves right, the right page moves left.
            let shifts = vec![amount, -amount];
            sheets.push(place_side(geoms, job, size, area, 2, 1, cell, quarter, &side, &shifts)?);
        }
    }
    Ok(sheets)
}

#[allow(clippy::too_many_arguments)]
fn place_side(
    geoms: &[PageGeom],
    job: &Job,
    size: (f64, f64),
    area: [f64; 4],
    cols: usize,
    rows: usize,
    cell: (f64, f64),
    quarter: u8,
    side: &[Option<usize>],
    shifts: &[f64],
) -> Result<SheetPlan, ImposeError> {
    let mut sheet = SheetPlan { width: size.0, height: size.1, placed: Vec::new(), lines: Vec::new(), circles: Vec::new(), slug: None };
    let (cw, ch) = cell;
    for row in 0..rows {
        for col in 0..cols {
            let slot = row * cols + col;
            let x = area[0] + col as f64 * (cw + job.sheet.gutter_x);
            let top = area[3] - row as f64 * (ch + job.sheet.gutter_y);
            let rect = [x, top - ch, x + cw, top];
            let shift = shifts.get(slot).copied().unwrap_or(0.0);
            let Some(page) = side.get(slot).copied().flatten() else {
                continue;
            };
            let geom = geoms.get(page).ok_or_else(|| ImposeError::Invalid(format!("page {} is out of range", page + 1)))?;
            let placed = place_one(geom, page, rect, quarter, job, shift)?;
            if job.marks.crop || job.marks.bleed {
                sheet.lines.extend(mark_lines(&placed, size, &job.marks));
            }
            sheet.placed.push(placed);
        }
    }
    if job.marks.registration {
        sheet.circles.extend(registration(job, size));
        for (cx, cy, r) in &sheet.circles {
            sheet.lines.push([cx - r, *cy, cx + r, *cy]);
            sheet.lines.push([*cx, cy - r, *cx, cy + r]);
        }
    }
    Ok(sheet)
}

fn place_one(geom: &PageGeom, page: usize, cell: [f64; 4], quarter: u8, job: &Job, shift_x: f64) -> Result<Placed, ImposeError> {
    let [tx0, ty0, tx1, ty1] = geom.trim;
    let (tw, th) = ((tx1 - tx0).abs(), (ty1 - ty0).abs());
    let (cw, ch) = (cell[2] - cell[0], cell[3] - cell[1]);
    let (rw, rh) = if quarter == 1 { (th, tw) } else { (tw, th) };
    let scale = match match_scale(job, rw, rh, cw, ch) {
        Some(s) => s,
        None => return Err(ImposeError::Invalid(format!("page {} does not fit in its cell at this scale", page.saturating_add(1)))),
    };
    let (pw, ph) = (rw * scale, rh * scale);
    let (mut ox, oy) = if job.center { (cell[0] + (cw - pw) / 2.0, cell[1] + (ch - ph) / 2.0) } else { (cell[0], cell[3] - ph) };
    ox += shift_x;
    let matrix = if quarter == 1 {
        [0.0, scale, -scale, 0.0, ox + scale * ty1, oy - scale * tx0]
    } else {
        [scale, 0.0, 0.0, scale, ox - scale * tx0, oy - scale * ty0]
    };
    if !matrix.iter().all(|v| v.is_finite()) {
        return Err(ImposeError::Invalid(format!("page {} cannot be placed", page + 1)));
    }
    let m = Matrix(matrix);
    let trim = m.bbox(geom.trim);
    let clip_box = m.bbox(geom.clip);
    let bleed =
        [(trim[0] - clip_box[0]).max(0.0), (trim[1] - clip_box[1]).max(0.0), (clip_box[2] - trim[2]).max(0.0), (clip_box[3] - trim[3]).max(0.0)];
    Ok(Placed { page, matrix, clip: geom.clip, trim, bleed })
}

fn match_scale(job: &Job, rw: f64, rh: f64, cw: f64, ch: f64) -> Option<f64> {
    if rw <= 0.0 || rh <= 0.0 || cw <= 0.0 || ch <= 0.0 {
        return None;
    }
    let scale = match job.layout {
        Layout::Grid { scale, .. } => scale,
        Layout::Booklet { .. } => Scale::Auto,
    };
    let fits = |s: f64| rw * s <= cw + 0.05 && rh * s <= ch + 0.05 && s.is_finite() && s > 0.0;
    match scale {
        Scale::Auto => {
            let s = (cw / rw).min(ch / rh);
            fits(s).then_some(s)
        }
        Scale::Actual => fits(1.0).then_some(1.0),
        Scale::Percent(p) => {
            let s = p / 100.0;
            fits(s).then_some(s)
        }
    }
}

fn rotation(geoms: &[PageGeom], pages: &[usize], cell: (f64, f64), scale: Scale, auto: bool) -> u8 {
    if !auto {
        return 0;
    }
    let Some(geom) = pages.first().and_then(|p| geoms.get(*p)) else { return 0 };
    let (tw, th) = ((geom.trim[2] - geom.trim[0]).abs(), (geom.trim[3] - geom.trim[1]).abs());
    let (cw, ch) = cell;
    let score = |w: f64, h: f64| -> f64 {
        match scale {
            Scale::Auto => {
                if w <= 0.0 || h <= 0.0 {
                    0.0
                } else {
                    (cw / w).min(ch / h)
                }
            }
            Scale::Actual => {
                if w <= cw + 0.05 && h <= ch + 0.05 {
                    1.0
                } else {
                    0.0
                }
            }
            Scale::Percent(p) => {
                let s = p / 100.0;
                if w * s <= cw + 0.05 && h * s <= ch + 0.05 { s } else { 0.0 }
            }
        }
    };
    if score(th, tw) > score(tw, th) + 1e-6 { 1 } else { 0 }
}

/// The sheet size and the printable rectangle `[left, bottom, right, top]`.
/// A booklet is turned so the long side is horizontal.
fn printable(job: &Job, booklet: bool) -> Result<((f64, f64), [f64; 4]), ImposeError> {
    let mut size = (job.sheet.width, job.sheet.height);
    if booklet && size.0 < size.1 {
        size = (size.1, size.0);
    }
    let g = job.sheet.gripper;
    let m = job.sheet.margin;
    let (mut left, mut bottom, mut right, mut top) = (m, m, m, m);
    match job.sheet.gripper_edge {
        Edge::Left => left += g,
        Edge::Bottom => bottom += g,
        Edge::Right => right += g,
        Edge::Top => top += g,
    }
    let area = [left, bottom, size.0 - right, size.1 - top];
    if area[2] - area[0] < 36.0 || area[3] - area[1] < 36.0 {
        return Err(ImposeError::Invalid("the margins and gripper leave no room for pages".into()));
    }
    Ok((size, area))
}

fn cell_size(area: [f64; 4], cols: usize, rows: usize, gx: f64, gy: f64) -> Result<(f64, f64), ImposeError> {
    let (aw, ah) = (area[2] - area[0], area[3] - area[1]);
    let cw = (aw - gx * (cols.saturating_sub(1) as f64)) / cols as f64;
    let ch = (ah - gy * (rows.saturating_sub(1) as f64)) / rows as f64;
    if !cw.is_finite() || !ch.is_finite() || cw < 18.0 || ch < 18.0 {
        return Err(ImposeError::Invalid("the grid does not fit on the sheet".into()));
    }
    Ok((cw, ch))
}

fn mirror_back(logical: Vec<Option<usize>>, cols: usize, rows: usize, duplex: Duplex) -> Vec<Option<usize>> {
    let mut physical = vec![None; logical.len()];
    for row in 0..rows {
        for col in 0..cols {
            let i = row * cols + col;
            let (pc, pr) = match duplex {
                Duplex::WorkAndTurn => (cols - 1 - col, row),
                Duplex::WorkAndTumble => (col, rows - 1 - row),
                Duplex::Simplex | Duplex::Sheetwise => (col, row),
            };
            let pi = pr * cols + pc;
            if let Some(slot) = physical.get_mut(pi) {
                *slot = logical.get(i).copied().flatten();
            }
        }
    }
    physical
}

fn mark_lines(placed: &Placed, size: (f64, f64), marks: &Marks) -> Vec<[f64; 4]> {
    let [x0, y0, x1, y1] = placed.trim;
    let [bl, bb, br, bt] = placed.bleed;
    let (off, len) = (marks.offset, marks.length);
    let mut lines = Vec::new();
    // Crop marks align with the trim and stop short of the bleed.
    if marks.crop {
        let corners = [(x0, y0, -1.0, -1.0), (x1, y0, 1.0, -1.0), (x0, y1, -1.0, 1.0), (x1, y1, 1.0, 1.0)];
        for (x, y, sx, sy) in corners {
            let bleed_x = if sx < 0.0 { bl } else { br };
            let bleed_y = if sy < 0.0 { bb } else { bt };
            push_line(&mut lines, size, [x + sx * (bleed_x + off + len), y, x + sx * (bleed_x + off), y]);
            push_line(&mut lines, size, [x, y + sy * (bleed_y + off + len), x, y + sy * (bleed_y + off)]);
        }
    }
    // Bleed marks align with the bleed edge, only on a side that has some.
    if marks.bleed {
        let corners = [
            (x0 - bl, y0 - bb, -1.0, -1.0, bl, bb),
            (x1 + br, y0 - bb, 1.0, -1.0, br, bb),
            (x0 - bl, y1 + bt, -1.0, 1.0, bl, bt),
            (x1 + br, y1 + bt, 1.0, 1.0, br, bt),
        ];
        for (x, y, sx, sy, bx, by) in corners {
            if bx > 0.5 {
                push_line(&mut lines, size, [x + sx * (off + len), y, x + sx * off, y]);
            }
            if by > 0.5 {
                push_line(&mut lines, size, [x, y + sy * (off + len), x, y + sy * off]);
            }
        }
    }
    lines
}

fn push_line(lines: &mut Vec<[f64; 4]>, size: (f64, f64), line: [f64; 4]) {
    let horizontal = (line[1] - line[3]).abs() < 0.01;
    let a = if horizontal {
        let y = line[1];
        if !(1.0..=size.1 - 1.0).contains(&y) {
            return;
        }
        let (x0, x1) = (line[0].min(line[2]).clamp(1.0, size.0 - 1.0), line[0].max(line[2]).clamp(1.0, size.0 - 1.0));
        if x1 - x0 < 1.0 {
            return;
        }
        [x0, y, x1, y]
    } else {
        let x = line[0];
        if !(1.0..=size.0 - 1.0).contains(&x) {
            return;
        }
        let (y0, y1) = (line[1].min(line[3]).clamp(1.0, size.1 - 1.0), line[1].max(line[3]).clamp(1.0, size.1 - 1.0));
        if y1 - y0 < 1.0 {
            return;
        }
        [x, y0, x, y1]
    };
    lines.push(a);
}

fn registration(job: &Job, size: (f64, f64)) -> Vec<(f64, f64, f64)> {
    let m = job.sheet.margin;
    let g = job.sheet.gripper;
    let band = |edge: Edge| m + if job.sheet.gripper_edge == edge { g } else { 0.0 };
    let r = 5.5;
    let mut out = Vec::new();
    let left = band(Edge::Left);
    let right = band(Edge::Right);
    let bottom = band(Edge::Bottom);
    let top = band(Edge::Top);
    if left >= 16.0 {
        out.push((left / 2.0, size.1 / 2.0, r));
    }
    if right >= 16.0 {
        out.push((size.0 - right / 2.0, size.1 / 2.0, r));
    }
    if bottom >= 16.0 {
        out.push((size.0 / 2.0, bottom / 2.0, r));
    }
    if top >= 16.0 {
        out.push((size.0 / 2.0, size.1 - top / 2.0, r));
    }
    out
}

fn slug_line(slug: &Slug, index: usize, count: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !slug.filename.is_empty() {
        parts.push(slug.filename.clone());
    }
    if !slug.date.is_empty() {
        parts.push(slug.date.clone());
    }
    parts.push(format!("{} of {count}", index + 1));
    let mut line = parts.join("  ");
    if line.chars().count() > 120 {
        line = line.chars().take(120).collect();
    }
    line
}

fn rect_of(doc: &Document, o: Option<&Object>) -> Option<[f64; 4]> {
    let o = doc.resolve(o?);
    let v: Vec<f64> = o.as_array()?.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    (v.len() == 4 && v.iter().all(|n| n.is_finite())).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn expand(r: [f64; 4], amount: f64) -> [f64; 4] {
    [r[0] - amount, r[1] - amount, r[2] + amount, r[3] + amount]
}

fn span(r: [f64; 4]) -> bool {
    finite_rect(r) && r[2] - r[0] > 0.5 && r[3] - r[1] > 0.5
}

fn finite_rect(r: [f64; 4]) -> bool {
    r.iter().all(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn card() -> PageGeom {
        // 2 × 3.5 in, with 9 pt of bleed already in the clip.
        PageGeom { trim: [0.0, 0.0, inch(2.0), inch(3.5)], clip: [-9.0, -9.0, inch(2.0) + 9.0, inch(3.5) + 9.0] }
    }

    fn letter() -> PageGeom {
        PageGeom { trim: [0.0, 0.0, inch(8.5), inch(11.0)], clip: [0.0, 0.0, inch(8.5), inch(11.0)] }
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 0.05, "{a} != {b}");
    }

    #[test]
    fn booklet_orders_an_eight_page_signature() {
        let geoms = vec![letter(); 8];
        let pages: Vec<usize> = (0..8).collect();
        let sheets = plan(&geoms, &pages, &booklet_preset(), &Slug::default()).unwrap();
        assert_eq!(sheets.len(), 4, "two sheets, front and back");
        let order: Vec<Vec<usize>> = sheets.iter().map(|s| s.placed.iter().map(|p| p.page).collect()).collect();
        // Outside: 8|1 then 2|7. Inside: 6|3 then 4|5. (0-based)
        assert_eq!(order, vec![vec![7, 0], vec![1, 6], vec![5, 2], vec![3, 4]]);
    }

    #[test]
    fn booklet_pads_with_blanks_to_a_multiple_of_four() {
        let geoms = vec![letter(); 5];
        let sheets = plan(&geoms, &[0, 1, 2, 3, 4], &booklet_preset(), &Slug::default()).unwrap();
        assert_eq!(sheets.len(), 4);
        // n = 8. Front of the outside sheet is (blank, page 1). Back is (page 2, blank).
        assert_eq!(sheets[0].placed.iter().map(|p| p.page).collect::<Vec<_>>(), vec![0]);
        let centre = (sheets[0].placed[0].trim[0] + sheets[0].placed[0].trim[2]) / 2.0;
        assert!(centre > sheets[0].width / 2.0, "the only page is the right-hand one");
        assert_eq!(sheets[1].placed.iter().map(|p| p.page).collect::<Vec<_>>(), vec![1]);
        let back = (sheets[1].placed[0].trim[0] + sheets[1].placed[0].trim[2]) / 2.0;
        assert!(back < sheets[1].width / 2.0, "page 2 is the left-hand one");
    }

    #[test]
    fn creep_shifts_the_outer_sheet_toward_the_spine() {
        let geoms = vec![letter(); 8];
        let mut job = booklet_preset();
        job.layout = Layout::Booklet { binding: Binding::Left, creep: 3.0 };
        let sheets = plan(&geoms, &(0..8).collect::<Vec<_>>(), &job, &Slug::default()).unwrap();
        let outer_left = sheets[0].placed[0].trim[0];
        let inner_left = sheets[2].placed[0].trim[0];
        near(outer_left - inner_left, 3.0);
        let outer_right = sheets[0].placed[1].trim[0];
        let inner_right = sheets[2].placed[1].trim[0];
        near(inner_right - outer_right, 3.0);
    }

    #[test]
    fn step_and_repeat_fills_a_sheet_with_one_page() {
        let geoms = vec![card(); 3];
        let sheets = plan(&geoms, &[0, 1, 2], &business_cards(), &Slug::default()).unwrap();
        assert_eq!(sheets.len(), 3);
        assert_eq!(sheets[0].placed.len(), 10);
        assert!(sheets[0].placed.iter().all(|p| p.page == 0));
        assert!(sheets[1].placed.iter().all(|p| p.page == 1));
        assert!(sheets[2].placed.iter().all(|p| p.page == 2));
    }

    #[test]
    fn sequential_flows_across_sheets() {
        let geoms = vec![letter(); 5];
        let sheets = plan(&geoms, &[0, 1, 2, 3, 4], &letter_two_up(), &Slug::default()).unwrap();
        let order: Vec<Vec<usize>> = sheets.iter().map(|s| s.placed.iter().map(|p| p.page).collect()).collect();
        assert_eq!(order, vec![vec![0, 1], vec![2, 3], vec![4]]);
    }

    #[test]
    fn work_and_turn_mirrors_the_back_columns() {
        let geoms = vec![letter(); 4];
        let mut job = letter_two_up();
        job.duplex = Duplex::WorkAndTurn;
        let sheets = plan(&geoms, &[0, 1, 2, 3], &job, &Slug::default()).unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].placed.iter().map(|p| p.page).collect::<Vec<_>>(), vec![0, 1]);
        // The page that backs up the left front is printed on the right.
        assert_eq!(sheets[1].placed.iter().map(|p| p.page).collect::<Vec<_>>(), vec![3, 2]);
    }

    #[test]
    fn work_and_tumble_mirrors_the_back_rows() {
        let geoms = vec![card(); 8];
        let mut job = business_cards();
        job.layout = Layout::Grid { cols: 2, rows: 2, flow: Flow::Sequential, scale: Scale::Auto };
        job.duplex = Duplex::WorkAndTumble;
        job.bleed = Bleed::Amount(0.0);
        let sheets = plan(&geoms, &(0..8).collect::<Vec<_>>(), &job, &Slug::default()).unwrap();
        assert_eq!(sheets[0].placed.iter().map(|p| p.page).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!(sheets[1].placed.iter().map(|p| p.page).collect::<Vec<_>>(), vec![6, 7, 4, 5]);
    }

    #[test]
    fn a_page_is_centred_on_its_trim() {
        let geoms = vec![card()];
        let mut job = business_cards();
        job.layout = Layout::Grid { cols: 1, rows: 1, flow: Flow::Repeat, scale: Scale::Actual };
        job.bleed = Bleed::Amount(0.0);
        job.auto_rotate = false;
        let sheets = plan(&geoms, &[0], &job, &Slug::default()).unwrap();
        let trim = sheets[0].placed[0].trim;
        let (w, h) = (sheets[0].width, sheets[0].height);
        // Actual size, one cell, centred in the printable area (margin 0.5 in, no gripper).
        let area_cx = (inch(0.5) + (w - inch(0.5))) / 2.0;
        let area_cy = (inch(0.5) + (h - inch(0.5))) / 2.0;
        near((trim[0] + trim[2]) / 2.0, area_cx);
        near((trim[1] + trim[3]) / 2.0, area_cy);
        near(trim[2] - trim[0], inch(2.0));
        near(trim[3] - trim[1], inch(3.5));
    }

    #[test]
    fn actual_size_refuses_a_page_that_does_not_fit() {
        let geoms = vec![letter()];
        let mut job = business_cards();
        job.layout = Layout::Grid { cols: 5, rows: 2, flow: Flow::Repeat, scale: Scale::Actual };
        job.auto_rotate = false;
        let err = plan(&geoms, &[0], &job, &Slug::default()).unwrap_err();
        assert!(matches!(err, ImposeError::Invalid(_)), "{err}");
    }

    #[test]
    fn the_gripper_insets_one_edge() {
        let geoms = vec![card()];
        let mut job = business_cards();
        job.layout = Layout::Grid { cols: 1, rows: 1, flow: Flow::Repeat, scale: Scale::Actual };
        job.bleed = Bleed::Amount(0.0);
        job.auto_rotate = false;
        job.sheet.gripper = inch(1.0);
        job.sheet.gripper_edge = Edge::Bottom;
        let sheets = plan(&geoms, &[0], &job, &Slug::default()).unwrap();
        let trim = sheets[0].placed[0].trim;
        // The printable area's bottom is margin + gripper.
        assert!(trim[1] >= inch(0.5) + inch(1.0) - 0.1);
    }

    #[test]
    fn crop_marks_sit_outside_the_bleed() {
        let geoms = vec![card()];
        let mut job = business_cards();
        job.layout = Layout::Grid { cols: 1, rows: 1, flow: Flow::Repeat, scale: Scale::Actual };
        job.auto_rotate = false;
        job.marks.bleed = false;
        job.marks.registration = false;
        let sheets = plan(&geoms, &[0], &job, &Slug::default()).unwrap();
        let trim = sheets[0].placed[0].trim;
        let bleed = 9.0;
        // A horizontal crop mark on the trim's bottom edge, ending `offset` past the bleed.
        let mark = sheets[0].lines.iter().find(|l| (l[1] - trim[1]).abs() < 0.05 && l[2] < trim[0]).copied();
        let mark = mark.expect("a crop mark left of the trim");
        near(mark[2], trim[0] - bleed - job.marks.offset);
    }

    #[test]
    fn presets_round_trip_through_json() {
        let job = business_cards();
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(back, job);
        assert!(preset("business cards 10-up 12x18").is_some());
        assert!(preset("no such preset").is_none());
    }

    #[test]
    fn impose_writes_a_new_pdf_and_leaves_the_source() {
        let doc = sample(2);
        let root = doc.trailer().get(b"Root").cloned();
        let job = letter_two_up();
        let imposed = impose(&doc, &[0, 1], &job, &Slug { filename: "cards.pdf".into(), date: "2026-10-26".into() }).unwrap();
        assert_eq!(imposed.sheets, 1, "two letter pages, 2-up, one sheet");
        assert_eq!(doc.trailer().get(b"Root"), root.as_ref());
        let out = Document::open(Arc::new(imposed.bytes)).unwrap();
        assert_eq!(pdfcraft_model::pages(&out).len(), 1, "two letter pages, 2-up, one sheet");
        let media = pdfcraft_model::pages(&out)[0].dict.get(b"MediaBox").and_then(|m| m.as_array()).map(|a| a.len());
        assert_eq!(media, Some(4));
    }

    fn sample(n: usize) -> Document {
        let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
        objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes());
        for i in 0..n {
            objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R >>", 4 + 2 * i).into_bytes());
            let body = format!("0.{i} 0 0 rg 36 36 540 720 re f");
            objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes());
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let x = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
        Document::open(Arc::new(out)).unwrap()
    }
}
