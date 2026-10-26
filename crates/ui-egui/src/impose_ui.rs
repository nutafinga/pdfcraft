//! File ▸ Impose. A press-sheet imposition written to a new PDF.
//!
//! The open document is never modified. The preview is the imposed sheet, drawn by the same
//! renderer as the page view, with rulers in the page-view unit.

use std::sync::Arc;

use egui::{Align, Align2, Color32, Layout, Rect, RichText, Stroke, pos2, vec2};

use pdfcraft_engine::prepress::{self, Binding, Bleed, Duplex, Edge, Flow, Layout as SheetLayout, Marks, Orientation, Scale};
use pdfcraft_engine::print::{self, Subset};

use crate::PdfCraftApp;
use crate::theme::{self, Tokens};
use crate::widgets;

const MAX_PRESETS: usize = 32;
const PREVIEW_LONG: f32 = 420.0;

/// One saved shop preset (built-ins live in `pdfcraft-prepress` and are not stored here).
#[derive(Default)]
pub(crate) struct ImposeDraft {
    pub job: prepress::Job,
    /// Page range, as in Print. Empty imposes every page.
    pub pages: String,
    /// Name typed into Save preset.
    pub preset_name: String,
    pub user_presets: Vec<(String, prepress::Job)>,
    pub error: String,
    /// Which imposed sheet the preview shows, 0-based.
    pub sheet: usize,
    pub open_result: bool,
    preview: Option<Preview>,
}

impl ImposeDraft {
    /// Drop the cached sheet so the next frame imposes again.
    pub(crate) fn reset_preview(&mut self) {
        self.sheet = 0;
        self.preview = None;
        self.error.clear();
    }
}

struct Preview {
    key: String,
    bytes: Arc<Vec<u8>>,
    sheets: usize,
    shown: usize,
    texture: Option<egui::TextureHandle>,
    pixels: [usize; 2],
    note: String,
}

impl PdfCraftApp {
    pub fn open_impose(&mut self) {
        self.impose.error.clear();
        self.dialog = Some(crate::Dialog::Impose);
    }

