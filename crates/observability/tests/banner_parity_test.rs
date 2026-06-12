//! Banner tests ported 1:1 from pyfly's `tests/core/test_banner.py`
//! (`BannerMode`, text/minimal/off rendering, custom file location,
//! `from_config`, metadata content), plus Rust-specific coverage for the
//! canonical Java metadata block, the optional Swagger-UI line, and
//! TTY-aware ANSI colouring (forced on/off + plain non-TTY default).

use std::collections::HashMap;

use firefly_observability::{BannerConfig, BannerMode, BannerPrinter};

// ---------------------------------------------------------------------------
// BannerMode — pyfly `TestBannerMode`
// ---------------------------------------------------------------------------

#[test]
fn banner_mode_from_name_is_case_insensitive() {
    assert_eq!(BannerMode::from_name("text"), BannerMode::Text);
    assert_eq!(BannerMode::from_name("TEXT"), BannerMode::Text);
    assert_eq!(BannerMode::from_name("Minimal"), BannerMode::Minimal);
    assert_eq!(BannerMode::from_name("MINIMAL"), BannerMode::Minimal);
    assert_eq!(BannerMode::from_name("off"), BannerMode::Off);
    assert_eq!(BannerMode::from_name("OFF"), BannerMode::Off);
}

#[test]
fn banner_mode_unknown_falls_back_to_text() {
    assert_eq!(BannerMode::from_name("nonsense"), BannerMode::Text);
    assert_eq!(BannerMode::default(), BannerMode::Text);
}

// ---------------------------------------------------------------------------
// Text mode — pyfly `TestBannerPrinterText`
// ---------------------------------------------------------------------------

#[test]
fn text_mode_contains_ascii_art() {
    let out = BannerPrinter::new().render();
    assert!(out.contains("____"), "missing ASCII art: {out}");
    // The stable second line of the figlet — the same drift guard the
    // observability_test uses.
    assert!(out.contains(r"_/ ____\__|"), "art drifted: {out}");
}

#[test]
fn text_mode_contains_version() {
    let out = BannerPrinter::new().with_version("1.2.3").render();
    assert!(out.contains("v1.2.3"), "missing version: {out}");
}

#[test]
fn text_mode_contains_framework_tagline() {
    let out = BannerPrinter::new().render();
    assert!(
        out.contains(":: Firefly Framework for Rust ::"),
        "missing tagline: {out}"
    );
}

#[test]
fn text_mode_contains_rust_runtime_version() {
    let out = BannerPrinter::new().with_rust_version("1.99.0").render();
    assert!(out.contains("Rust 1.99.0"), "missing runtime: {out}");
}

#[test]
fn text_mode_contains_copyright() {
    let out = BannerPrinter::new().render();
    assert!(
        out.contains("(c) 2026 Firefly Software Foundation"),
        "missing copyright: {out}"
    );
}

#[test]
fn text_mode_contains_license() {
    let out = BannerPrinter::new().render();
    assert!(
        out.contains("Licensed under Apache 2.0"),
        "missing license: {out}"
    );
}

#[test]
fn text_mode_contains_app_and_starter() {
    let out = BannerPrinter::new()
        .with_starter("starter-application")
        .with_app("OrderService")
        .render();
    assert!(
        out.contains("starter-application"),
        "missing starter: {out}"
    );
    assert!(out.contains("OrderService"), "missing app: {out}");
}

#[test]
fn text_mode_renders_active_profiles() {
    let out = BannerPrinter::new()
        .with_profiles(["dev", "local"])
        .render();
    assert!(
        out.contains("Active Profiles: dev, local"),
        "missing profiles: {out}"
    );
}

#[test]
fn text_mode_default_profiles_label() {
    let out = BannerPrinter::new().render();
    assert!(
        out.contains("Active Profiles: default"),
        "missing default profiles: {out}"
    );
}

