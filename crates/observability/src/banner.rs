// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The Firefly startup banner.
//!
//! The Go port embeds `banner.txt` with `go:embed` and renders it through
//! `text/template`; this port embeds the same file with [`include_str!`]
//! and renders it through simple `{placeholder}` substitution. Editing
//! `crates/observability/banner.txt` re-flows through every service that
//! calls [`print_banner`], keeping the on-disk template and the runtime
//! banner in lock-step.
//!
//! Two layers of API live here:
//!
//! * The original [`print_banner`] / [`render_banner`] / [`banner_string`]
//!   helpers render the embedded template plainly (no ANSI) — the shape the
//!   Go port exposes and that captured/log output expects.
//! * A richer [`BannerPrinter`] mirrors the pyfly
//!   (`pyfly.core.banner.BannerPrinter`) and Java
//!   (`firefly-application` `banner.txt`) surface: a [`BannerMode`]
//!   (Text / Minimal / Off), active profiles, an optional Swagger-UI URL,
//!   a custom banner file location, and TTY-aware ANSI colouring (red art,
//!   green foundation/license lines, bold accents) with a plain path for
//!   non-terminal writers.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

/// The canonical Firefly banner template — same ASCII art as the Java
/// `firefly-application` module's `banner.txt`, the .NET per-starter
/// embedded `banner.txt`, and the Go module's `banner.txt`. Placeholders:
/// `{version}`, `{starter}`, `{app}`, `{rust_version}`, `{profiles}`.
const BANNER_TEMPLATE: &str = include_str!("../banner.txt");

/// The `rustc` version that compiled this crate (e.g. `"1.87.0"`),
/// captured by the build script — the Rust analog of the Go port reading
/// `runtime.Version()` and stripping the leading `"go"`.
pub const RUSTC_VERSION: &str = env!("FIREFLY_RUSTC_VERSION");

// ---- ANSI escape codes (mirror Java's AnsiColor / AnsiStyle) -------------

const ANSI_RESET: &str = "\u{1b}[0m";
const ANSI_RED: &str = "\u{1b}[31m";
const ANSI_GREEN: &str = "\u{1b}[32m";
const ANSI_BOLD: &str = "\u{1b}[1m";

/// The typed model rendered into the banner template. The template knows
/// about `{version}`, `{starter}`, `{app}`, `{rust_version}`, and
/// `{profiles}`; service authors typically only set `starter` and `app`
/// via the [`print_banner`] convenience wrapper.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BannerData {
    /// Framework version; empty falls back to [`firefly_kernel::VERSION`].
    pub version: String,
    /// Active starter (`"starter-core"`, `"starter-application"`, …).
    pub starter: String,
    /// Application name from the starter config.
    pub app: String,
    /// Compiler version; empty falls back to [`RUSTC_VERSION`].
    pub rust_version: String,
}

/// Renders the embedded banner template against the given starter and
/// application name and writes it to `w`. Used by every Firefly Rust
/// service at startup — equivalent to Spring Boot's `banner-on-start`
/// behaviour, the .NET port's `WriteBanner()`, and the Go port's
/// `PrintBanner`.
///
/// Output is always plain (no ANSI), matching the Go port and keeping
/// captured/log output clean. For TTY-aware colour, active profiles, a
/// Swagger-UI line, custom banner files, or the [`BannerMode`] selection,
/// use [`BannerPrinter`].
pub fn print_banner<W: Write>(w: &mut W, starter: &str, app: &str) -> io::Result<()> {
    render_banner(
        w,
        BannerData {
            starter: starter.to_string(),
            app: app.to_string(),
            ..BannerData::default()
        },
    )
}

/// The typed variant of [`print_banner`] — pass a fully populated
/// [`BannerData`] to override every field. Empty `version` /
/// `rust_version` fields fall back to the canonical defaults
/// ([`firefly_kernel::VERSION`] and [`RUSTC_VERSION`]). Output is plain
/// (no ANSI).
pub fn render_banner<W: Write>(w: &mut W, mut data: BannerData) -> io::Result<()> {
    if data.version.is_empty() {
        data.version = firefly_kernel::VERSION.to_string();
    }
    if data.rust_version.is_empty() {
        data.rust_version = RUSTC_VERSION.to_string();
    }
    let rendered = BANNER_TEMPLATE
        .replace("{version}", &data.version)
        .replace("{starter}", &data.starter)
        .replace("{app}", &data.app)
        .replace("{rust_version}", &data.rust_version)
        // The simple path has no profile list; mirror Java's `:default`.
        .replace("{profiles}", "default");
    w.write_all(rendered.as_bytes())
}