    /// Write the imposition. `false` leaves the dialog open and says why.
    pub(crate) fn impose_now(&mut self) -> bool {
        let Some((_, id)) = self.active_ids() else {
            self.notify_tr("Open a document first");
            return false;
        };
        let Some(doc) = self.session.get(id) else {
            self.notify_tr("Open a document first");
            return false;
        };
        let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
        let count = doc.info.pages.len();
        let name = doc.name.clone();
        let range = self.impose.pages.clone();
        let job = self.impose.job.clone();
        let open_result = self.impose.open_result;
        let pages = match print::select_pages(count, Some(range.as_str()).filter(|r| !r.trim().is_empty()), &labels, Subset::All, false) {
            Ok(pages) => pages,
            Err(e) => {
                self.impose.error = e.to_string();
                self.notify_error(&e);
                return false;
            }
        };
        let (bytes, sheets) = match self.session.impose_pdf(id, &pages, &job, &name) {
            Ok(out) => out,
            Err(e) => {
                self.impose.error = e.to_string();
                self.notify_error(&e);
                return false;
            }
        };
        let stem = name.trim_end_matches(".pdf").trim_end_matches(".PDF");
        let file = format!("{stem} (imposed).pdf");
        self.save_imposed(&file, bytes, sheets, open_result)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_imposed(&mut self, name: &str, bytes: Vec<u8>, sheets: usize, open_result: bool) -> bool {
        let file_name = name.to_string();
        let dialog_name = file_name.clone();
        let note = sheets.to_string();
        let write = move |app: &mut Self, path: std::path::PathBuf| match crate::editing::write_atomically(&path.to_string_lossy(), &bytes) {
            Ok(()) => {
                app.notify_fmt("Saved {sheets} imposed sheets to {path}", &[("sheets", &note), ("path", &path.display().to_string())]);
                if open_result {
                    let opened = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(file_name);
                    if let Err(e) = app.open_bytes(&opened, Some(path.to_string_lossy().into_owned()), bytes.clone()) {
                        app.notify_error(e);
                    }
                }
                true
            }
            Err(e) => {
                app.notify_fmt("Could not save: {e}", &[("e", &e.to_string())]);
                false
            }
        };
        match self.save_override.clone() {
            Some(p) => write(self, p.into()),
            None => {
                let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(dialog_name);
                self.ask_one(crate::pickers::Ask::Save(dialog), None, move |app, path| {
                    write(app, path);
                })
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn save_imposed(&mut self, name: &str, bytes: Vec<u8>, sheets: usize, open_result: bool) -> bool {
        match crate::editing::download(name, &bytes) {
            Ok(()) => {
                self.notify_fmt("Downloaded {name} ({sheets} sheets)", &[("name", name), ("sheets", &sheets.to_string())]);
                if open_result && let Err(e) = self.open_bytes(name, None, bytes) {
                    self.notify_error(e);
                }
                true
            }
            Err(e) => {
                self.notify_fmt("Couldn't download {name}: {e}", &[("name", name), ("e", &e)]);
                false
            }
        }
    }
}

/// Remembered with the rest of the settings. Malformed entries are skipped.
pub(crate) fn persist_job(job: &prepress::Job) -> serde_json::Value {
    serde_json::to_value(job).unwrap_or(serde_json::Value::Null)
}

pub(crate) fn persist_presets(presets: &[(String, prepress::Job)]) -> serde_json::Value {
    serde_json::Value::Array(
        presets
            .iter()
            .take(MAX_PRESETS)
            .filter_map(|(name, job)| Some(serde_json::json!({ "name": name, "job": serde_json::to_value(job).ok()? })))
            .collect(),
    )
}

pub(crate) fn restore_job(value: &serde_json::Value) -> Option<prepress::Job> {
    serde_json::from_value(value.clone()).ok()
}

pub(crate) fn restore_presets(value: &serde_json::Value) -> Vec<(String, prepress::Job)> {
    let Some(list) = value.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for item in list.iter().take(MAX_PRESETS) {
        let Some(name) = item.get("name").and_then(|n| n.as_str()).map(str::trim) else { continue };
        if name.is_empty() || name.chars().count() > 80 {
            continue;
        }
        let Some(job) = item.get("job").and_then(|j| serde_json::from_value(j.clone()).ok()) else { continue };
        out.push((name.to_string(), job));
    }
    out
}

/// Save `job` under `name`. Built-in names are refused. An existing user preset of that name is replaced.
pub(crate) fn store_preset(presets: &mut Vec<(String, prepress::Job)>, name: &str, job: prepress::Job) -> Result<(), &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name the preset");
    }
    if name.chars().count() > 80 {
        return Err("The preset name is too long");
    }
    if prepress::preset(name).is_some() {
        return Err("That name is a built-in preset");
    }
    if let Some(existing) = presets.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        existing.0 = name.to_string();
        existing.1 = job;
        return Ok(());
    }
    if presets.len() >= MAX_PRESETS {
        return Err("32 presets is the limit");
    }
    presets.push((name.to_string(), job));
    Ok(())
}

/// The dialog body. `(save, cancel)`.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> (bool, bool) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Impose")).font(theme::semibold(18.0)));
    ui.label(RichText::new(tl!("Writes a new PDF. The open document is not changed.")).color(t.text_muted));
    ui.add_space(8.0);
    let mut save = false;
    let mut cancel = false;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(500.0);
            egui::ScrollArea::vertical().id_salt("impose-controls").max_height(560.0).show(ui, |ui| {
                controls(ui, app, &t);
            });
        });
        ui.vertical(|ui| {
            ui.set_min_width(360.0);
            preview(ui, app, &t);
        });
    });
    if !app.impose.error.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(crate::i18n::t(&app.impose.error)).color(Color32::from_rgb(0xB4, 0x23, 0x18)));
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add(widgets::checkbox(&mut app.impose.open_result, tl!("Open the imposed PDF")));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Impose…"), true).clicked() {
                save = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        });
    });
    (save, cancel)
}

