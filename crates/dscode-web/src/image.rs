//! `GET /api/image?path=<absolute path>` — hand a generated image to the browser.
//!
//! The desktop shell loads generated images through Tauri's `asset:` protocol,
//! which a plain browser does not have; this endpoint is the web equivalent.
//! It is also the only place in this shell where a *caller-supplied string*
//! reaches the filesystem, so the containment check below is the whole point of
//! the module — every other route takes ids or JSON, not paths.
//!
//! Order of operations, and why it is not negotiable:
//!   1. reject non-absolute input,
//!   2. `canonicalize` the request (fails → 404),
//!   3. `canonicalize` the root and require the result to be inside it,
//!   4. only then read the bytes.
//! Steps 2 and 3 both work on canonicalized paths, never on the caller's
//! string: `~/.dscode/images/../../.ssh/id_rsa` is inside `~/.dscode/images`
//! as a *string* and nowhere near it on disk, so a prefix check on the raw
//! input would pass it straight through.
//!
//! Every refusal answers `404` with one constant body. A `403` — or a `404`
//! whose message names the reason — would tell a prober which paths exist.

use std::path::{Component, Path};

use axum::extract::rejection::QueryRejection;
use axum::extract::Query;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tracing::warn;

use dscode_core::config::settings::Config;

use crate::dispatch::{ApiError, ApiResult};

/// Subdirectory of `Config::data_dir()` that `do_image_generate` writes to.
///
/// NOTE: this must stay in step with the writer in `dscode-core`
/// (`Config::data_dir().join("images")`). If that side grows a helper, this
/// should call it rather than repeat the literal.
const IMAGES_DIR: &str = "images";

/// Generated names carry a timestamp and the bytes at a given path never
/// change, so the browser may keep them for as long as it likes. `private`
/// keeps shared and proxy caches out of it — the shell can be bound to a
/// non-loopback address.
const CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// One body for every refusal; see the module docs.
const NOT_FOUND: &str = "image not found";

/// Query parameters for `GET /api/image`.
///
/// `token` (the SSE fallback in `auth.rs`) is not declared here on purpose:
/// `serde_urlencoded` ignores unknown fields, so `?path=…&token=…` still
/// parses, and the middleware reads the token from the raw URI.
#[derive(Deserialize)]
pub struct ImageQuery {
    path: String,
}

/// Why a path was refused. The variant reaches the server log only — the caller
/// gets the same bare 404 whichever one it is, so it cannot be used to probe
/// for paths outside `images/`.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    /// A relative path can only ever be resolved against the server's cwd.
    NotAbsolute,
    /// Missing, or otherwise unresolvable — includes symlink loops.
    Unresolved,
    /// `Config::data_dir()/images` does not exist (no image generated yet).
    RootUnavailable,
    /// Resolved to a real location outside the images directory.
    OutsideRoot,
    /// Inside the images directory, but not a file we serve.
    NotAnImage,
}

impl Refused {
    /// Server-log wording. Never rendered into the HTTP response.
    fn reason(self) -> &'static str {
        match self {
            Refused::NotAbsolute => "path is not absolute",
            Refused::Unresolved => "path does not resolve to an existing file",
            Refused::RootUnavailable => "images directory is unavailable",
            Refused::OutsideRoot => "path is outside the images directory",
            Refused::NotAnImage => "extension is not a served image type",
        }
    }
}

/// A validated image, ready to be turned into a response.
#[derive(Debug)]
struct Loaded {
    mime: &'static str,
    bytes: Vec<u8>,
}

pub async fn image_handler(
    query: Result<Query<ImageQuery>, QueryRejection>,
) -> ApiResult<Response> {
    // A query string that will not parse never reaches the checks below, but it
    // must not fall through to axum's default 400 either: refusals from this
    // endpoint are indistinguishable by design.
    let Ok(Query(query)) = query else {
        return Err(ApiError::not_found(NOT_FOUND));
    };
    let requested = query.path;
    let asked = requested.clone();

    // `canonicalize` and `read` are blocking; keep them off the reactor.
    let loaded = tokio::task::spawn_blocking(move || load_image(&requested))
        .await
        .map_err(|e| {
            warn!(error = %e, "web: /api/image worker panicked");
            ApiError::internal("image worker failed")
        })?
        .map_err(|refused| {
            // Operator gets the path and the reason; the caller gets neither.
            warn!(path = %asked, reason = refused.reason(), "web: /api/image refused");
            ApiError::not_found(NOT_FOUND)
        })?;

    // Header list before the body: `IntoResponse` for a tuple applies the parts
    // *after* the body's own response, so this overrides the
    // `application/octet-stream` that `Vec<u8>` would otherwise send.
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(loaded.mime)),
            (header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL)),
            // The bytes are generated, not authored, but they land on disk in a
            // directory a tool writes to. Refusing to sniff keeps a crafted file
            // from being re-interpreted as HTML in the page's origin.
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        loaded.bytes,
    )
        .into_response())
}

