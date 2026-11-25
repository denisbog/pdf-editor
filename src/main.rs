//! PDF Editor — edit Typst source and read the typeset pages beside it.
//!
//! The page view is the document itself: `typst` lays it out, the pages are rasterised for the
//! screen, and the PDF is exported from the same compiled document. The chrome (tab strip,
//! mode bar, rails, pasteboard, palette) is PdfCraft's, as in `../pdf-translate`.
//!
//! Debug helpers (no UI):
//!   pdf-editor --compile <file.typ> [out.pdf]   compile and export
//!   pdf-editor --check   <file.typ>             compile and list diagnostics
//!   pdf-editor --pages   <file.typ>             list pages, sizes and click-to-source hits

mod app;
mod editor;
mod engine;
mod icons;
mod theme;
mod vim;
mod widgets;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--compile") => return compile_cli(&args),
        Some("--check") => return check_cli(&args),
        Some("--pages") => return pages_cli(&args),
        Some("--help" | "-h") => {
            println!(
                "pdf-editor [file.typ …]\n\n  --compile <file.typ> [out.pdf]   compile and export a PDF\n  --check   <file.typ>             compile and list diagnostics\n  --pages   <file.typ>             list pages, sizes and click-to-source hits"
            );
            return Ok(());
        }
        _ => {}
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 920.0]).with_min_inner_size([880.0, 520.0]).with_title("PDF Editor"),
        ..Default::default()
    };
    eframe::run_native("PDF Editor", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

fn read_source(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| {
        eprintln!("cannot read {path}: {error}");
        std::process::exit(1);
    })
}

fn compiled(path: &str, text: &str) -> engine::Outcome {
    engine::warm_fonts(None);
    engine::compile(std::path::Path::new(path), text, engine::fonts())
}

fn compile_cli(args: &[String]) -> eframe::Result<()> {
    let Some(path) = args.get(2) else {
        eprintln!("usage: pdf-editor --compile <file.typ> [out.pdf]");
        std::process::exit(2);
    };
    let text = read_source(path);
    let outcome = compiled(path, &text);
    for diagnostic in &outcome.diagnostics {
        let at = match &diagnostic.file {
            Some(file) => format!("{file}:{}:{}", diagnostic.line + 1, diagnostic.column + 1),
            None => format!("{}:{}", diagnostic.line + 1, diagnostic.column + 1),
        };
        eprintln!("{at} {:?}: {}", diagnostic.severity, diagnostic.message);
        for hint in &diagnostic.hints {
            eprintln!("    hint: {hint}");
        }
    }
    let Some(document) = outcome.document else {
        eprintln!("compilation failed");
        std::process::exit(1);
    };
    println!("compiled {} pages in {} ms", document.pages().len(), outcome.millis);
    let out = args.get(3).map(std::path::PathBuf::from).unwrap_or_else(|| std::path::Path::new(path).with_extension("pdf"));
    match typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default()) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(&out, &bytes) {
                eprintln!("cannot write {}: {error}", out.display());
                std::process::exit(1);
            }
            println!("wrote {} ({} bytes)", out.display(), bytes.len());
        }
        Err(errors) => {
            for error in errors {
                eprintln!("export: {}", error.message);
            }
            std::process::exit(1);
        }
    }
    Ok(())
}

fn check_cli(args: &[String]) -> eframe::Result<()> {
    let Some(path) = args.get(2) else {
        eprintln!("usage: pdf-editor --check <file.typ>");
        std::process::exit(2);
    };
    let text = read_source(path);
    let outcome = compiled(path, &text);
    println!("{} page(s), {} error(s), {} warning(s), {} ms", outcome.pages.len(), outcome.errors(), outcome.warnings(), outcome.millis);
    for diagnostic in &outcome.diagnostics {
        let at = match &diagnostic.file {
            Some(file) => format!("{file}:{}:{}", diagnostic.line + 1, diagnostic.column + 1),
            None => format!("{}:{}", diagnostic.line + 1, diagnostic.column + 1),
        };
        println!("{at} {:?}: {}", diagnostic.severity, diagnostic.message);
        for hint in &diagnostic.hints {
            println!("    hint: {hint}");
        }
    }
    if outcome.errors() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn pages_cli(args: &[String]) -> eframe::Result<()> {
    let Some(path) = args.get(2) else {
        eprintln!("usage: pdf-editor --pages <file.typ>");
        std::process::exit(2);
    };
    let text = read_source(path);
    let outcome = compiled(path, &text);
    for (index, page) in outcome.pages.iter().enumerate() {
        println!("page {} — {:.1} × {:.1} pt, {} text run(s) indexed", index + 1, page.width, page.height, page.hits.len());
        for hit in page.hits.iter().take(6) {
            println!("    line {:>3}  x {:.1}–{:.1}  y {:.1}–{:.1}", hit.line + 1, hit.x0, hit.x1, hit.y0, hit.y1);
        }
    }
    for diagnostic in &outcome.diagnostics {
        println!("{:?}: {}", diagnostic.severity, diagnostic.message);
    }
    Ok(())
}