fn controls(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    preset_row(ui, app);
    ui.add_space(8.0);
    ui.label(RichText::new(tl!("Sheet")).font(theme::semibold(14.0)));
    sheet_row(ui, app);
    ui.horizontal_wrapped(|ui| {
        ui.label(tl!("Units"));
        for unit in crate::rulers::RulerUnit::ALL {
            if ui.add(widgets::radio(app.ruler_unit == unit, tl!(unit.long_name()))).clicked() {
                app.ruler_unit = unit;
            }
        }
    });
    let unit = app.ruler_unit;
    let max_side = app.impose.job.sheet.width.min(app.impose.job.sheet.height);
    size_row(ui, app, unit);
    ui.horizontal(|ui| {
        labeled_points(ui, tl!("Margin"), &mut app.impose.job.sheet.margin, unit, max_side);
        labeled_points(ui, tl!("Column gutter"), &mut app.impose.job.sheet.gutter_x, unit, max_side);
        labeled_points(ui, tl!("Row gutter"), &mut app.impose.job.sheet.gutter_y, unit, max_side);
    });
    ui.horizontal(|ui| {
        labeled_points(ui, tl!("Gripper"), &mut app.impose.job.sheet.gripper, unit, max_side);
        ui.label(tl!("Edge"));
        edge_combo(ui, &mut app.impose.job.sheet.gripper_edge);
    });
    ui.add_space(8.0);
    ui.label(RichText::new(tl!("Layout")).font(theme::semibold(14.0)));
    layout_row(ui, app, unit);
    ui.add_space(8.0);
    ui.label(RichText::new(tl!("Duplex")).font(theme::semibold(14.0)));
    duplex_row(ui, &mut app.impose.job.duplex);
    if matches!(app.impose.job.layout, SheetLayout::Booklet { .. }) {
        ui.label(RichText::new(tl!("A booklet always prints the front and the back.")).color(t.text_muted).small());
    }
    ui.add_space(8.0);
    ui.label(RichText::new(tl!("Bleed")).font(theme::semibold(14.0)));
    bleed_row(ui, app, unit);
    ui.add_space(8.0);
    ui.label(RichText::new(tl!("Marks")).font(theme::semibold(14.0)));
    marks_row(ui, &mut app.impose.job.marks, unit);
    ui.add_space(8.0);
    ui.label(tl!("Pages"));
    ui.add(widgets::line(&mut app.impose.pages).desired_width(f32::INFINITY).hint_text(tl!("Blank imposes every page. Example: 1-3, 6")));
}

fn preset_row(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let user = app.impose.user_presets.clone();
    let mut loaded: Option<(String, prepress::Job)> = None;
    let label = app.impose.preset_name.clone();
    let shown = if label.is_empty() { tl!("Preset").to_string() } else { label };
    egui::ComboBox::from_id_salt("impose-preset")
        .width(280.0)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for (name, job) in prepress::presets() {
                if ui.selectable_label(false, name).clicked() {
                    loaded = Some((name.to_string(), job));
                }
            }
            for (name, job) in &user {
                if ui.selectable_label(false, name).clicked() {
                    loaded = Some((name.clone(), job.clone()));
                }
            }
        })
        .response
        .accessible_name(tl!("Preset"));
    if let Some((name, job)) = loaded {
        app.impose.preset_name = name;
        app.impose.job = job;
        app.impose.sheet = 0;
        app.impose.preview = None;
        app.impose.error.clear();
    }
    ui.horizontal(|ui| {
        ui.add(widgets::line(&mut app.impose.preset_name).desired_width(220.0).hint_text(tl!("Preset name")));
        if widgets::pill_button(ui, tl!("Save preset"), false).clicked() {
            let name = app.impose.preset_name.clone();
            let job = app.impose.job.clone();
            match store_preset(&mut app.impose.user_presets, &name, job) {
                Ok(()) => app.impose.error.clear(),
                Err(e) => app.impose.error = e.to_string(),
            }
        }
        if widgets::pill_button(ui, tl!("Delete"), false).clicked() {
            let name = app.impose.preset_name.clone();
            app.impose.user_presets.retain(|(n, _)| !n.eq_ignore_ascii_case(name.trim()));
        }
    });
}

