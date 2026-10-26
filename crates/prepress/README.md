# pdfcraft-prepress

Press-sheet imposition. [`plan`] lays source pages onto a sheet (n-up, step-and-repeat, or a saddle-stitch booklet, with optional creep and a mirrored back). [`impose`] writes that plan as a **new** PDF. The document it is given is cloned and never modified.

Bleed comes from the page's bleed box or from a margin past the trim. Crop, bleed and registration marks, and a slug line, are drawn in the sheet's margin. The slug's date is supplied by the caller; this crate does not read a clock.

`pdfcraft-engine::Session::impose_pdf` and the `doc_impose` automation tool call into here. The Print dialog's own n-up and booklet (`pdfcraft-print`) are a separate path.
