//! Page-view rulers and the guides dragged out of them.
//!
//! Rulers measure displayed distance from the current page's trim box (then its media box, then
//! the rendered page's top-left), in a prepress unit. Guides are session view state: they follow
//! that page's origin and are never written into the PDF. Grid, snap and imposition (n-up,
//! booklets, step-and-repeat, crop and bleed marks) are not part of this module; [`PageFrame`],
//! [`ruler_origin`] and [`RulerUnit`] are the pieces those can share.

use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, pos2, vec2};

use crate::theme::{self, Tokens};

/// Thickness of each ruler band, in screen points.
const THICKNESS: f32 = 28.0;
/// Major ticks are at least about this far apart.
const MIN_MAJOR_PX: f64 = 56.0;
/// Minor ticks are at least about this far apart.
const MIN_MINOR_PX: f64 = 6.0;
/// How many tick marks one ruler will draw. A hostile span strides instead of allocating more.
const MAX_TICKS: i64 = 800;
/// Guides kept for one document. Further drags do not add one.
pub(crate) const MAX_GUIDES: usize = 64;
/// A guide offset is clamped to this many displayed points either side of the origin.
const MAX_OFFSET: f32 = 1.0e6;
/// A guide is grabbed within this many screen points.
const GUIDE_SLOP: f32 = 4.0;

/// The unit the rulers (and only the rulers) measure in. Page sizes keep [`crate::units::Unit`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RulerUnit {
    #[default]
    Inches,
    Millimetres,
    Points,
    Picas,
}

impl RulerUnit {
    pub(crate) const ALL: [RulerUnit; 4] = [RulerUnit::Inches, RulerUnit::Millimetres, RulerUnit::Points, RulerUnit::Picas];

    /// PDF points in one of this unit.
    pub(crate) fn points(self) -> f64 {
        match self {
            Self::Inches => 72.0,
            Self::Millimetres => 72.0 / 25.4,
            Self::Points => 1.0,
            Self::Picas => 12.0,
        }
    }

    /// Stored in settings and reported by the control channel.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Inches => "inches",
            Self::Millimetres => "millimetres",
            Self::Points => "points",
            Self::Picas => "picas",
        }
    }

    /// English label for menus and Preferences. Catalogs fall back to it.
    pub(crate) fn long_name(self) -> &'static str {
        match self {
            Self::Inches => "Inches",
            Self::Millimetres => "Millimetres",
            Self::Points => "Points",
            Self::Picas => "Picas",
        }
    }

    /// The corner of the two rulers, and the imposition dialog's unit label.
    pub(crate) fn short(self) -> &'static str {
        match self {
            Self::Inches => "in",
            Self::Millimetres => "mm",
            Self::Points => "pt",
            Self::Picas => "pc",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inches" | "inch" | "in" => Some(Self::Inches),
            "millimetres" | "millimeters" | "millimetre" | "millimeter" | "mm" => Some(Self::Millimetres),
            "points" | "point" | "pt" => Some(Self::Points),
            "picas" | "pica" | "pc" => Some(Self::Picas),
            _ => None,
        }
    }

    fn base_steps(self) -> &'static [f64] {
        match self {
            Self::Inches => &[1.0 / 64.0, 1.0 / 32.0, 1.0 / 16.0, 1.0 / 8.0, 1.0 / 4.0, 1.0 / 2.0, 1.0, 2.0, 4.0, 6.0, 12.0],
            Self::Millimetres => &[1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0],
            Self::Points => &[1.0, 3.0, 6.0, 12.0, 36.0, 72.0, 144.0, 360.0, 720.0],
            Self::Picas => &[1.0, 3.0, 6.0, 12.0, 24.0, 36.0, 72.0, 144.0],
        }
    }
}

/// Where the rulers sit inside the document view, and the rectangle left for the pages.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bands {
    pub corner: Rect,
    pub horizontal: Rect,
    pub vertical: Rect,
    pub page: Rect,
    pub full: Rect,
}

impl Bands {
    /// Inset `full` by the ruler thickness. A band never takes more than a quarter of the
    /// shorter side. A non-finite or empty rectangle keeps the pages on `full` and draws no band.
    pub(crate) fn split(full: Rect) -> Self {
        let (w, h) = (full.width(), full.height());
        let finite = w.is_finite() && h.is_finite() && full.min.x.is_finite() && full.min.y.is_finite();
        if !finite || w <= 0.0 || h <= 0.0 {
            return Self { corner: Rect::NOTHING, horizontal: Rect::NOTHING, vertical: Rect::NOTHING, page: full, full };
        }
        let t = THICKNESS.min(w * 0.25).min(h * 0.25).max(0.0);
        let corner = Rect::from_min_max(full.min, pos2(full.left() + t, full.top() + t));
        let horizontal = Rect::from_min_max(pos2(full.left() + t, full.top()), pos2(full.right(), full.top() + t));
        let vertical = Rect::from_min_max(pos2(full.left(), full.top() + t), pos2(full.left() + t, full.bottom()));
        let page = Rect::from_min_max(pos2(full.left() + t, full.top() + t), full.max);
        Self { corner, horizontal, vertical, page, full }
    }
}

/// The page rectangle on screen, in the same terms as [`crate::canvas::PageXform`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageFrame {
    pub rect: Rect,
    pub rot: u16,
    /// Page size in view space (points, y down, before the view rotation).
    pub pw: f32,
    pub ph: f32,
}