fn sheet_row(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let current = sheet_name(&app.impose.job.sheet);
    let mut pick: Option<usize> = None;
    egui::ComboBox::from_id_salt("impose-sheet")
        .width(180.0)
        .selected_text(tl!(current))
        .show_ui(ui, |ui| {
            for (index, (name, _, _)) in prepress::SHEETS.iter().enumerate() {
                if ui.selectable_label(current == *name, *name).clicked() {
                    pick = Some(index);
                }
            }
            let _ = ui.selectable_label(current == "Custom", tl!("Custom"));
        })
        .response
        .accessible_name(tl!("Press sheet"));
    if let Some(index) = pick
        && let Some((_, short, long)) = prepress::SHEETS.get(index)
    {
        let which = orientation_of(app.impose.job.sheet.width, app.impose.job.sheet.height);
        let (w, h) = prepress::orient(*short, *long, which);
        app.impose.job.sheet.width = w;
        app.impose.job.sheet.height = h;
    }
    ui.horizontal(|ui| {
        let portrait = app.impose.job.sheet.width <= app.impose.job.sheet.height;
        if ui.add(widgets::radio(portrait, tl!("Portrait"))).clicked() {
            let (w, h) = prepress::orient(app.impose.job.sheet.width, app.impose.job.sheet.height, Orientation::Portrait);
            app.impose.job.sheet.width = w;
            app.impose.job.sheet.height = h;
        }
        if ui.add(widgets::radio(!portrait, tl!("Landscape"))).clicked() {
            let (w, h) = prepress::orient(app.impose.job.sheet.width, app.impose.job.sheet.height, Orientation::Landscape);
            app.impose.job.sheet.width = w;
            app.impose.job.sheet.height = h;
        }
    });
}

fn size_row(ui: &mut egui::Ui, app: &mut PdfCraftApp, unit: crate::rulers::RulerUnit) {
    ui.horizontal(|ui| {
        labeled_points(ui, tl!("Width"), &mut app.impose.job.sheet.width, unit, 72.0 * 200.0);
        labeled_points(ui, tl!("Height"), &mut app.impose.job.sheet.height, unit, 72.0 * 200.0);
    });
}

fn layout_row(ui: &mut egui::Ui, app: &mut PdfCraftApp, unit: crate::rulers::RulerUnit) {
    let grid = matches!(app.impose.job.layout, SheetLayout::Grid { .. });
    ui.horizontal(|ui| {
        if ui.add(widgets::radio(grid, tl!("Grid"))).clicked() && !grid {
            app.impose.job.layout = SheetLayout::Grid { cols: 2, rows: 1, flow: Flow::Sequential, scale: Scale::Auto };
        }
        if ui.add(widgets::radio(!grid, tl!("Booklet"))).clicked() && grid {
            app.impose.job.layout = SheetLayout::Booklet { binding: Binding::Left, creep: 0.0 };
        }
    });
    match &mut app.impose.job.layout {
        SheetLayout::Grid { cols, rows, flow, scale } => {
            ui.horizontal(|ui| {
                ui.label(tl!("Columns"));
                let mut c = *cols as i32;
                ui.add(egui::DragValue::new(&mut c).range(1..=16)).accessible_name(tl!("Columns"));
                ui.label(tl!("Rows"));
                let mut r = *rows as i32;
                ui.add(egui::DragValue::new(&mut r).range(1..=16)).accessible_name(tl!("Rows"));
                *cols = c.clamp(1, 16) as u32;
                *rows = r.clamp(1, 16) as u32;
            });
            ui.horizontal(|ui| {
                if ui.add(widgets::radio(*flow == Flow::Sequential, tl!("Sequential"))).clicked() {
                    *flow = Flow::Sequential;
                }
                if ui.add(widgets::radio(*flow == Flow::Repeat, tl!("Repeat each page"))).clicked() {
                    *flow = Flow::Repeat;
                }
            });
            scale_row(ui, scale);
            ui.horizontal(|ui| {
                ui.add(widgets::checkbox(&mut app.impose.job.center, tl!("Centre in the cell")));
                ui.add(widgets::checkbox(&mut app.impose.job.auto_rotate, tl!("Turn pages to fit")));
            });
        }
        SheetLayout::Booklet { binding, creep } => {
            ui.horizontal(|ui| {
                if ui.add(widgets::radio(*binding == Binding::Left, tl!("Left binding"))).clicked() {
                    *binding = Binding::Left;
                }
                if ui.add(widgets::radio(*binding == Binding::Right, tl!("Right binding"))).clicked() {
                    *binding = Binding::Right;
                }
            });
            labeled_points(ui, tl!("Creep"), creep, unit, 72.0);
            ui.label(RichText::new(tl!("Points toward the spine, per sheet, from the inside out.")).small());
            ui.add(widgets::checkbox(&mut app.impose.job.auto_rotate, tl!("Turn pages to fit")));
        }
    }
}