/// Convenience: renders the banner for `starter` / `app` into a `String`.
///
/// ```
/// let banner = firefly_observability::banner_string("starter-core", "orders");
/// assert!(banner.contains("Firefly Framework for Rust"));
/// assert!(banner.contains("orders"));
/// ```
pub fn banner_string(starter: &str, app: &str) -> String {
    let mut buf = Vec::new();
    let _ = print_banner(&mut buf, starter, app);
    String::from_utf8(buf).unwrap_or_default()
}

// ---- BannerMode + BannerPrinter (pyfly / Java parity) --------------------

/// How much of the banner to render — mirrors pyfly's
/// `pyfly.core.banner.BannerMode` and Spring Boot's `Banner.Mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BannerMode {
    /// Full ASCII art + canonical metadata block (the default).
    #[default]
    Text,
    /// A single tagline line:
    /// `:: Firefly Framework for Rust :: (v..) app=.. profiles=..`.
    Minimal,
    /// Render nothing.
    Off,
}

impl BannerMode {
    /// Parses a mode name case-insensitively (`text` / `minimal` / `off`).
    /// Any unrecognised value falls back to [`BannerMode::Text`], matching
    /// pyfly's `from_config` and Spring Boot's lenient binding.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_uppercase().as_str() {
            "MINIMAL" => Self::Minimal,
            "OFF" => Self::Off,
            _ => Self::Text,
        }
    }
}

/// A minimal, dependency-free view of the few config keys the banner cares
/// about — `firefly.banner.mode` and `firefly.banner.location`.
///
/// Observability stays decoupled from `firefly-config`: rather than depend
/// on a concrete config type, [`BannerPrinter::from_config`] accepts any
/// implementor of this trait. The host wires its own config behind it
/// (mirroring how pyfly passes the resolved `mode`/`location` in). A blanket
/// impl is provided for `Fn(&str) -> Option<String>` so a closure works too.
pub trait BannerConfig {
    /// Returns the configured value for `key`, or `None` when unset.
    fn get(&self, key: &str) -> Option<String>;
}

impl<F> BannerConfig for F
where
    F: Fn(&str) -> Option<String>,
{
    fn get(&self, key: &str) -> Option<String> {
        self(key)
    }
}

/// Renders the Firefly startup banner with mode selection, active profiles,
/// an optional Swagger-UI URL, custom banner files, and TTY-aware colour.
///
/// This is the rich analog of pyfly's `BannerPrinter` and the Java
/// `firefly-application` `banner.txt`. The plain [`print_banner`] /
/// [`render_banner`] helpers remain the simple, colourless entry points.
#[derive(Debug, Clone)]
pub struct BannerPrinter {
    mode: BannerMode,
    version: String,
    starter: String,
    app: String,
    app_version: String,
    rust_version: String,
    profiles: Vec<String>,
    swagger_path: Option<String>,
    custom_location: Option<String>,
    /// `None` = auto-detect from the writer; `Some(b)` = force colour on/off.
    force_color: Option<bool>,
}

impl Default for BannerPrinter {
    fn default() -> Self {
        Self {
            mode: BannerMode::Text,
            version: firefly_kernel::VERSION.to_string(),
            starter: String::new(),
            app: String::new(),
            app_version: String::new(),
            rust_version: RUSTC_VERSION.to_string(),
            profiles: Vec::new(),
            swagger_path: None,
            custom_location: None,
            force_color: None,
        }
    }
}

impl BannerPrinter {
    /// A fresh printer in [`BannerMode::Text`] with framework and toolchain
    /// versions pre-filled from [`firefly_kernel::VERSION`] and
    /// [`RUSTC_VERSION`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a printer from a [`BannerConfig`], reading `firefly.banner.mode`
    /// and `firefly.banner.location` — the Rust analog of pyfly's
    /// `BannerPrinter.from_config` reading `pyfly.banner.{mode,location}`.
    ///
    /// An unknown or missing mode falls back to [`BannerMode::Text`]; an
    /// empty location is treated as "no custom banner".
    pub fn from_config<C: BannerConfig>(config: &C) -> Self {
        let mode = config
            .get("firefly.banner.mode")
            .map(|m| BannerMode::from_name(&m))
            .unwrap_or(BannerMode::Text);
        let location = config
            .get("firefly.banner.location")
            .filter(|l| !l.trim().is_empty());
        Self {
            mode,
            custom_location: location,
            ..Self::default()
        }
    }