impl PageFrame {
    /// Screen points per view-space point. Matches [`crate::canvas::PageXform::zoom`].
    pub(crate) fn screen_per_point(self) -> f32 {
        let across = if self.rot % 180 == 90 { self.rect.height() } else { self.rect.width() };
        across / self.pw.max(1.0)
    }

    /// A view-space point (points, y down, before the view rotation) on screen.
    /// Matches [`crate::canvas::PageXform::norm_to_screen`] for the same rectangle.
    pub(crate) fn view_to_screen(self, x: f32, y: f32) -> Pos2 {
        let (u, v) = (x / self.pw.max(1.0), y / self.ph.max(1.0));
        let (a, b) = match self.rot {
            90 => (1.0 - v, u),
            180 => (1.0 - u, 1.0 - v),
            270 => (v, 1.0 - u),
            _ => (u, v),
        };
        pos2(self.rect.left() + a * self.rect.width(), self.rect.top() + b * self.rect.height())
    }
}

/// One page the rulers can measure: its screen frame, page info and boxes.
pub(crate) struct RulerPage {
    pub index: usize,
    pub frame: PageFrame,
    pub info: pdfcraft_render::PageInfo,
    /// Media, crop, bleed, trim, art. `None` when the document could not be read.
    pub boxes: Option<[[f64; 4]; 5]>,
}

/// Screen position of the ruler's zero: the displayed top-left of the trim box, then the media
/// box, then the rendered page. `None` only when the page rectangle itself is unusable.
pub(crate) fn ruler_origin(info: &pdfcraft_render::PageInfo, frame: &PageFrame, boxes: Option<[[f64; 4]; 5]>) -> Option<Pos2> {
    if let Some(boxes) = boxes {
        if let Some(origin) = boxes.get(3).copied().and_then(|rect| box_origin(info, frame, rect)) {
            return Some(origin);
        }
        if let Some(origin) = boxes.first().copied().and_then(|rect| box_origin(info, frame, rect)) {
            return Some(origin);
        }
    }
    let rect = frame.rect;
    (rect.width() > 0.0 && rect.height() > 0.0 && rect.left().is_finite() && rect.top().is_finite()).then_some(rect.left_top())
}

fn usable_box(rect: [f64; 4]) -> bool {
    rect.iter().all(|v| v.is_finite()) && (rect[2] - rect[0]).abs() > 0.5 && (rect[3] - rect[1]).abs() > 0.5
}

/// The displayed top-left of a user-space box: the minimum screen position of its four corners.
fn box_origin(info: &pdfcraft_render::PageInfo, frame: &PageFrame, rect: [f64; 4]) -> Option<Pos2> {
    if !usable_box(rect) {
        return None;
    }
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    for (x, y) in [(rect[0], rect[1]), (rect[2], rect[1]), (rect[0], rect[3]), (rect[2], rect[3])] {
        let [vx, vy] = info.user_to_view(x as f32, y as f32);
        let screen = frame.view_to_screen(vx, vy);
        if !screen.x.is_finite() || !screen.y.is_finite() {
            return None;
        }
        min_x = min_x.min(screen.x);
        min_y = min_y.min(screen.y);
    }
    (min_x.is_finite() && min_y.is_finite()).then_some(pos2(min_x, min_y))
}

/// A tick along one ruler. `at` is a screen coordinate on that ruler's axis.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tick {
    pub at: f32,
    pub major: bool,
    pub label: Option<String>,
}

/// Screen coordinate of `value` (in `unit`) measured from `origin`.
///
/// Kept for the geometry tests and for imposition, which measures in these units.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn unit_to_screen(origin: f32, value: f64, screen_per_pt: f32, unit: RulerUnit) -> f32 {
    origin + (value * unit.points()) as f32 * screen_per_pt
}

/// Displayed points from `origin` to `screen`. `None` when the scale cannot measure.
pub(crate) fn offset_at(origin: f32, screen: f32, screen_per_pt: f32) -> Option<f32> {
    if !origin.is_finite() || !screen.is_finite() || !screen_per_pt.is_finite() || screen_per_pt <= 0.0 {
        return None;
    }
    let points = (screen - origin) / screen_per_pt;
    points.is_finite().then_some(points.clamp(-MAX_OFFSET, MAX_OFFSET))
}

pub(crate) fn screen_at(origin: f32, offset_pt: f32, screen_per_pt: f32) -> f32 {
    origin + offset_pt * screen_per_pt
}