fn scale_row(ui: &mut egui::Ui, scale: &mut Scale) {
    let mode = match scale {
        Scale::Auto => 0,
        Scale::Actual => 1,
        Scale::Percent(_) => 2,
    };
    ui.horizontal(|ui| {
        if ui.add(widgets::radio(mode == 0, tl!("Auto-fit"))).clicked() {
            *scale = Scale::Auto;
        }
        if ui.add(widgets::radio(mode == 1, tl!("Actual size"))).clicked() {
            *scale = Scale::Actual;
        }
        if ui.add(widgets::radio(mode == 2, tl!("Percent"))).clicked() && mode != 2 {
            *scale = Scale::Percent(100.0);
        }
        if let Scale::Percent(p) = scale {
            ui.add(egui::DragValue::new(p).range(1.0..=1000.0).suffix("%")).accessible_name(tl!("Percent"));
        }
    });
}

fn duplex_row(ui: &mut egui::Ui, duplex: &mut Duplex) {
    ui.horizontal_wrapped(|ui| {
        for (value, label) in [
            (Duplex::Simplex, tl!("Simplex")),
            (Duplex::Sheetwise, tl!("Sheetwise")),
            (Duplex::WorkAndTurn, tl!("Work and turn")),
            (Duplex::WorkAndTumble, tl!("Work and tumble")),
        ] {
            if ui.add(widgets::radio(*duplex == value, label)).clicked() {
                *duplex = value;
            }
        }
    });
}

fn bleed_row(ui: &mut egui::Ui, app: &mut PdfCraftApp, unit: crate::rulers::RulerUnit) {
    let from_box = matches!(app.impose.job.bleed, Bleed::FromBox);
    ui.horizontal(|ui| {
        if ui.add(widgets::radio(from_box, tl!("From the bleed box"))).clicked() {
            app.impose.job.bleed = Bleed::FromBox;
        }
        if ui.add(widgets::radio(!from_box, tl!("Amount"))).clicked() && from_box {
            app.impose.job.bleed = Bleed::Amount(9.0);
        }
        if let Bleed::Amount(amount) = &mut app.impose.job.bleed {
            drag_points(ui, amount, unit, 144.0, tl!("Bleed amount"));
        }
    });
}

fn marks_row(ui: &mut egui::Ui, marks: &mut Marks, unit: crate::rulers::RulerUnit) {
    ui.horizontal_wrapped(|ui| {
        ui.add(widgets::checkbox(&mut marks.crop, tl!("Crop marks")));
        ui.add(widgets::checkbox(&mut marks.bleed, tl!("Bleed marks")));
        ui.add(widgets::checkbox(&mut marks.registration, tl!("Registration marks")));
        ui.add(widgets::checkbox(&mut marks.slug, tl!("Slug line")));
    });
    ui.horizontal(|ui| {
        labeled_points(ui, tl!("Offset"), &mut marks.offset, unit, 72.0);
        labeled_points(ui, tl!("Length"), &mut marks.length, unit, 72.0);
    });
}