#[test]
fn text_mode_optional_swagger_line_present_when_set() {
    let out = BannerPrinter::new()
        .with_swagger("http://localhost:8080/swagger-ui.html")
        .render();
    assert!(
        out.contains("Application SwaggerUI: http://localhost:8080/swagger-ui.html"),
        "missing swagger line: {out}"
    );
}

#[test]
fn text_mode_omits_swagger_line_when_unset() {
    let out = BannerPrinter::new().render();
    assert!(!out.contains("SwaggerUI"), "unexpected swagger line: {out}");
}

#[test]
fn text_mode_optional_app_version_line() {
    let with_v = BannerPrinter::new().with_app_version("3.4.5").render();
    assert!(
        with_v.contains("Application Version: 3.4.5"),
        "missing app version: {with_v}"
    );
    let without = BannerPrinter::new().render();
    assert!(
        !without.contains("Application Version:"),
        "unexpected app version line: {without}"
    );
}

// ---------------------------------------------------------------------------
// Minimal mode — pyfly `TestBannerPrinterMinimal`
// ---------------------------------------------------------------------------

#[test]
fn minimal_mode_is_one_line() {
    let out = BannerPrinter::new()
        .with_mode(BannerMode::Minimal)
        .with_app("orders")
        .render();
    assert_eq!(out.lines().count(), 1, "minimal must be one line: {out}");
    assert!(out.starts_with(":: Firefly Framework for Rust ::"));
    assert!(out.contains("app=orders"));
    assert!(out.contains("profiles=default"));
}

#[test]
fn minimal_mode_contains_version_and_profiles() {
    let out = BannerPrinter::new()
        .with_mode(BannerMode::Minimal)
        .with_version("2.0.0")
        .with_app("orders")
        .with_profiles(["prod"])
        .render();
    assert!(out.contains("(v2.0.0)"), "missing version: {out}");
    assert!(out.contains("profiles=prod"), "missing profiles: {out}");
}

// ---------------------------------------------------------------------------
// Off mode — pyfly `TestBannerPrinterOff`
// ---------------------------------------------------------------------------

#[test]
fn off_mode_returns_empty() {
    let out = BannerPrinter::new().with_mode(BannerMode::Off).render();
    assert_eq!(out, "");
}

#[test]
fn off_mode_writes_nothing() {
    let mut buf: Vec<u8> = Vec::new();
    BannerPrinter::new()
        .with_mode(BannerMode::Off)
        .write_to(&mut buf)
        .unwrap();
    assert!(buf.is_empty());
}

// ---------------------------------------------------------------------------
// Custom file location — pyfly `TestBannerPrinterCustomFile`
// ---------------------------------------------------------------------------

#[test]
fn custom_banner_file_is_loaded_with_placeholders() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom-banner.txt");
    std::fs::write(&path, "Hello {version} - {app}\n").unwrap();
    let out = BannerPrinter::new()
        .with_version("0.1.0")
        .with_app("OrderService")
        .with_location(path.to_string_lossy().to_string())
        .render();
    assert!(
        out.contains("Hello 0.1.0 - OrderService"),
        "custom banner not applied: {out}"
    );
}

#[test]
fn custom_banner_file_supports_profiles_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("banner.txt");
    std::fs::write(&path, "Profiles: {profiles}\n").unwrap();
    let out = BannerPrinter::new()
        .with_profiles(["dev", "local"])
        .with_location(path.to_string_lossy().to_string())
        .render();
    assert!(out.contains("Profiles: dev, local"), "{out}");
}

#[test]
fn missing_custom_file_falls_back_to_default() {
    let out = BannerPrinter::new()
        .with_location("/nonexistent/banner.txt")
        .render();
    assert!(
        out.contains(":: Firefly Framework for Rust ::"),
        "fallback failed: {out}"
    );
}

// ---------------------------------------------------------------------------
// from_config — pyfly `TestBannerFromConfig`
// ---------------------------------------------------------------------------