/// Ticks from `start` to `end` (screen coordinates) with zero at `origin`.
pub(crate) fn ticks(origin: f32, start: f32, end: f32, screen_per_pt: f32, unit: RulerUnit) -> Vec<Tick> {
    if ![origin, start, end, screen_per_pt].iter().all(|v| v.is_finite()) || screen_per_pt <= 0.0 || end < start {
        return Vec::new();
    }
    let screen_per_unit = f64::from(screen_per_pt) * unit.points();
    if !screen_per_unit.is_finite() || screen_per_unit <= 0.0 {
        return Vec::new();
    }
    let major = major_step(screen_per_unit, unit);
    let div = minor_divisions(major, screen_per_unit);
    let minor = major / f64::from(div);
    let minor_px = minor * screen_per_unit;
    if !minor.is_finite() || !minor_px.is_finite() || minor_px <= 1.0e-6 {
        return Vec::new();
    }
    let index_at = |screen: f32, up: bool| -> i64 {
        let x = (f64::from(screen) - f64::from(origin)) / minor_px;
        if !x.is_finite() {
            return 0;
        }
        let x = x.clamp(-1.0e12, 1.0e12);
        let rounded = if up { x.ceil() } else { x.floor() };
        rounded as i64
    };
    let first = index_at(start, false);
    let last = index_at(end, true);
    if last < first {
        return Vec::new();
    }
    let span = last.saturating_sub(first);
    let stride = if span > MAX_TICKS { (span / MAX_TICKS).max(1) } else { 1 };
    let mut out = Vec::new();
    let mut i = first;
    let mut drawn = 0i64;
    while i <= last && drawn < MAX_TICKS {
        let at = f64::from(origin) + i as f64 * minor_px;
        if at.is_finite() && at >= f64::from(start) - 0.5 && at <= f64::from(end) + 0.5 {
            let major_tick = i.rem_euclid(i64::from(div)) == 0;
            let value = i as f64 * minor;
            let label = major_tick.then(|| format_tick(unit, value));
            out.push(Tick { at: at as f32, major: major_tick, label });
        }
        drawn += 1;
        let next = i.saturating_add(stride);
        if next <= i {
            break;
        }
        i = next;
    }
    out
}

fn major_step(screen_per_unit: f64, unit: RulerUnit) -> f64 {
    let bases = unit.base_steps();
    let mut steps = Vec::with_capacity(bases.len().saturating_add(6));
    steps.extend_from_slice(bases);
    if let Some(last) = bases.last().copied() {
        let mut value = last * 10.0;
        for _ in 0..6 {
            if !value.is_finite() {
                break;
            }
            steps.push(value);
            value *= 10.0;
        }
    }
    steps.into_iter().find(|step| step * screen_per_unit >= MIN_MAJOR_PX).unwrap_or(1.0e6)
}

fn minor_divisions(major: f64, screen_per_unit: f64) -> u32 {
    for divisions in [10, 8, 5, 4, 2] {
        if major / f64::from(divisions) * screen_per_unit >= MIN_MINOR_PX {
            return divisions;
        }
    }
    1
}

fn format_tick(unit: RulerUnit, value: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    if value.abs() < 1.0e-6 {
        return "0".to_string();
    }
    match unit {
        RulerUnit::Inches => format_inches(value),
        _ => format_number(value),
    }
}

fn format_inches(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let magnitude = value.abs();
    let nearest = (magnitude * 8.0).round() / 8.0;
    if !nearest.is_finite() || (magnitude - nearest).abs() > 1.0e-3 {
        return format!("{value:.2}");
    }
    let eighths_f = nearest * 8.0;
    if !eighths_f.is_finite() || eighths_f.abs() > 1.0e12 {
        return format!("{value:.2}");
    }
    let eighths = eighths_f.round() as i64;
    let (whole, frac) = (eighths / 8, eighths.rem_euclid(8));
    let frac = match frac {
        1 => "⅛",
        2 => "¼",
        3 => "⅜",
        4 => "½",
        5 => "⅝",
        6 => "¾",
        7 => "⅞",
        _ => "",
    };
    if whole == 0 && frac.is_empty() {
        "0".to_string()
    } else if whole == 0 {
        format!("{sign}{frac}")
    } else if frac.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}{frac}")
    }
}

fn format_number(value: f64) -> String {
    let rounded = value.round();
    if rounded.is_finite() && (value - rounded).abs() < 1.0e-3 && rounded.abs() < 1.0e12 {
        format!("{}", rounded as i64)
    } else {
        format!("{value:.1}")
    }
}

/// The number shown beside the cursor mark.
pub(crate) fn format_readout(unit: RulerUnit, points: f32) -> String {
    if !points.is_finite() {
        return String::new();
    }
    let value = f64::from(points) / unit.points();
    if !value.is_finite() {
        return String::new();
    }
    match unit {
        RulerUnit::Inches | RulerUnit::Picas => format!("{value:.2}"),
        RulerUnit::Millimetres | RulerUnit::Points => format!("{value:.1}"),
    }
}

/// A guide dragged out of a ruler. `offset_pt` is displayed points from that page's ruler
/// origin (right and down are positive). It is not a PDF object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Guide {
    pub page: usize,
    /// A vertical line (constant displayed X). Horizontal when false.
    pub vertical: bool,
    pub offset_pt: f32,
}

#[derive(Clone, Copy)]
struct Drag {
    kind: DragKind,
    page: usize,
    vertical: bool,
    offset_pt: f32,
    entered_page: bool,
}

