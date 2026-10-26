//! Write an imposition plan as a new PDF. The source [`Document`] is only cloned.

use pdfcraft_content::Matrix;
use pdfcraft_cos::{DecodeBudget, Dict, Document, Object, SaveOptions, Stream, write_full};

use crate::{ImposeError, Job, Placed, SheetPlan, Slug, page_geoms, plan};

/// The imposed PDF and how many sheets it holds (fronts and backs each count).
pub struct Imposed {
    pub bytes: Vec<u8>,
    pub sheets: usize,
}

/// Build the imposed PDF. `src` is not modified.
pub fn impose(src: &Document, pages: &[usize], job: &Job, slug: &Slug) -> Result<Imposed, ImposeError> {
    let geoms = page_geoms(src, job.bleed)?;
    let sheets = plan(&geoms, pages, job, slug)?;
    if sheets.is_empty() {
        return Err(ImposeError::NoPages);
    }
    let model_pages = pdfcraft_model::pages(src);
    let mut doc = src.clone();
    let mut forms = std::collections::HashMap::<usize, pdfcraft_cos::ObjRef>::new();
    let pages_root = doc.add(Object::Null);
    let font = helvetica(&mut doc);
    let mut kids = Vec::new();
    for sheet in &sheets {
        kids.push(Object::Ref(write_sheet(&mut doc, &model_pages, sheet, font, pages_root, &mut forms)?));
    }
    let mut tree = Dict::new();
    tree.set(b"Type".to_vec(), Object::name("Pages"));
    tree.set(b"Count".to_vec(), Object::Int(i64::try_from(kids.len()).unwrap_or(i64::MAX)));
    tree.set(b"Kids".to_vec(), Object::Array(kids));
    doc.set(pages_root, Object::Dict(tree));
    let mut cat = Dict::new();
    cat.set(b"Type".to_vec(), Object::name("Catalog"));
    cat.set(b"Pages".to_vec(), Object::Ref(pages_root));
    let layers = src.trailer().get(b"Root").and_then(|r| src.resolve(r).as_dict().and_then(|root| root.get(b"OCProperties").cloned()));
    if let Some(layers) = layers {
        cat.set(b"OCProperties".to_vec(), layers);
    }
    let root = doc.add(Object::Dict(cat));
    doc.trailer_mut().set(b"Root".to_vec(), Object::Ref(root));
    doc.trailer_mut().remove(b"Info");
    if doc.trailer().contains(b"Encrypt") {
        doc.remove_encryption();
    }
    Ok(Imposed { bytes: write_full(&doc, &SaveOptions::default())?, sheets: sheets.len() })
}

fn write_sheet(
    doc: &mut Document,
    model_pages: &[pdfcraft_model::Page],
    sheet: &SheetPlan,
    font: pdfcraft_cos::ObjRef,
    parent: pdfcraft_cos::ObjRef,
    forms: &mut std::collections::HashMap<usize, pdfcraft_cos::ObjRef>,
) -> Result<pdfcraft_cos::ObjRef, ImposeError> {
    let mut body = String::new();
    let mut xo = Dict::new();
    for (i, placed) in sheet.placed.iter().enumerate() {
        let form = match forms.get(&placed.page) {
            Some(f) => *f,
            None => {
                let f = page_form(doc, model_pages, placed.page)?;
                forms.insert(placed.page, f);
                f
            }
        };
        let name = format!("P{i}");
        xo.set(name.clone().into_bytes(), Object::Ref(form));
        let Some(to_display) = model_pages.get(placed.page).and_then(|p| Matrix(p.view_matrix(doc)).invert()) else {
            return Err(ImposeError::Invalid(format!("page {} cannot be placed", placed.page + 1)));
        };
        paint_page(&mut body, &name, placed, to_display);
    }
    if !sheet.lines.is_empty() || !sheet.circles.is_empty() {
        body.push_str("q 0 0 0 RG 0.3 w\n");
        for line in &sheet.lines {
            body.push_str(&format!("{} {} m {} {} l S\n", n(line[0]), n(line[1]), n(line[2]), n(line[3])));
        }
        for (cx, cy, r) in &sheet.circles {
            circle(&mut body, *cx, *cy, *r);
        }
        body.push_str("Q\n");
    }
    if let Some(text) = &sheet.slug {
        body.push_str(&format!("q BT /Slug 7 Tf 1 0 0 1 8 6 Tm ({}) Tj ET Q\n", pdf_text(text)));
    }
    let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), body.as_bytes())));
    let mut fonts = Dict::new();
    fonts.set(b"Slug".to_vec(), Object::Ref(font));
    let mut res = Dict::new();
    res.set(b"XObject".to_vec(), Object::Dict(xo));
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    page.set(b"Parent".to_vec(), Object::Ref(parent));
    page.set(b"MediaBox".to_vec(), rect_num([0.0, 0.0, sheet.width, sheet.height]));
    page.set(b"CropBox".to_vec(), rect_num([0.0, 0.0, sheet.width, sheet.height]));
    if let Some(trim) = union_trim(sheet) {
        page.set(b"TrimBox".to_vec(), rect_num(trim));
    }
    page.set(b"Resources".to_vec(), Object::Dict(res));
    page.set(b"Contents".to_vec(), Object::Ref(contents));
    Ok(doc.add(Object::Dict(page)))
}