/// A tiny in-memory [`BannerConfig`] for the from_config tests, the analog
/// of pyfly's `Config({...})`.
struct MapConfig(HashMap<String, String>);

impl MapConfig {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

impl BannerConfig for MapConfig {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

#[test]
fn from_config_text_mode() {
    let cfg = MapConfig::new(&[("firefly.banner.mode", "TEXT")]);
    let printer = BannerPrinter::from_config(&cfg);
    assert!(printer
        .render()
        .contains(":: Firefly Framework for Rust ::"));
}

#[test]
fn from_config_off_mode() {
    let cfg = MapConfig::new(&[("firefly.banner.mode", "OFF")]);
    let printer = BannerPrinter::from_config(&cfg);
    assert_eq!(printer.render(), "");
}

#[test]
fn from_config_minimal_mode() {
    let cfg = MapConfig::new(&[("firefly.banner.mode", "MINIMAL")]);
    let printer = BannerPrinter::from_config(&cfg).with_app("svc");
    let out = printer.render();
    assert_eq!(out.lines().count(), 1);
    assert!(out.contains("app=svc"));
}

#[test]
fn from_config_defaults_to_text() {
    let cfg = MapConfig::new(&[]);
    let printer = BannerPrinter::from_config(&cfg);
    assert!(printer
        .render()
        .contains(":: Firefly Framework for Rust ::"));
}

#[test]
fn from_config_reads_custom_location() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf-banner.txt");
    std::fs::write(&path, "From config {version}\n").unwrap();
    let cfg = MapConfig::new(&[
        ("firefly.banner.mode", "TEXT"),
        ("firefly.banner.location", &path.to_string_lossy()),
    ]);
    let out = BannerPrinter::from_config(&cfg)
        .with_version("7.7.7")
        .render();
    assert!(out.contains("From config 7.7.7"), "{out}");
}

#[test]
fn from_config_closure_impl() {
    // The blanket `Fn(&str) -> Option<String>` impl works as a config.
    let cfg = |key: &str| match key {
        "firefly.banner.mode" => Some("MINIMAL".to_string()),
        _ => None,
    };
    let printer = BannerPrinter::from_config(&cfg);
    assert_eq!(printer.render().lines().count(), 1);
}

// ---------------------------------------------------------------------------
// Colour — Rust-specific (Java `${AnsiColor.*}` parity)
// ---------------------------------------------------------------------------

#[test]
fn render_string_is_always_plain() {
    let out = BannerPrinter::new()
        .with_starter("starter-core")
        .with_app("orders")
        .render();
    assert!(
        !out.contains('\u{1b}'),
        "render() must be colourless: {out}"
    );
}

#[test]
fn write_to_non_tty_buffer_is_plain() {
    let mut buf: Vec<u8> = Vec::new();
    BannerPrinter::new()
        .with_app("orders")
        .write_to(&mut buf)
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(
        !out.contains('\u{1b}'),
        "non-TTY writer must be plain: {out}"
    );
    assert!(out.contains(":: Firefly Framework for Rust ::"));
}

#[test]
fn forced_color_emits_ansi_red_and_green() {
    let mut buf: Vec<u8> = Vec::new();
    BannerPrinter::new()
        .with_color(true)
        .with_app("orders")
        .write_to(&mut buf)
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(
        out.contains('\u{1b}'),
        "forced colour must emit ANSI: {out}"
    );
    assert!(out.contains("\u{1b}[31m"), "red art missing: {out}");
    assert!(out.contains("\u{1b}[32m"), "green license missing: {out}");
}

#[test]
fn forced_color_off_overrides_detection() {
    let mut buf: Vec<u8> = Vec::new();
    BannerPrinter::new()
        .with_color(false)
        .with_app("orders")
        .write_to(&mut buf)
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(!out.contains('\u{1b}'), "forced-off must be plain: {out}");
}