#[derive(Clone, Copy)]
enum DragKind {
    New,
    Move(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragOutcome {
    None,
    Add,
    Update(usize, f32),
    Delete(usize),
}

fn finish_drag(drag: Drag, guides_len: usize, released_on_ruler: bool) -> DragOutcome {
    match drag.kind {
        DragKind::New => {
            let Some(offset) = clamp_offset(drag.offset_pt) else { return DragOutcome::None };
            if drag.entered_page && !released_on_ruler && guides_len < MAX_GUIDES && offset.is_finite() {
                DragOutcome::Add
            } else {
                DragOutcome::None
            }
        }
        DragKind::Move(index) => {
            if released_on_ruler {
                DragOutcome::Delete(index)
            } else if let Some(offset) = clamp_offset(drag.offset_pt) {
                DragOutcome::Update(index, offset)
            } else {
                DragOutcome::None
            }
        }
    }
}

fn clamp_offset(value: f32) -> Option<f32> {
    value.is_finite().then_some(value.clamp(-MAX_OFFSET, MAX_OFFSET))
}

fn apply_drag(guides: &mut Vec<Guide>, drag: Drag, on_ruler: bool) {
    match finish_drag(drag, guides.len(), on_ruler) {
        DragOutcome::None => {}
        DragOutcome::Add => {
            if guides.len() < MAX_GUIDES
                && let Some(offset) = clamp_offset(drag.offset_pt)
            {
                guides.push(Guide { page: drag.page, vertical: drag.vertical, offset_pt: offset });
            }
        }
        DragOutcome::Update(index, offset) => {
            if let Some(guide) = guides.get_mut(index) {
                guide.offset_pt = offset;
            }
        }
        DragOutcome::Delete(index) => {
            if index < guides.len() {
                guides.remove(index);
            }
        }
    }
}

/// The guide within [`GUIDE_SLOP`] of `pointer` on `page`, or none when the pointer is outside
/// the page viewport. The drag hit-testing uses the same distance; this is the pure form the
/// tests lock, ready for snap later.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn nearest_guide(guides: &[Guide], page: usize, origin: Pos2, scale: f32, pointer: Pos2, page_rect: Rect) -> Option<usize> {
    if !page_rect.contains(pointer) || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (index, guide) in guides.iter().enumerate() {
        if guide.page != page || !guide.offset_pt.is_finite() {
            continue;
        }
        let at = if guide.vertical { screen_at(origin.x, guide.offset_pt, scale) } else { screen_at(origin.y, guide.offset_pt, scale) };
        if !at.is_finite() {
            continue;
        }
        let distance = if guide.vertical { (pointer.x - at).abs() } else { (pointer.y - at).abs() };
        if distance <= GUIDE_SLOP && best.is_none_or(|(_, previous)| distance < previous) {
            best = Some((index, distance));
        }
    }
    best.map(|(index, _)| index)
}

#[derive(Clone, Copy)]
struct Measure {
    page: usize,
    origin: Pos2,
    scale: f32,
    rect: Rect,
}

fn measure_of(page: &RulerPage) -> Option<Measure> {
    let origin = ruler_origin(&page.info, &page.frame, page.boxes)?;
    let scale = page.frame.screen_per_point();
    (scale.is_finite() && scale > 0.0 && origin.x.is_finite() && origin.y.is_finite()).then_some(Measure {
        page: page.index,
        origin,
        scale,
        rect: page.frame.rect,
    })
}

fn measure_for(pages: &[RulerPage], index: usize) -> Option<Measure> {
    pages.iter().find(|page| page.index == index).and_then(measure_of)
}

/// Paint the rulers and run guide drags. `guides` belong to `doc_id` and survive only in the view.
pub(crate) fn show(ui: &mut egui::Ui, bands: &Bands, pages: &[RulerPage], current: usize, unit: RulerUnit, doc_id: u64, guides: &mut Vec<Guide>) {
    let tokens = Tokens::get(ui.ctx());
    let painter = ui.painter();
    paint_chrome(painter, bands, &tokens, unit);
    let current_measure = measure_for(pages, current);
    if let Some(measure) = current_measure {
        paint_axis(painter, bands, true, measure.origin.x, measure.scale, unit, &tokens);
        paint_axis(painter, bands, false, measure.origin.y, measure.scale, unit, &tokens);
    }
    paint_guides(ui.painter().with_clip_rect(bands.page), bands, pages, guides);

    let drag_id = egui::Id::unique(("pdfcraft-ruler-drag", doc_id));
    let mut drag: Option<Drag> = ui.data(|data| data.get_temp(drag_id));
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let released = ui.input(|input| input.pointer.primary_released());

    let horizontal = interact_band(ui, bands.horizontal, "ruler-h");
    let vertical = interact_band(ui, bands.vertical, "ruler-v");
    let (move_started, guide_stopped) = guide_interaction(ui, bands, pages, guides, doc_id);

    if escape {
        drag = None;
    } else {
        if drag.is_none() {
            drag = start_drag(guides, current_measure, pointer, bands, horizontal.drag_started(), vertical.drag_started(), move_started);
        }
        if let (Some(active), Some(pos)) = (drag.as_mut(), pointer)
            && let Some(measure) = measure_for(pages, active.page)
        {
            let (origin, screen) = if active.vertical { (measure.origin.x, pos.x) } else { (measure.origin.y, pos.y) };
            if let Some(offset) = offset_at(origin, screen, measure.scale) {
                active.offset_pt = offset;
            }
            if bands.page.contains(pos) {
                active.entered_page = true;
            }
        }
    }

    if let Some(active) = drag {
        paint_preview(ui.painter().with_clip_rect(bands.page), bands, pages, active, tokens.accent);
    }
    if let Some(pos) = pointer.filter(|pos| bands.full.contains(*pos)) {
        paint_cursor(ui, bands, current_measure, pos, unit, &tokens);
    }

    let stopped = !escape && drag.is_some() && (released || horizontal.drag_stopped() || vertical.drag_stopped() || guide_stopped);
    if escape || stopped {
        if !escape && let Some(active) = drag.take() {
            let on_ruler = pointer.is_some_and(|pos| bands.horizontal.contains(pos) || bands.vertical.contains(pos) || bands.corner.contains(pos));
            apply_drag(guides, active, on_ruler);
        }
        ui.data_mut(|data| data.remove::<Drag>(drag_id));
    } else if let Some(active) = drag {
        ui.data_mut(|data| data.insert_temp(drag_id, active));
    }
}

fn interact_band(ui: &mut egui::Ui, rect: Rect, salt: &str) -> egui::Response {
    let response = ui.interact(rect, ui.scope_id().with(salt), Sense::drag());
    response.on_hover_cursor(CursorIcon::Crosshair)
}

fn start_drag(
    guides: &[Guide],
    current: Option<Measure>,
    pointer: Option<Pos2>,
    bands: &Bands,
    from_horizontal: bool,
    from_vertical: bool,
    move_started: Option<usize>,
) -> Option<Drag> {
    let pos = pointer?;
    if from_horizontal || from_vertical {
        if guides.len() >= MAX_GUIDES {
            return None;
        }
        let measure = current?;
        let vertical = from_vertical && !from_horizontal;
        let (origin, screen) = if vertical { (measure.origin.x, pos.x) } else { (measure.origin.y, pos.y) };
        let offset = offset_at(origin, screen, measure.scale)?;
        return Some(Drag { kind: DragKind::New, page: measure.page, vertical, offset_pt: offset, entered_page: bands.page.contains(pos) });
    }
    let index = move_started?;
    let guide = guides.get(index)?;
    Some(Drag { kind: DragKind::Move(index), page: guide.page, vertical: guide.vertical, offset_pt: guide.offset_pt, entered_page: true })
}

fn guide_interaction(ui: &mut egui::Ui, bands: &Bands, pages: &[RulerPage], guides: &[Guide], doc_id: u64) -> (Option<usize>, bool) {
    let mut started = None;
    let mut stopped = false;
    for (index, guide) in guides.iter().enumerate() {
        let Some(measure) = measure_for(pages, guide.page) else { continue };
        let at = if guide.vertical {
            screen_at(measure.origin.x, guide.offset_pt, measure.scale)
        } else {
            screen_at(measure.origin.y, guide.offset_pt, measure.scale)
        };
        if !at.is_finite() {
            continue;
        }
        let strip = if guide.vertical {
            Rect::from_min_max(pos2(at - GUIDE_SLOP, bands.page.top()), pos2(at + GUIDE_SLOP, bands.page.bottom()))
        } else {
            Rect::from_min_max(pos2(bands.page.left(), at - GUIDE_SLOP), pos2(bands.page.right(), at + GUIDE_SLOP))
        };
        let strip = strip.intersect(bands.page);
        if !strip.is_positive() {
            continue;
        }
        let response = ui.interact(strip, ui.scope_id().with(("ruler-guide", doc_id, index)), Sense::drag());
        if response.drag_started() {
            started = Some(index);
        }
        if response.drag_stopped() {
            stopped = true;
        }
        response.on_hover_cursor(CursorIcon::Grab);
    }
    (started, stopped)
}

fn paint_chrome(painter: &egui::Painter, bands: &Bands, tokens: &Tokens, unit: RulerUnit) {
    let stroke = Stroke::new(1.0, tokens.border);
    for rect in [bands.corner, bands.horizontal, bands.vertical] {
        if rect.is_positive() {
            painter.rect_filled(rect, 0.0, tokens.chrome);
        }
    }
    if bands.horizontal.is_positive() {
        painter.line_segment([bands.horizontal.left_bottom(), bands.horizontal.right_bottom()], stroke);
    }
    if bands.vertical.is_positive() {
        painter.line_segment([bands.vertical.right_top(), bands.vertical.right_bottom()], stroke);
    }
    if bands.corner.is_positive() {
        painter.line_segment([bands.corner.left_bottom(), bands.corner.right_bottom()], stroke);
        painter.line_segment([bands.corner.right_top(), bands.corner.right_bottom()], stroke);
        painter.text(bands.corner.center(), Align2::CENTER_CENTER, unit.short(), theme::regular(10.0), tokens.text_muted);
    }
}

fn paint_axis(painter: &egui::Painter, bands: &Bands, horizontal: bool, origin: f32, scale: f32, unit: RulerUnit, tokens: &Tokens) {
    let (start, end) = if horizontal { (bands.horizontal.left(), bands.horizontal.right()) } else { (bands.vertical.top(), bands.vertical.bottom()) };
    if !start.is_finite() || !end.is_finite() || end <= start {
        return;
    }
    for tick in ticks(origin, start, end, scale, unit) {
        let length = if tick.major { 12.0 } else { 6.0 };
        let zero = tick.label.as_deref() == Some("0");
        let color = if zero { tokens.accent } else { tokens.text_muted };
        let stroke = Stroke::new(1.0, color);
        if horizontal {
            let y1 = bands.horizontal.bottom();
            painter.line_segment([pos2(tick.at, y1 - length), pos2(tick.at, y1)], stroke);
            if let Some(label) = tick.label.filter(|_| tick.at > start + 12.0 && tick.at < end - 8.0) {
                painter.text(pos2(tick.at, y1 - length - 1.0), Align2::CENTER_BOTTOM, label, theme::regular(10.0), color);
            }
        } else {
            let x1 = bands.vertical.right();
            painter.line_segment([pos2(x1 - length, tick.at), pos2(x1, tick.at)], stroke);
            if let Some(label) = tick.label.filter(|_| tick.at > start + 8.0 && tick.at < end - 8.0) {
                paint_vertical_label(painter, pos2(x1 - length - 1.0, tick.at), &label, color);
            }
        }
    }
}

fn paint_vertical_label(painter: &egui::Painter, anchor: Pos2, label: &str, color: Color32) {
    let galley = painter.layout_no_wrap(label.to_string(), theme::regular(10.0), color);
    // Rotate counterclockwise about the label's centre so it reads upward, just left of the tick.
    let angle = -std::f32::consts::FRAC_PI_2;
    let half = galley.size() / 2.0;
    let centre = pos2(anchor.x - half.y, anchor.y);
    let (sn, cs) = angle.sin_cos();
    let offset = vec2(half.x * cs - half.y * sn, half.x * sn + half.y * cs);
    painter.add(egui::epaint::TextShape::new(centre - offset, galley, color).with_angle(angle));
}

fn paint_guides(painter: egui::Painter, bands: &Bands, pages: &[RulerPage], guides: &[Guide]) {
    let color = Color32::from_rgb(0x00, 0x9B, 0xB8);
    let stroke = Stroke::new(1.0, color);
    for guide in guides {
        let Some(measure) = measure_for(pages, guide.page) else { continue };
        let Some((a, b)) = guide_segment(guide.vertical, guide.offset_pt, measure, bands) else { continue };
        painter.line_segment([a, b], stroke);
    }
}

fn paint_preview(painter: egui::Painter, bands: &Bands, pages: &[RulerPage], drag: Drag, color: Color32) {
    let Some(measure) = measure_for(pages, drag.page) else { return };
    let Some((a, b)) = guide_segment(drag.vertical, drag.offset_pt, measure, bands) else { return };
    painter.line_segment([a, b], Stroke::new(1.0, color));
}

fn guide_segment(vertical: bool, offset: f32, measure: Measure, bands: &Bands) -> Option<(Pos2, Pos2)> {
    let rect = measure.rect.intersect(bands.page);
    if !rect.is_positive() || !offset.is_finite() {
        return None;
    }
    let at = if vertical { screen_at(measure.origin.x, offset, measure.scale) } else { screen_at(measure.origin.y, offset, measure.scale) };
    if !at.is_finite() {
        return None;
    }
    Some(if vertical { (pos2(at, rect.top()), pos2(at, rect.bottom())) } else { (pos2(rect.left(), at), pos2(rect.right(), at)) })
}

fn paint_cursor(ui: &egui::Ui, bands: &Bands, measure: Option<Measure>, pointer: Pos2, unit: RulerUnit, tokens: &Tokens) {
    let painter = ui.painter();
    let stroke = Stroke::new(1.0, tokens.accent);
    if bands.horizontal.is_positive() && pointer.x >= bands.horizontal.left() && pointer.x <= bands.horizontal.right() {
        painter.line_segment([pos2(pointer.x, bands.horizontal.top()), pos2(pointer.x, bands.horizontal.bottom())], stroke);
        if let Some(measure) = measure
            && let Some(points) = offset_at(measure.origin.x, pointer.x, measure.scale)
        {
            paint_chip(painter, bands.horizontal, pointer.x, true, &format_readout(unit, points), tokens.accent);
        }
    }
    if bands.vertical.is_positive() && pointer.y >= bands.vertical.top() && pointer.y <= bands.vertical.bottom() {
        painter.line_segment([pos2(bands.vertical.left(), pointer.y), pos2(bands.vertical.right(), pointer.y)], stroke);
        if let Some(measure) = measure
            && let Some(points) = offset_at(measure.origin.y, pointer.y, measure.scale)
        {
            paint_chip(painter, bands.vertical, pointer.y, false, &format_readout(unit, points), tokens.accent);
        }
    }
}

fn paint_chip(painter: &egui::Painter, band: Rect, at: f32, horizontal: bool, text: &str, fill: Color32) {
    if text.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(text.to_string(), theme::regular(10.0), Color32::WHITE);
    let pad = vec2(4.0, 1.0);
    let size = galley.size() + pad * 2.0;
    let origin = if horizontal {
        let mut left = at + 6.0;
        if left + size.x > band.right() - 2.0 {
            left = at - size.x - 6.0;
        }
        pos2(left.clamp(band.left(), (band.right() - size.x).max(band.left())), band.center().y - size.y * 0.5)
    } else {
        let mut top = at + 6.0;
        if top + size.y > band.bottom() - 2.0 {
            top = at - size.y - 6.0;
        }
        pos2(band.center().x - size.x * 0.5, top.clamp(band.top(), (band.bottom() - size.y).max(band.top())))
    };
    let rect = Rect::from_min_size(origin, size);
    painter.rect_filled(rect, 2.0, fill);
    painter.galley(origin + pad, galley, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_render::PageInfo;

    const SCALE: f32 = 96.0 / 72.0;

    fn letter_frame() -> (PageInfo, PageFrame) {
        let info = PageInfo { width: 612.0, height: 792.0, label: String::new(), crop: [0.0, 0.0, 612.0, 792.0], rotation: 0 };
        let frame = PageFrame { rect: Rect::from_min_size(pos2(100.0, 80.0), vec2(612.0 * SCALE, 792.0 * SCALE)), rot: 0, pw: 612.0, ph: 792.0 };
        (info, frame)
    }

    #[test]
    fn bands_inset_the_page_and_ignore_a_bad_rect() {
        let full = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let bands = Bands::split(full);
        assert!((bands.page.left() - THICKNESS).abs() < 0.01);
        assert!((bands.page.top() - THICKNESS).abs() < 0.01);
        assert!((bands.page.right() - 800.0).abs() < 0.01);
        assert!((bands.horizontal.height() - THICKNESS).abs() < 0.01);
        let tiny = Bands::split(Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0)));
        assert!(tiny.corner.width() <= 5.0 && tiny.page.width() > 0.0);
        let broken = Rect::from_min_max(pos2(0.0, 0.0), pos2(f32::NAN, 10.0));
        assert!(!Bands::split(broken).corner.is_positive());
    }