fn edge_combo(ui: &mut egui::Ui, edge: &mut Edge) {
    let label = match edge {
        Edge::Left => tl!("Left"),
        Edge::Bottom => tl!("Bottom"),
        Edge::Right => tl!("Right"),
        Edge::Top => tl!("Top"),
    };
    egui::ComboBox::from_id_salt("impose-gripper")
        .selected_text(label)
        .show_ui(ui, |ui| {
            for (value, name) in [(Edge::Bottom, tl!("Bottom")), (Edge::Top, tl!("Top")), (Edge::Left, tl!("Left")), (Edge::Right, tl!("Right"))] {
                if ui.selectable_label(*edge == value, name).clicked() {
                    *edge = value;
                }
            }
        })
        .response
        .accessible_name(tl!("Gripper edge"));
}

fn labeled_points(ui: &mut egui::Ui, label: &str, points: &mut f64, unit: crate::rulers::RulerUnit, max_points: f64) {
    ui.label(label);
    drag_points(ui, points, unit, max_points, label);
    ui.label(RichText::new(unit.short()).small());
}

fn drag_points(ui: &mut egui::Ui, points: &mut f64, unit: crate::rulers::RulerUnit, max_points: f64, name: &str) {
    let per = unit.points();
    if per <= 0.0 {
        return;
    }
    let mut shown = if points.is_finite() { *points / per } else { 0.0 };
    let max_shown = (max_points / per).max(0.0);
    let speed = match unit {
        crate::rulers::RulerUnit::Inches => 0.01,
        crate::rulers::RulerUnit::Millimetres => 0.5,
        crate::rulers::RulerUnit::Points => 1.0,
        crate::rulers::RulerUnit::Picas => 0.1,
    };
    let response = ui.add(egui::DragValue::new(&mut shown).range(0.0..=max_shown).speed(speed).max_decimals(3)).accessible_name(name);
    if response.changed() && shown.is_finite() {
        *points = (shown * per).clamp(0.0, max_points.max(0.0));
    }
}

fn orientation_of(width: f64, height: f64) -> Orientation {
    if width <= height { Orientation::Portrait } else { Orientation::Landscape }
}

fn sheet_name(sheet: &prepress::Sheet) -> &'static str {
    let which = orientation_of(sheet.width, sheet.height);
    for (name, short, long) in prepress::SHEETS {
        let (w, h) = prepress::orient(*short, *long, which);
        if (w - sheet.width).abs() < 0.6 && (h - sheet.height).abs() < 0.6 {
            return name;
        }
    }
    "Custom"
}

fn preview(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    ensure_preview(ui, app);
    let Some(preview) = app.impose.preview.as_ref() else {
        ui.label(RichText::new(tl!("Open a document to preview the sheet.")).color(t.text_muted));
        return;
    };
    if preview.texture.is_none() {
        ui.label(RichText::new(crate::i18n::t(&preview.note)).color(t.text_muted));
        return;
    }
    let sheets = preview.sheets;
    let shown = preview.shown;
    let pixels = preview.pixels;
    let (sheet_w, sheet_h) = prepress::output_size(&app.impose.job);
    if sheets > 1 {
        ui.horizontal(|ui| {
            if ui.add_enabled(shown > 0, egui::Button::new(tl!("Previous"))).clicked() {
                app.impose.sheet = shown.saturating_sub(1);
            }
            ui.label(crate::i18n::fmt(tl!("Sheet {n} of {m}"), &[("n", &(shown + 1).to_string()), ("m", &sheets.to_string())]));
            if ui.add_enabled(shown + 1 < sheets, egui::Button::new(tl!("Next"))).clicked() {
                app.impose.sheet = shown + 1;
            }
        });
    }
    let Some(tex) = app.impose.preview.as_ref().and_then(|p| p.texture.as_ref()) else { return };
    paint_preview(ui, tex, pixels, sheet_w, sheet_h, app.ruler_unit, t);
}

