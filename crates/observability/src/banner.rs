//! The Firefly startup banner.
//!
//! The Go port embeds `banner.txt` with `go:embed` and renders it through
//! `text/template`; this port embeds the same file with [`include_str!`]
//! and renders it through simple `{placeholder}` substitution. Editing
//! `crates/observability/banner.txt` re-flows through every service that
//! calls [`print_banner`], keeping the on-disk template and the runtime
//! banner in lock-step.

use std::io::{self, Write};

/// The canonical Firefly banner template — same ASCII art as the Java
/// `firefly-common` module's `banner.txt`, the .NET per-starter embedded
/// `banner.txt`, and the Go module's `banner.txt`. Placeholders:
/// `{version}`, `{starter}`, `{app}`, `{rust_version}`.
const BANNER_TEMPLATE: &str = include_str!("../banner.txt");

/// The `rustc` version that compiled this crate (e.g. `"1.87.0"`),
/// captured by the build script — the Rust analog of the Go port reading
/// `runtime.Version()` and stripping the leading `"go"`.
pub const RUSTC_VERSION: &str = env!("FIREFLY_RUSTC_VERSION");

/// The typed model rendered into the banner template. The template knows
/// about `{version}`, `{starter}`, `{app}`, and `{rust_version}`; service
/// authors typically only set `starter` and `app` via the [`print_banner`]
/// convenience wrapper.
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
/// Customise the framework banner globally by editing
/// `crates/observability/banner.txt`; per-service bullets can be added by
/// composing your own writer + template after `print_banner` returns.
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
/// ([`firefly_kernel::VERSION`] and [`RUSTC_VERSION`]).
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
        .replace("{rust_version}", &data.rust_version);
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
}