    /// Sets the banner mode.
    pub fn with_mode(mut self, mode: BannerMode) -> Self {
        self.mode = mode;
        self
    }

    /// Overrides the framework version (default [`firefly_kernel::VERSION`]).
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Sets the active starter name.
    pub fn with_starter(mut self, starter: impl Into<String>) -> Self {
        self.starter = starter.into();
        self
    }

    /// Sets the application name.
    pub fn with_app(mut self, app: impl Into<String>) -> Self {
        self.app = app.into();
        self
    }

    /// Sets the application's own version (rendered in metadata).
    pub fn with_app_version(mut self, app_version: impl Into<String>) -> Self {
        self.app_version = app_version.into();
        self
    }

    /// Overrides the runtime (Rust toolchain) version (default
    /// [`RUSTC_VERSION`]).
    pub fn with_rust_version(mut self, rust_version: impl Into<String>) -> Self {
        self.rust_version = rust_version.into();
        self
    }

    /// Sets the active profiles, rendered as `Active Profiles: a, b`.
    pub fn with_profiles<I, S>(mut self, profiles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.profiles = profiles.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the Swagger-UI / OpenAPI URL, adding an `Application SwaggerUI:`
    /// line to the metadata block (omitted when unset).
    pub fn with_swagger(mut self, url: impl Into<String>) -> Self {
        self.swagger_path = Some(url.into());
        self
    }

    /// Points the printer at a custom banner file. A missing or unreadable
    /// file falls back to the embedded template (pyfly behaviour).
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        let location = location.into();
        self.custom_location = if location.trim().is_empty() {
            None
        } else {
            Some(location)
        };
        self
    }

    /// Forces ANSI colour on (`true`) or off (`false`), overriding TTY
    /// auto-detection. The plain [`Self::render`] string is always colourless.
    pub fn with_color(mut self, enabled: bool) -> Self {
        self.force_color = Some(enabled);
        self
    }

    /// Renders the banner to a plain `String` (never coloured) — the form
    /// reported in tests and logs, and the analog of pyfly's `render()`.
    pub fn render(&self) -> String {
        self.render_inner(false)
    }