fn ensure_preview(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let Some((_, id)) = app.active_ids() else {
        app.impose.preview = None;
        return;
    };
    let Some(doc) = app.session.get(id) else {
        app.impose.preview = None;
        return;
    };
    let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
    let count = doc.info.pages.len();
    let name = doc.name.clone();
    let generation = doc.edit_generation();
    let range = app.impose.pages.clone();
    let job = app.impose.job.clone();
    let job_key = serde_json::to_string(&job).unwrap_or_default();
    let key = format!("{}:{generation}:{range}:{job_key}", id.0);
    let sheet = app.impose.sheet;
    if app.impose.preview.as_ref().is_some_and(|p| p.key == key && p.shown == sheet && (p.texture.is_some() || !p.note.is_empty())) {
        return;
    }
    let same_bytes = app.impose.preview.as_ref().is_some_and(|p| p.key == key);
    if !same_bytes {
        let pages = match print::select_pages(count, Some(range.as_str()).filter(|r| !r.trim().is_empty()), &labels, Subset::All, false) {
            Ok(pages) => pages,
            Err(e) => {
                app.impose.preview =
                    Some(Preview { key, bytes: Arc::new(Vec::new()), sheets: 0, shown: 0, texture: None, pixels: [0, 0], note: e.to_string() });
                app.impose.error = e.to_string();
                return;
            }
        };
        match app.session.impose_pdf(id, &pages, &job, &name) {
            Ok((bytes, sheets)) => {
                app.impose.error.clear();
                app.impose.sheet = sheet.min(sheets.saturating_sub(1));
                app.impose.preview =
                    Some(Preview { key, bytes: Arc::new(bytes), sheets, shown: usize::MAX, texture: None, pixels: [0, 0], note: String::new() });
            }
            Err(e) => {
                app.impose.error = e.to_string();
                app.impose.preview =
                    Some(Preview { key, bytes: Arc::new(Vec::new()), sheets: 0, shown: 0, texture: None, pixels: [0, 0], note: e.to_string() });
                return;
            }
        }
    }
    render_sheet(ui, app);
}

fn render_sheet(ui: &mut egui::Ui, app: &mut PdfCraftApp) {
    let Some(preview) = app.impose.preview.as_mut() else { return };
    if preview.bytes.is_empty() || preview.sheets == 0 {
        return;
    }
    let sheet = app.impose.sheet.min(preview.sheets.saturating_sub(1));
    let (sheet_w, sheet_h) = prepress::output_size(&app.impose.job);
    let long = sheet_w.max(sheet_h);
    if !(long.is_finite() && long > 1.0) {
        preview.note = "the sheet has no size".to_string();
        return;
    }
    let scale = (PREVIEW_LONG / long as f32).clamp(0.02, 8.0);
    let mut renderer = pdfcraft_render::PageRenderer::new(preview.bytes.clone(), pdfcraft_render::RenderConfig::default());
    let rendered = renderer.render(pdfcraft_render::RenderRequest {
        page: sheet,
        kind: pdfcraft_render::RequestKind::Pixels,
        scale,
        preview: false,
        ..Default::default()
    });
    if let Some(error) = rendered.error {
        preview.note = error;
        preview.texture = None;
        preview.shown = sheet;
        return;
    }
    let w = rendered.width as usize;
    let h = rendered.height as usize;
    let needed = w.checked_mul(h).and_then(|n| n.checked_mul(4));
    if needed.is_none_or(|n| rendered.rgba.len() != n) || w == 0 || h == 0 {
        preview.note = "the preview could not be drawn".to_string();
        preview.texture = None;
        preview.shown = sheet;
        return;
    }
    let image = egui::ColorImage::from_rgba_premultiplied([w, h], &rendered.rgba);
    preview.texture = Some(ui.ctx().load_texture("impose-preview", image, egui::TextureOptions::LINEAR));
    preview.pixels = [w, h];
    preview.shown = sheet;
    preview.note.clear();
}