    #[test]
    fn inch_ticks_match_zoom_and_origin() {
        let marks = ticks(100.0, 0.0, 400.0, SCALE, RulerUnit::Inches);
        let zero = marks.iter().find(|tick| tick.label.as_deref() == Some("0")).expect("zero");
        let one = marks.iter().find(|tick| tick.label.as_deref() == Some("1")).expect("one inch");
        let minus = marks.iter().find(|tick| tick.label.as_deref() == Some("-1")).expect("minus one");
        assert!((zero.at - 100.0).abs() < 0.5, "{}", zero.at);
        assert!((one.at - 196.0).abs() < 0.5, "{}", one.at);
        assert!((minus.at - 4.0).abs() < 0.5, "{}", minus.at);
        let half = marks.iter().find(|tick| (tick.at - 148.0).abs() < 0.5).expect("half inch");
        assert!(!half.major && half.label.is_none());
        let doubled = ticks(100.0, 0.0, 500.0, SCALE * 2.0, RulerUnit::Inches);
        let one = doubled.iter().find(|tick| tick.label.as_deref() == Some("1")).expect("one inch at 2x");
        assert!((one.at - 292.0).abs() < 0.5, "{}", one.at);
        let shifted = ticks(40.0, 0.0, 200.0, SCALE, RulerUnit::Inches);
        let zero = shifted.iter().find(|tick| tick.label.as_deref() == Some("0")).expect("shifted zero");
        assert!((zero.at - 40.0).abs() < 0.5);
        assert!(ticks(0.0, 0.0, 100.0, f32::NAN, RulerUnit::Inches).is_empty());
        assert!(ticks(0.0, 0.0, 100.0, 0.0, RulerUnit::Inches).is_empty());
        assert!(ticks(0.0, 0.0, 100.0, -1.0, RulerUnit::Inches).is_empty());
        assert!(ticks(0.0, -1.0e7, 1.0e7, 0.001, RulerUnit::Inches).len() as i64 <= MAX_TICKS);
    }