/// Resolve against the real `~/.dscode/images`.
fn load_image(requested: &str) -> Result<Loaded, Refused> {
    let root = Config::data_dir()
        .map(|data_dir| data_dir.join(IMAGES_DIR))
        .map_err(|_| Refused::RootUnavailable)?;
    load_image_in(requested, &root)
}

/// Resolve `requested` and prove it is an image inside `root`.
///
/// `root` is a parameter so the checks can be tested against a scratch
/// directory instead of the developer's real `~/.dscode`.
fn load_image_in(requested: &str, root: &Path) -> Result<Loaded, Refused> {
    let path = Path::new(requested);
    if !path.is_absolute() {
        return Err(Refused::NotAbsolute);
    }

    // The load-bearing call. It strips `.` and `..`, follows symlinks and
    // junctions, expands Windows 8.3 short names and normalises case, so what
    // we compare below — and then open — is the location on disk rather than
    // the one the caller typed. It also fails outright unless every component
    // exists, which is why a missing file is a 404 here and not later.
    let real = std::fs::canonicalize(path).map_err(|_| Refused::Unresolved)?;

    let real_root = std::fs::canonicalize(root).map_err(|_| Refused::RootUnavailable)?;

    // Both sides are canonical, so this is a comparison of real locations.
    if !within(&real_root, &real) {
        return Err(Refused::OutsideRoot);
    }

    // The whitelist is applied to the canonical path — that is the file about
    // to be opened. A link named `a.png` pointing at `id_rsa` looks fine by the
    // requested name and fails here.
    let mime = image_mime(&real).ok_or(Refused::NotAnImage)?;

    let bytes = std::fs::read(&real).map_err(|_| Refused::Unresolved)?;
    Ok(Loaded { mime, bytes })
}

/// Is `candidate` inside `base`, comparing component by component?
///
/// `Path::starts_with` compares bytes. That is correct on Unix, where
/// `/home/x/.dscode` and `/home/x/.DSCODE` are different directories, but wrong
/// on Windows, where `C:\Users\x\.DSCODE\images\a.png` and
/// `C:\Users\x\.dscode\images\a.png` are the same file and a byte compare would
/// 404 the first one.
///
/// So: fold ASCII case on Windows only. Folding on Unix would widen the check
/// to directories that are genuinely not ours, which buys nothing.
///
/// Component-wise also matters for containment itself: a string prefix check
/// would accept `…/images-evil/a.png` because it starts with `…/images`.
fn within(base: &Path, candidate: &Path) -> bool {
    let mut base = base.components();
    let mut candidate = candidate.components();
    loop {
        match (base.next(), candidate.next()) {
            // Base exhausted: everything in it matched.
            (None, _) => return true,
            // Candidate is shorter than base.
            (Some(_), None) => return false,
            (Some(b), Some(c)) if !component_eq(b, c) => return false,
            (Some(_), Some(_)) => {}
        }
    }
}

#[cfg(windows)]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

#[cfg(not(windows))]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a == b
}