    /// Renders the banner to an arbitrary writer (file, pipe, in-memory
    /// buffer). Output is plain unless colour was forced on via
    /// [`Self::with_color`] — keeping captured/log output clean by default.
    /// Use [`Self::print`] to auto-detect a terminal on stdout.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let color = self.force_color.unwrap_or(false);
        self.write_with_color(w, color)
    }

    /// Prints the banner to stdout, colourising when stdout is a terminal
    /// (or when colour was forced on/off via [`Self::with_color`]).
    pub fn print(&self) -> io::Result<()> {
        let mut out = io::stdout();
        let color = self
            .force_color
            .unwrap_or_else(|| io::stdout().is_terminal());
        self.write_with_color(&mut out, color)
    }

    fn write_with_color<W: Write>(&self, w: &mut W, color: bool) -> io::Result<()> {
        let rendered = self.render_inner(color);
        if rendered.is_empty() {
            return Ok(());
        }
        writeln!(w, "{rendered}")
    }

    // ---- internals -------------------------------------------------------

    fn version(&self) -> String {
        if self.version.is_empty() {
            firefly_kernel::VERSION.to_string()
        } else {
            self.version.clone()
        }
    }

    fn rust_version(&self) -> String {
        if self.rust_version.is_empty() {
            RUSTC_VERSION.to_string()
        } else {
            self.rust_version.clone()
        }
    }

    fn profiles_label(&self) -> String {
        if self.profiles.is_empty() {
            "default".to_string()
        } else {
            self.profiles.join(", ")
        }
    }

    fn load_template(&self) -> String {
        if let Some(loc) = &self.custom_location {
            if let Ok(text) = std::fs::read_to_string(Path::new(loc)) {
                return text;
            }
        }
        BANNER_TEMPLATE.to_string()
    }

    fn render_inner(&self, color: bool) -> String {
        match self.mode {
            BannerMode::Off => String::new(),
            BannerMode::Minimal => self.render_minimal(color),
            BannerMode::Text => self.render_text(color),
        }
    }

    fn render_minimal(&self, color: bool) -> String {
        let app = if self.app.is_empty() {
            "unknown".to_string()
        } else {
            self.app.clone()
        };
        let line = format!(
            ":: Firefly Framework for Rust :: (v{}) app={} profiles={}",
            self.version(),
            app,
            self.profiles_label()
        );
        if color {
            format!("{ANSI_BOLD}{line}{ANSI_RESET}")
        } else {
            line
        }
    }

    fn render_text(&self, color: bool) -> String {
        let template = self.load_template();
        let mut rendered = template
            .replace("{version}", &self.version())
            .replace("{starter}", &self.starter)
            .replace("{app}", &self.app)
            .replace("{rust_version}", &self.rust_version())
            .replace("{profiles}", &self.profiles_label())
            .replace("${firefly.version}", &self.version())
            .replace("${rust.version}", &self.rust_version())
            .replace("${app.name}", &self.app)
            .replace("${app.version}", &self.app_version)
            .replace("${profiles.active}", &self.profiles_label());
        rendered = rendered.trim_end_matches('\n').to_string();

        // Optional application-version line, mirroring Java's metadata block.
        if !self.app_version.is_empty() {
            rendered.push_str(&format!("\nApplication Version: {}", self.app_version));
        }
        // Optional Swagger-UI line — only when an OpenAPI path was supplied.
        if let Some(path) = &self.swagger_path {
            rendered.push_str(&format!("\nApplication SwaggerUI: {path}"));
        }

        if color {
            colourise(&rendered)
        } else {
            rendered
        }
    }
}

/// Wraps the ASCII-art block in red, the foundation/license lines in green,
/// and the tagline in bold — mirroring Java's `${AnsiColor.RED}` art and
/// `${AnsiColor.GREEN}` foundation/license markers.
fn colourise(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    // The ASCII art is the leading block before the first blank line; we are
    // inside it until that blank line ends the run. Tracking the region this
    // way paints the whole block red — including the top row, whose glyphs
    // (`  _____.__ …`) match none of the per-line art markers below.
    let mut in_art = true;
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Per-line art markers — kept as a belt-and-braces fallback in case
        // the art ever drifts past a blank line.
        let is_art_line = line.contains('\\') || line.contains("_/") || line.contains("|__|");
        if line.contains("Firefly Software Foundation") || line.contains("Licensed under Apache") {
            in_art = false;
            out.push_str(&format!("{ANSI_GREEN}{ANSI_BOLD}{line}{ANSI_RESET}"));
        } else if line.starts_with(":: Firefly Framework") {
            in_art = false;
            out.push_str(&format!("{ANSI_BOLD}{line}{ANSI_RESET}"));
        } else if line.trim().is_empty() {
            // A blank line closes the leading art block.
            in_art = false;
            out.push_str(line);
        } else if is_art_line || in_art {
            out.push_str(&format!("{ANSI_RED}{line}{ANSI_RESET}"));
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustc_version_captured_and_prefix_free() {
        assert!(!RUSTC_VERSION.is_empty());
        assert!(!RUSTC_VERSION.starts_with("rustc"));
    }

    #[test]
    fn defaults_fall_back_to_kernel_version_and_toolchain() {
        let mut buf = Vec::new();
        render_banner(&mut buf, BannerData::default()).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains(firefly_kernel::VERSION));
        assert!(out.contains(RUSTC_VERSION));
        // No unresolved placeholders survive rendering.
        assert!(!out.contains('{'));
    }

    #[test]
    fn banner_string_matches_print_banner() {
        let mut buf = Vec::new();
        print_banner(&mut buf, "starter-core", "orders").unwrap();
        assert_eq!(
            banner_string("starter-core", "orders"),
            String::from_utf8(buf).unwrap()
        );
    }

    #[test]
    fn legacy_render_has_no_ansi() {
        let s = banner_string("starter-core", "orders");
        assert!(!s.contains('\u{1b}'), "legacy banner must be plain");
    }
}