    #[test]
    fn units_share_an_inch_on_screen() {
        let inch = unit_to_screen(100.0, 1.0, SCALE, RulerUnit::Inches);
        assert!((inch - 196.0).abs() < 0.05);
        assert!((unit_to_screen(100.0, 6.0, SCALE, RulerUnit::Picas) - inch).abs() < 0.05);
        assert!((unit_to_screen(100.0, 72.0, SCALE, RulerUnit::Points) - inch).abs() < 0.05);
        assert!((unit_to_screen(100.0, 25.4, SCALE, RulerUnit::Millimetres) - inch).abs() < 0.05);
        assert!((unit_to_screen(100.0, 1.0, SCALE * 2.0, RulerUnit::Inches) - 292.0).abs() < 0.05);
        let points = ticks(100.0, 0.0, 400.0, SCALE, RulerUnit::Points);
        let seventy_two = points.iter().find(|tick| tick.label.as_deref() == Some("72")).expect("72 pt");
        assert!((seventy_two.at - 196.0).abs() < 0.5);
        let picas = ticks(100.0, 0.0, 400.0, SCALE, RulerUnit::Picas);
        let six = picas.iter().find(|tick| tick.label.as_deref() == Some("6")).expect("6 pc");
        assert!((six.at - 196.0).abs() < 0.5);
    }