/// Extension whitelist. One table drives both the accept/reject decision and
/// the `Content-Type`, so the two cannot drift apart.
///
/// The value is matched case-insensitively (`.PNG` is a real PNG on Windows)
/// but is never echoed from the input — the header value comes from this fixed
/// set, so nothing caller-supplied can reach it.
fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A private scratch tree. Distinct `name` per test so they can run in
    /// parallel, and under the OS temp dir so the real `~/.dscode` is never
    /// read or written.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dscode-web-image-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn load(path: &Path, root: &Path) -> Result<Loaded, Refused> {
        load_image_in(path.to_str().unwrap(), root)
    }

    #[test]
    fn serves_an_image_under_the_images_dir() {
        let home = scratch("serves");
        let root = home.join(".dscode").join("images");
        let image = root.join("gen-20260910-120000.png");
        write(&image, b"\x89PNG\r\n");

        let got = load(&image, &root).unwrap();
        assert_eq!(got.mime, "image/png");
        assert_eq!(got.bytes, b"\x89PNG\r\n");
    }

    #[test]
    fn mime_covers_the_whole_whitelist() {
        let home = scratch("mimes");
        let root = home.join(".dscode").join("images");
        for (name, mime) in [
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.webp", "image/webp"),
            ("a.gif", "image/gif"),
            // Case is folded for the whitelist, not just for the paths.
            ("a.PNG", "image/png"),
            ("a.JpEg", "image/jpeg"),
        ] {
            let path = root.join(name);
            write(&path, b"x");
            assert_eq!(load(&path, &root).unwrap().mime, mime, "{name}");
        }
    }

    /// `~/.dscode/config.toml` is where the plaintext API key lives. It is a
    /// sibling of `images/`, so only the containment check stands in the way.
    #[test]
    fn refuses_a_sibling_of_the_images_dir() {
        let home = scratch("sibling");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let config = home.join(".dscode").join("config.toml");
        write(&config, b"api_key = \"sk-live-SECRET\"");

        assert_eq!(load(&config, &root).unwrap_err(), Refused::OutsideRoot);
    }

    /// Traversal to a file that really exists. The precondition matters: if the
    /// target did not exist, `canonicalize` would fail first and this test would
    /// pass without ever reaching the containment check — green for the wrong
    /// reason.
    #[test]
    fn refuses_a_traversal_to_an_existing_file() {
        let home = scratch("traversal");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let key = home.join(".ssh").join("id_rsa");
        write(&key, b"-----BEGIN OPENSSH PRIVATE KEY-----");

        let attack = root.join("..").join("..").join(".ssh").join("id_rsa");
        assert!(
            std::fs::canonicalize(&attack).is_ok(),
            "fixture must exist, or this test proves nothing"
        );

        assert_eq!(load(&attack, &root).unwrap_err(), Refused::OutsideRoot);
    }

    /// The same trick, aimed at a file inside the data dir but outside
    /// `images/` — the string still starts with the images path.
    #[test]
    fn refuses_a_dotdot_hop_to_the_data_dir() {
        let home = scratch("dotdot");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let config = home.join(".dscode").join("config.toml");
        write(&config, b"api_key = \"sk-live-SECRET\"");

        let attack = root.join("..").join("config.toml");
        assert!(std::fs::canonicalize(&attack).is_ok());

        assert_eq!(load(&attack, &root).unwrap_err(), Refused::OutsideRoot);
    }

    /// A directory one level up that is named like ours. Component-wise
    /// comparison rejects it; a string `starts_with` on the joined path would
    /// have accepted it.
    #[test]
    fn refuses_a_lookalike_images_directory() {
        let home = scratch("lookalike");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();

        for sibling in ["images-evil", "images.bak", "images2"] {
            let fake = home.join(".dscode").join(sibling).join("a.png");
            write(&fake, b"x");
            assert_eq!(
                load(&fake, &root).unwrap_err(),
                Refused::OutsideRoot,
                "{sibling}"
            );
        }

        // Nested the other way: a lookalike *parent*.
        let nested = home.join("evil").join(".dscode").join("images").join("a.png");
        write(&nested, b"x");
        assert_eq!(load(&nested, &root).unwrap_err(), Refused::OutsideRoot);
    }

    #[test]
    fn refuses_a_non_image_inside_the_images_dir() {
        let home = scratch("ext");
        let root = home.join(".dscode").join("images");
        let notes = root.join("notes.txt");
        write(&notes, b"not an image");

        // It is inside the directory, so only the whitelist refuses it.
        assert!(std::fs::canonicalize(&notes).is_ok());
        assert_eq!(load(&notes, &root).unwrap_err(), Refused::NotAnImage);
    }

    #[test]
    fn refuses_a_relative_path() {
        let home = scratch("relative");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();

        assert_eq!(
            load_image_in("images/a.png", &root).unwrap_err(),
            Refused::NotAbsolute
        );
    }

    #[test]
    fn refuses_a_path_that_does_not_exist() {
        let home = scratch("missing");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();

        assert_eq!(
            load(&root.join("gone.png"), &root).unwrap_err(),
            Refused::Unresolved
        );
    }

    /// With no images generated yet the directory is absent; the endpoint must
    /// fail closed rather than treat "no root" as "no restriction".
    #[test]
    fn refuses_everything_when_the_images_dir_is_absent() {
        let home = scratch("noroot");
        let root = home.join(".dscode").join("images");
        let outside = home.join("a.png");
        write(&outside, b"x");

        assert_eq!(load(&outside, &root).unwrap_err(), Refused::RootUnavailable);
    }

    /// Windows callers may spell the directory in any case. Verified here
    /// rather than assumed: the check is what keeps a legitimate request from
    /// being 404'd on a case-insensitive filesystem.
    #[cfg(windows)]
    #[test]
    fn accepts_a_case_variant_of_the_root() {
        let home = scratch("case");
        let root = home.join(".dscode").join("images");
        write(&root.join("a.png"), b"PNG");

        let shouted = home.join(".DSCODE").join("IMAGES").join("A.PNG");
        assert_eq!(load(&shouted, &root).unwrap().mime, "image/png");
    }

    /// Case folding must stay inside the component: a sibling whose name
    /// differs by more than case is still out.
    #[test]
    fn case_folding_does_not_admit_a_different_component() {
        let home = scratch("case-negative");
        let root = home.join(".dscode").join("images");
        std::fs::create_dir_all(&root).unwrap();
        let other = home.join(".dscode").join("imagesX").join("a.png");
        write(&other, b"x");

        assert_eq!(load(&other, &root).unwrap_err(), Refused::OutsideRoot);
    }
}