fn paint_preview(
    ui: &mut egui::Ui,
    tex: &egui::TextureHandle,
    pixels: [usize; 2],
    sheet_w: f64,
    sheet_h: f64,
    unit: crate::rulers::RulerUnit,
    t: &Tokens,
) {
    let thick = 22.0;
    let avail = (ui.available_width() - thick).clamp(80.0, 460.0);
    let aspect = if sheet_w > 1.0 { (sheet_h / sheet_w) as f32 } else { 1.0 };
    let mut img_w = avail;
    let mut img_h = img_w * aspect.max(0.05);
    if img_h > 480.0 {
        img_h = 480.0;
        img_w = img_h / aspect.max(0.05);
    }
    if !img_w.is_finite() || !img_h.is_finite() {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(img_w + thick, img_h + thick), egui::Sense::hover());
    let image = Rect::from_min_size(pos2(rect.left() + thick, rect.top() + thick), vec2(img_w, img_h));
    let painter = ui.painter();
    let corner = Rect::from_min_size(rect.left_top(), vec2(thick, thick));
    let horizontal = Rect::from_min_size(pos2(image.left(), rect.top()), vec2(img_w, thick));
    let vertical = Rect::from_min_size(pos2(rect.left(), image.top()), vec2(thick, img_h));
    for band in [corner, horizontal, vertical] {
        painter.rect_filled(band, 0.0, t.chrome);
    }
    painter.line_segment([horizontal.left_bottom(), horizontal.right_bottom()], Stroke::new(1.0, t.border));
    painter.line_segment([vertical.right_top(), vertical.right_bottom()], Stroke::new(1.0, t.border));
    painter.text(corner.center(), Align2::CENTER_CENTER, unit.short(), theme::regular(9.0), t.text_muted);
    let scale_x = if sheet_w > 1.0 { img_w / sheet_w as f32 } else { 1.0 };
    let scale_y = if sheet_h > 1.0 { img_h / sheet_h as f32 } else { 1.0 };
    paint_ticks(painter, Axis { horizontal: true, origin: image.left(), start: image.left(), end: image.right(), scale: scale_x }, unit, t);
    paint_ticks(painter, Axis { horizontal: false, origin: image.top(), start: image.top(), end: image.bottom(), scale: scale_y }, unit, t);
    painter.rect_filled(image, 0.0, Color32::WHITE);
    painter.image(tex.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    painter.rect_stroke(image, 0.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let _ = pixels;
}

struct Axis {
    horizontal: bool,
    origin: f32,
    start: f32,
    end: f32,
    scale: f32,
}

fn paint_ticks(painter: &egui::Painter, axis: Axis, unit: crate::rulers::RulerUnit, t: &Tokens) {
    let Axis { horizontal, origin, start, end, scale } = axis;
    for tick in crate::rulers::ticks(origin, start, end, scale, unit) {
        let length = if tick.major { 8.0 } else { 4.0 };
        let zero = tick.label.as_deref() == Some("0");
        let color = if zero { t.accent } else { t.text_muted };
        let stroke = Stroke::new(1.0, color);
        if horizontal {
            painter.line_segment([pos2(tick.at, origin - 1.0), pos2(tick.at, origin - length)], stroke);
            if let Some(label) = tick.label.filter(|_| tick.major && tick.at > start + 10.0 && tick.at < end - 8.0) {
                painter.text(pos2(tick.at, origin - length - 1.0), Align2::CENTER_BOTTOM, label, theme::regular(8.0), color);
            }
        } else {
            painter.line_segment([pos2(origin - 1.0, tick.at), pos2(origin - length, tick.at)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_preset_round_trips() {
        let mut app = PdfCraftApp::new();
        let booklet = prepress::preset("Saddle-stitch booklet").unwrap();
        let cards = prepress::preset("Business cards 10-up 12x18").unwrap();
        app.impose.job = booklet.clone();
        store_preset(&mut app.impose.user_presets, "Shop cards", cards.clone()).unwrap();
        assert!(store_preset(&mut app.impose.user_presets, "Business cards 10-up 12x18", cards.clone()).is_err());
        assert!(store_preset(&mut app.impose.user_presets, "  ", cards.clone()).is_err());
        let saved = app.persist();
        let mut restored = PdfCraftApp::new();
        restored.restore(&saved);
        assert_eq!(restored.impose.job, booklet);
        assert_eq!(restored.impose.user_presets, vec![("Shop cards".to_string(), cards)]);
    }
}