    #[test]
    fn trim_origin_follows_the_displayed_box() {
        let (info, frame) = letter_frame();
        let crop = [0.0, 0.0, 612.0, 792.0];
        let trim = [36.0, 36.0, 576.0, 756.0];
        let origin = ruler_origin(&info, &frame, Some([crop, crop, crop, trim, crop])).expect("origin");
        assert!((origin.x - 148.0).abs() < 0.05 && (origin.y - 128.0).abs() < 0.05, "{origin:?}");
        let media_only = ruler_origin(&info, &frame, Some([trim, crop, crop, [0.0, 0.0, 0.0, 0.0], crop])).expect("media");
        assert!((media_only.x - 148.0).abs() < 0.05 && (media_only.y - 128.0).abs() < 0.05);
        let nan = [f64::NAN, 0.0, 1.0, 1.0];
        let corner = ruler_origin(&info, &frame, Some([nan, crop, crop, nan, crop])).expect("corner");
        assert!((corner.x - frame.rect.left()).abs() < 0.01 && (corner.y - frame.rect.top()).abs() < 0.01);
        assert_eq!(ruler_origin(&info, &frame, None).expect("page").x, frame.rect.left());

        let rotated = PageInfo { width: 200.0, height: 100.0, label: String::new(), crop: [0.0, 0.0, 100.0, 200.0], rotation: 90 };
        let rotated_frame = PageFrame { rect: Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0)), rot: 0, pw: 200.0, ph: 100.0 };
        let box_ = [0.0, 0.0, 100.0, 200.0];
        let origin = ruler_origin(&rotated, &rotated_frame, Some([box_, box_, box_, box_, box_])).expect("rotated");
        assert!((origin.x - 10.0).abs() < 0.01 && (origin.y - 20.0).abs() < 0.01, "{origin:?}");
        let upright = PageInfo { width: 200.0, height: 100.0, label: String::new(), crop: [0.0, 0.0, 200.0, 100.0], rotation: 0 };
        let turned = PageFrame { rect: Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 200.0)), rot: 90, pw: 200.0, ph: 100.0 };
        let full = [0.0, 0.0, 200.0, 100.0];
        let origin = ruler_origin(&upright, &turned, Some([full, full, full, full, full])).expect("view rotation");
        assert!((origin.x - 10.0).abs() < 0.01 && (origin.y - 20.0).abs() < 0.01, "{origin:?}");
    }

    #[test]
    fn guide_drag_places_moves_and_deletes() {
        let fresh = Drag { kind: DragKind::New, page: 0, vertical: true, offset_pt: 12.0, entered_page: false };
        assert!(matches!(finish_drag(fresh, 0, false), DragOutcome::None));
        let entered = Drag { entered_page: true, ..fresh };
        assert!(matches!(finish_drag(entered, 0, false), DragOutcome::Add));
        assert!(matches!(finish_drag(entered, 0, true), DragOutcome::None));
        assert!(matches!(finish_drag(entered, MAX_GUIDES, false), DragOutcome::None));
        assert!(matches!(finish_drag(Drag { offset_pt: f32::NAN, ..entered }, 0, false), DragOutcome::None));
        let moving = Drag { kind: DragKind::Move(2), entered_page: true, offset_pt: 15.0, ..fresh };
        assert!(matches!(finish_drag(moving, 3, true), DragOutcome::Delete(2)));
        assert!(matches!(finish_drag(moving, 3, false), DragOutcome::Update(2, 15.0)));
        assert_eq!(offset_at(0.0, 1.0e9, 1.0), Some(MAX_OFFSET));
        assert!(offset_at(0.0, 10.0, 0.0).is_none());
        assert!(offset_at(0.0, f32::NAN, 1.0).is_none());
        let page = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let guides = [Guide { page: 0, vertical: true, offset_pt: 10.0 }, Guide { page: 1, vertical: false, offset_pt: 4.0 }];
        assert_eq!(nearest_guide(&guides, 0, pos2(0.0, 0.0), 2.0, pos2(22.0, 40.0), page), Some(0));
        assert!(nearest_guide(&guides, 0, pos2(0.0, 0.0), 2.0, pos2(40.0, 40.0), page).is_none());
        assert!(nearest_guide(&guides, 0, pos2(0.0, 0.0), 2.0, pos2(200.0, 40.0), page).is_none());
    }

    #[test]
    fn readout_uses_the_ruler_unit() {
        assert_eq!(format_readout(RulerUnit::Inches, 72.0), "1.00");
        assert_eq!(format_readout(RulerUnit::Picas, 72.0), "6.00");
        assert_eq!(format_readout(RulerUnit::Points, 72.0), "72.0");
        assert_eq!(format_readout(RulerUnit::Millimetres, 72.0), "25.4");
        assert!(format_readout(RulerUnit::Inches, f32::NAN).is_empty());
    }

    #[test]
    fn ruler_preferences_round_trip() {
        let mut app = crate::PdfCraftApp::new();
        assert!(!app.show_rulers);
        assert_eq!(app.ruler_unit, RulerUnit::Inches);
        assert!(!app.execute("view.rulers"), "the shortcut needs an open document");
        assert!(!app.show_rulers);
        app.show_rulers = true;
        app.ruler_unit = RulerUnit::Picas;
        let saved = app.persist();
        let mut restored = crate::PdfCraftApp::new();
        restored.restore(&saved);
        assert!(restored.show_rulers);
        assert_eq!(restored.ruler_unit, RulerUnit::Picas);
        restored.restore(r#"{"show_rulers":"no","ruler_unit":"furlongs"}"#);
        assert!(restored.show_rulers);
        assert_eq!(restored.ruler_unit, RulerUnit::Picas);
        assert!(app.set_option("rulers", "off").is_ok());
        assert!(app.set_option("ruler-unit", "millimeters").is_ok());
        assert!(!app.show_rulers);
        assert_eq!(app.ruler_unit, RulerUnit::Millimetres);
        assert!(app.set_option("ruler-unit", "furlongs").is_err());
        assert_eq!(app.ruler_unit, RulerUnit::Millimetres);
        assert!(app.set_option("rulers", "on").is_ok());
        assert!(app.show_rulers);
    }
}