fn paint_page(body: &mut String, name: &str, placed: &Placed, to_display: Matrix) {
    let [a, b, c, d, e, f] = placed.matrix;
    let [ua, ub, uc, ud, ue, uf] = to_display.0;
    let [x0, y0, x1, y1] = placed.clip;
    body.push_str(&format!(
        "q {} {} {} {} {} {} cm {} {} {} {} re W n {} {} {} {} {} {} cm /{name} Do Q\n",
        n(a),
        n(b),
        n(c),
        n(d),
        n(e),
        n(f),
        n(x0),
        n(y0),
        n(x1 - x0),
        n(y1 - y0),
        n(ua),
        n(ub),
        n(uc),
        n(ud),
        n(ue),
        n(uf)
    ));
}

fn page_form(doc: &mut Document, pages: &[pdfcraft_model::Page], index: usize) -> Result<pdfcraft_cos::ObjRef, ImposeError> {
    let page = pages.get(index).ok_or_else(|| ImposeError::Invalid(format!("there is no page {}", index + 1)))?;
    let media = rect_from(doc, page.dict.get(b"MediaBox")).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let res = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
    let body = decoded(doc, &page.dict).ok_or_else(|| ImposeError::Invalid(format!("page {} has more content than can be imposed", index + 1)))?;
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("XObject"));
    d.set(b"Subtype".to_vec(), Object::name("Form"));
    d.set(b"BBox".to_vec(), rect_num(media));
    d.set(b"Resources".to_vec(), Object::Dict(res));
    if let Some(g) = page.dict.get(b"Group") {
        d.set(b"Group".to_vec(), g.clone());
    }
    Ok(doc.add(Object::Stream(Stream::flate(d, &body))))
}

fn decoded(doc: &Document, page: &pdfcraft_cos::Dict) -> Option<Vec<u8>> {
    let Some(contents) = page.get(b"Contents") else { return Some(Vec::new()) };
    let mut budget = DecodeBudget::page();
    let mut out = Vec::new();
    for data in budget.decode_contents(doc, contents) {
        match data {
            Ok(data) => {
                out.extend_from_slice(&data);
                out.push(b'\n');
            }
            Err(_) if budget.remaining() == 0 => return None,
            Err(_) => {}
        }
    }
    Some(out)
}

fn helvetica(doc: &mut Document) -> pdfcraft_cos::ObjRef {
    let mut font = Dict::new();
    font.set(b"Type".to_vec(), Object::name("Font"));
    font.set(b"Subtype".to_vec(), Object::name("Type1"));
    font.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    doc.add(Object::Dict(font))
}

fn union_trim(sheet: &SheetPlan) -> Option<[f64; 4]> {
    let mut acc: Option<[f64; 4]> = None;
    for placed in &sheet.placed {
        let t = placed.trim;
        acc = Some(match acc {
            Some(a) => [a[0].min(t[0]), a[1].min(t[1]), a[2].max(t[2]), a[3].max(t[3])],
            None => t,
        });
    }
    acc.filter(|r| r.iter().all(|v| v.is_finite()) && r[2] > r[0] && r[3] > r[1])
}

fn circle(body: &mut String, cx: f64, cy: f64, r: f64) {
    let steps = 24;
    for i in 0..=steps {
        let a = i as f64 * std::f64::consts::TAU / steps as f64;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 {
            body.push_str(&format!("{} {} m ", n(x), n(y)));
        } else {
            body.push_str(&format!("{} {} l ", n(x), n(y)));
        }
    }
    body.push_str("S\n");
}

fn rect_num(r: [f64; 4]) -> Object {
    Object::Array(r.iter().map(|v| Object::Real(*v)).collect())
}

fn rect_from(doc: &Document, o: Option<&Object>) -> Option<[f64; 4]> {
    let o = doc.resolve(o?);
    let v: Vec<f64> = o.as_array()?.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    (v.len() == 4 && v.iter().all(|n| n.is_finite())).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn n(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" { "0".into() } else { s.into() }
}

fn pdf_text(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().take(120) {
        match ch {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            c if c.is_ascii() && !c.is_control() => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}
