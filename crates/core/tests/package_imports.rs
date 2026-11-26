//! A document that imports `@preview/…` packages compiles from the packages installed on this
//! machine, and its diagnostics name package files by their package path.

use std::path::{Path, PathBuf};

use pdf_editor_core::engine::{compile, fonts};

/// A scratch directory for this test binary.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdf-editor-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Lay a package out in the directory structure of a package root.
fn install(root: &Path, name: &str, version: &str, files: &[(&str, &str)]) {
    let package = root.join("preview").join(name).join(version);
    std::fs::create_dir_all(package.join("src")).unwrap();
    let manifest = format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nentrypoint = \"lib.typ\"\n");
    std::fs::write(package.join("typst.toml"), manifest).unwrap();
    for (path, text) in files {
        std::fs::write(package.join(path), text).unwrap();
    }
}

#[test]
fn an_imported_package_compiles_and_is_named_in_diagnostics() {
    let root = scratch("packages");
    install(
        &root,
        "demo",
        "0.1.0",
        &[
            // The warning has to point into the package, not into the document, so it sits in
            // the package's own entrypoint, which also imports a nested package file.
            ("lib.typ", "#set text(font: \"No Such Font\")\n#import \"src/more.typ\": shout\n#let greet = [hello]\n"),
            ("src/more.typ", "#let shout(body) = upper(body)\n"),
        ],
    );
    std::env::set_var("TYPST_PACKAGE_PATH", &root);

    let document = root.join("document");
    std::fs::create_dir_all(&document).unwrap();
    let main = document.join("main.typ");
    let text = "#import \"@preview/demo:0.1.0\": greet, shout\n= #shout(greet)\n";
    std::fs::write(&main, text).unwrap();

    let outcome = compile(&main, text, fonts());
    let messages: Vec<&str> = outcome.diagnostics.iter().map(|diagnostic| &diagnostic.message[..]).collect();
    assert!(outcome.document.is_some(), "did not compile: {messages:?}");
    assert!(outcome.diagnostics.iter().all(|diagnostic| !diagnostic.is_error()), "errors: {messages:?}");

    let warning = outcome.diagnostics.first().expect("the package's font warning is reported");
    assert_eq!(warning.file.as_deref(), Some("@preview/demo:0.1.0/lib.typ"));
}

#[test]
fn a_package_that_is_not_installed_says_so() {
    let root = scratch("missing-package");
    let document = root.join("document");
    std::fs::create_dir_all(&document).unwrap();
    let main = document.join("main.typ");
    let text = "#import \"@preview/definitely-not-installed:0.0.1\": nothing\n= Hi\n";
    std::fs::write(&main, text).unwrap();

    let outcome = compile(&main, text, fonts());
    let messages: Vec<&str> = outcome.diagnostics.iter().map(|diagnostic| &diagnostic.message[..]).collect();
    assert!(outcome.document.is_none());
    assert!(
        messages.iter().any(|message| message.contains("package not found")),
        "the import should report a missing package, not a missing file: {messages:?}",
    );
}
