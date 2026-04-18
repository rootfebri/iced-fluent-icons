//! # iced-fluent-icons
//!
//! Proc-macro crate for embedding Fluent UI SVG icons into an [`iced`](https://iced.rs)
//! application with minimal LSP overhead.
//!
//! ## How it works
//!
//! The crate is split into two complementary macros:
//!
//! ### 1. [`declare!`] — stub generation (LSP-friendly, compile-time lean)
//!
//! Call this once in your icons module:
//!
//! ```ignore
//! // src/icons/fluent.rs
//! iced_fluent_icons::declare!();
//! ```
//!
//! For each `Foo.svg` in the `icons/` directory this emits roughly:
//!
//! ```ignore
//! /// ![Foo](https://raw.githubusercontent.com/…/icons/Foo.svg)
//! ///
//! /// `Foo.svg` — annotate the caller with `#[iced_fluent_icons::inventory]`.
//! pub fn foo() -> ::iced::widget::Svg<'static> {
//!     panic!("icon stub …")
//! }
//! ```
//!
//! The stubs are intentionally hollow:
//!
//! - **no `include_bytes!`** → no byte arrays in the LSP's expanded view
//! - **no `impl` blocks** → no trait solving cost per icon
//! - correct **return type** so type inference and IDE completion work
//! - **rustdoc image** so VS Code / JetBrains IDEs show the icon on hover / sig-help
//!
//! ### 2. [`inventory`] — call-site rewriting (compile-time, not LSP-time)
//!
//! Annotate every function (or `impl` block, or `mod`) that calls icon stubs:
//!
//! ```ignore
//! #[iced_fluent_icons::inventory]
//! fn toolbar() -> iced::Element<'_, Message> {
//!     let close = icons::fluent::dismiss_circle_color();
//!     // ↑ rewritten at compile time to:
//!     // {
//!     //     let bytes: &'static [u8] = include_bytes!("/…/DismissCircleColor.svg");
//!     //     let handle = ::iced::widget::svg::Handle::from_memory(bytes);
//!     //     ::iced::widget::svg(handle).width(24).height(24)
//!     // }
//! }
//! ```
//!
//! #### Custom icon size
//!
//! Pass `size`, `width`, or `height` arguments to override the default 24 × 24 px:
//!
//! ```ignore
//! #[iced_fluent_icons::inventory(size = 32)]           // 32 × 32
//! #[iced_fluent_icons::inventory(width = 20, height = 24)]  // custom
//! ```
//!
//! [`inventory`] walks the entire item's AST using [`syn::visit_mut`] and replaces every
//! zero-argument call expression whose **last path segment** is a known icon function name.
//! Icon calls inside widget macros (`column![]`, `row![]`, `stack![]`, etc.) are also
//! rewritten — the macro body is parsed as comma-separated expressions and each one is
//! visited recursively.
//!
//! All of the following call forms are handled identically:
//!
//! ```ignore
//! dismiss_circle_color()                          // after `use icons::fluent::*`
//! fluent::dismiss_circle_color()
//! icons::fluent::dismiss_circle_color()
//! crate::icons::fluent::dismiss_circle_color()
//! ```
//!
//! ## Feature flags
//!
//! Use **exclusion** features to omit entire variant families:
//!
//! | Feature | Excludes |
//! |---|---|
//! | `no-filled` | `*Filled.svg` |
//! | `no-color` | `*Color.svg` |
//! | `no-regular` | `*Regular.svg` |
//! | `no-light` | `*Light.svg` |
//!
//! Use **inclusion** features to keep *only* one variant family:
//!
//! | Feature | Keeps only |
//! |---|---|
//! | `only-filled` | `*Filled.svg` |
//! | `only-color` | `*Color.svg` |
//! | `only-regular` | `*Regular.svg` |
//! | `only-light` | `*Light.svg` |

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::{
  parse::{Parse, ParseStream},
  parse_macro_input,
  punctuated::Punctuated,
  visit_mut::VisitMut,
  Error, LitInt, LitStr, Result, Token,
};

/// Absolute path to the `icons/` directory inside this proc-macro crate.
///
/// Set by `build.rs` via `cargo:rustc-env=FLUENTUI_ICONS_DIR=…` so the value is
/// baked into the proc-macro binary and is always correct regardless of which crate
/// invokes the macros.
const ICONS_DIR: &str = env!("FLUENTUI_ICONS_DIR");

// ── helpers: feature-aware filename filtering ─────────────────────────────────

/// Returns `true` if the icon file should be **included** based on the active
/// Cargo features.  Both exclusion (`no-*`) and inclusion (`only-*`) features are
/// evaluated here so that every place that filters icons uses identical logic.
fn icon_included(filename: &str) -> bool {
  // ── positive-filter: `only-*` features ───────────────────────────────────
  // If any `only-*` feature is active, a file must match at least one of them.
  let has_only =
    cfg!(any(feature = "only-filled", feature = "only-color", feature = "only-regular", feature = "only-light"));

  if has_only {
    let keep = (cfg!(feature = "only-filled") && filename.ends_with("Filled.svg"))
      || (cfg!(feature = "only-color") && filename.ends_with("Color.svg"))
      || (cfg!(feature = "only-regular") && filename.ends_with("Regular.svg"))
      || (cfg!(feature = "only-light") && filename.ends_with("Light.svg"));
    if !keep {
      return false;
    }
  }

  // ── negative-filter: `no-*` features ─────────────────────────────────────
  if cfg!(feature = "no-filled") && filename.ends_with("Filled.svg") {
    return false;
  }
  if cfg!(feature = "no-color") && filename.ends_with("Color.svg") {
    return false;
  }
  if cfg!(feature = "no-regular") && filename.ends_with("Regular.svg") {
    return false;
  }
  if cfg!(feature = "no-light") && filename.ends_with("Light.svg") {
    return false;
  }

  true
}

// ── declare!() ───────────────────────────────────────────────────────────────

/// Generate lean stub functions for every Fluent UI SVG icon in the `icons/` directory.
///
/// Place this macro call **once** in your icons module to make every icon discoverable
/// by the IDE without any runtime cost:
///
/// ```ignore
/// // src/icons/fluent.rs
/// iced_fluent_icons::declare!();
/// ```
///
/// Each stub:
///
/// - is a plain `pub fn` — no structs, no `impl` blocks, no traits, no `include_bytes!`
/// - carries a rustdoc image so VS Code and JetBrains IDEs render the icon on hover
/// - has the return type `::iced::widget::Svg<'static>` for correct type inference
/// - has a `panic!` body — it is never executed because [`inventory`] rewrites every
///   call site at compile time
///
/// Active [feature flags] are respected: icons belonging to excluded variant families
/// are not emitted.
///
/// Generated stub shape (one per included SVG file):
///
/// ```ignore
/// /// ![DismissCircleColor](https://raw.githubusercontent.com/…/DismissCircleColor.svg)
/// ///
/// /// `DismissCircleColor.svg` — annotate the calling function with
/// /// `#[iced_fluent_icons::inventory]`.
/// pub fn dismiss_circle_color() -> ::iced::widget::Svg<'static> {
///     panic!("icon stub …")
/// }
/// ```
#[proc_macro]
pub fn declare(input: TokenStream) -> TokenStream {
  let _ = input;
  match expand_declare() {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
}

fn expand_declare() -> Result<proc_macro2::TokenStream> {
  let files = collect_svg_files(Path::new(ICONS_DIR))?;

  let stubs = files
    .iter()
    .filter_map(|path| {
      let filename = path.file_name()?.to_str()?;
      if icon_included(filename) { Some(path) } else { None }
    })
    .map(|path| generate_stub(path))
    .collect::<Result<Vec<_>>>()?;

  Ok(quote! { #(#stubs)* })
}

/// Emit the hollow stub for one SVG file.
fn generate_stub(path: &Path) -> Result<proc_macro2::TokenStream> {
  let stem = path
    .file_stem()
    .and_then(|n| n.to_str())
    .ok_or_else(|| Error::new(Span::call_site(), "icon path has a non-UTF-8 file stem"))?;

  validate_stem(stem)?;

  let filename_ext = path.file_name().and_then(std::ffi::OsStr::to_str).unwrap();
  let preview_link = format!(
    "https://raw.githubusercontent.com/rootfebri/iced-fluent-icons/refs/heads/master/icons/{filename_ext}"
  );

  let fn_ident = format_ident!("{}", to_snake_case(stem));

  let image_doc = format!("![{stem}]({preview_link})");
  let detail_doc = format!(
    "`{filename_ext}` — annotate the calling function with `#[iced_fluent_icons::inventory]`."
  );
  let panic_msg = format!(
    "icon stub `{stem}` — the calling function must be annotated with `#[iced_fluent_icons::inventory]`"
  );

  Ok(quote! {
    #[doc = #image_doc]
    #[doc = ""]
    #[doc = #detail_doc]
    pub fn #fn_ident() -> ::iced::widget::Svg<'static> {
      panic!(#panic_msg)
    }
  })
}

// ── #[inventory] ─────────────────────────────────────────────────────────────

/// Parsed arguments for `#[inventory(…)]`.
struct InventoryArgs {
  /// Pixel width for every emitted icon widget.
  width: u32,
  /// Pixel height for every emitted icon widget.
  height: u32,
}

impl Default for InventoryArgs {
  fn default() -> Self {
    Self { width: 24, height: 24 }
  }
}

/// Accepted syntax variants:
///
/// ```ignore
/// #[inventory]                              // → 24 × 24
/// #[inventory(size = 32)]                   // → 32 × 32
/// #[inventory(width = 20)]                  // → 20 × 24
/// #[inventory(height = 48)]                 // → 24 × 48
/// #[inventory(width = 20, height = 48)]     // → 20 × 48
/// #[inventory(size = 32, height = 48)]      // size is the fallback; height overrides it
/// ```
impl Parse for InventoryArgs {
  fn parse(input: ParseStream) -> Result<Self> {
    if input.is_empty() {
      return Ok(Self::default());
    }

    // key = value pairs separated by commas
    let pairs = Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated(input)?;

    let mut size: Option<u32> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;

    for pair in &pairs {
      let key = pair
        .path
        .get_ident()
        .ok_or_else(|| Error::new_spanned(&pair.path, "expected `size`, `width`, or `height`"))?
        .to_string();

      let val: u32 = match &pair.value {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(n), .. }) => n.base10_parse()?,
        other => {
          return Err(Error::new_spanned(other, "expected an integer literal"));
        }
      };

      match key.as_str() {
        "size" => size = Some(val),
        "width" => width = Some(val),
        "height" => height = Some(val),
        _ => return Err(Error::new_spanned(&pair.path, "expected `size`, `width`, or `height`")),
      }
    }

    let fallback = size.unwrap_or(24);
    Ok(Self { width: width.unwrap_or(fallback), height: height.unwrap_or(fallback) })
  }
}

/// Replace calls to icon stub functions with inline SVG-loading code.
///
/// Apply this attribute to any `fn`, `impl` block, or `mod` that calls icon stubs
/// generated by [`declare!`]:
///
/// ```ignore
/// #[iced_fluent_icons::inventory]
/// fn view(&self) -> iced::Element<'_, Message> {
///     let close = icons::fluent::dismiss_circle_color();
///     // …every qualifying call is rewritten at compile time…
/// }
/// ```
///
/// ## Custom icon size
///
/// ```ignore
/// #[iced_fluent_icons::inventory(size = 32)]              // 32 × 32
/// #[iced_fluent_icons::inventory(width = 20, height = 24)]
/// ```
///
/// ## What gets rewritten
///
/// Any **zero-argument call expression** whose **last path segment** matches a known
/// icon function name is replaced with an inline block:
///
/// ```ignore
/// {
///     let bytes: &'static [u8] = include_bytes!("/abs/path/to/DismissCircleColor.svg");
///     let handle = ::iced::widget::svg::Handle::from_memory(bytes);
///     ::iced::widget::svg(handle).width(24).height(24)
/// }
/// ```
///
/// Calls with arguments, and calls whose name does not match any icon, are left untouched.
///
/// ## Scope
///
/// - a single `fn` — rewrites that function's body
/// - an `impl` block — rewrites all method bodies within it
/// - a `mod` — rewrites all functions inside the module (including nested items)
#[proc_macro_attribute]
pub fn inventory(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as InventoryArgs);

  let icon_map = match build_icon_map() {
    Ok(m) => m,
    Err(e) => return e.to_compile_error().into(),
  };

  let mut ast = parse_macro_input!(item as syn::Item);

  IconCallReplacer { icon_map, width: args.width, height: args.height }.visit_item_mut(&mut ast);

  quote! { #ast }.into()
}

// ── AST visitor ───────────────────────────────────────────────────────────────

struct IconCallReplacer {
  /// `"dismiss_circle_color"` → `/abs/path/to/DismissCircleColor.svg`
  icon_map: HashMap<String, PathBuf>,
  width: u32,
  height: u32,
}

impl VisitMut for IconCallReplacer {
  fn visit_expr_mut(&mut self, expr: &mut syn::Expr) {
    // Bottom-up: recurse into sub-expressions first so that icon calls nested
    // inside other expressions (closures, if-let arms, etc.) are all replaced.
    syn::visit_mut::visit_expr_mut(self, expr);

    // Only care about `some::path::fn_name()` — a path call with zero arguments.
    let svg_path: Option<PathBuf> = match expr {
      syn::Expr::Call(call) if call.args.is_empty() => match &*call.func {
        syn::Expr::Path(path_expr) => path_expr
          .path
          .segments
          .last()
          .map(|seg| seg.ident.to_string())
          .and_then(|name| self.icon_map.get(&name).cloned()),
        _ => None,
      },
      _ => None,
    };

    if let Some(path) = svg_path {
      // Absolute, forward-slash path → safe as an include_bytes! literal on all platforms.
      let path_lit = LitStr::new(&path.to_string_lossy().replace('\\', "/"), Span::call_site());
      let w = LitInt::new(&self.width.to_string(), Span::call_site());
      let h = LitInt::new(&self.height.to_string(), Span::call_site());

      *expr = syn::parse_quote! {
        {
          let bytes: &'static [u8] = include_bytes!(#path_lit);
          let handle = ::iced::widget::svg::Handle::from_memory(bytes);
          ::iced::widget::svg::<'static, ::iced::Theme>(handle).width(#w).height(#h)
        }
      };
    }
  }

  /// Parse the macro body as comma-separated expressions and visit each one.
  ///
  /// Without this override, `syn::visit_mut` treats macro bodies as opaque
  /// `TokenStream`s — icon calls inside `column![]`, `row![]`, `stack![]`, etc.
  /// would never be reached by `visit_expr_mut`.
  ///
  /// If the body cannot be parsed as `Expr, Expr, …` parsing fails silently and
  /// the tokens are left untouched.
  fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
    if let Ok(mut args) =
      mac.parse_body_with(Punctuated::<syn::Expr, Token![,]>::parse_terminated)
    {
      for arg in args.iter_mut() {
        self.visit_expr_mut(arg);
      }
      mac.tokens = quote! { #args };
    }
  }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Collect all `.svg` files directly inside `dir` (non-recursive), sorted by filename.
fn collect_svg_files(dir: &Path) -> Result<Vec<PathBuf>> {
  let read_dir = fs::read_dir(dir).map_err(|e| {
    Error::new(
      Span::call_site(),
      format!("cannot read icon directory `{}`: {e}", dir.display()),
    )
  })?;

  let mut files: Vec<PathBuf> = read_dir
    .flatten()
    .map(|entry| entry.path())
    .filter(|p| {
      p.is_file()
        && p
          .extension()
          .and_then(|ext| ext.to_str())
          .map(|ext| ext.eq_ignore_ascii_case("svg"))
          .unwrap_or(false)
    })
    .collect();

  // Deterministic order regardless of filesystem traversal order.
  files.sort_unstable_by(|a, b| {
    a.file_name()
      .and_then(|n| n.to_str())
      .cmp(&b.file_name().and_then(|n| n.to_str()))
  });

  Ok(files)
}

/// Build the lookup table used by [`IconCallReplacer`].
///
/// Returns `snake_case_name → absolute SVG path`.
/// Respects the same feature-flag filtering as [`expand_declare`] so that
/// `#[inventory]` never tries to embed an icon that was excluded by a feature flag.
fn build_icon_map() -> Result<HashMap<String, PathBuf>> {
  let files = collect_svg_files(Path::new(ICONS_DIR))?;
  let mut map = HashMap::with_capacity(files.len());

  for path in files {
    let filename = match path.file_name().and_then(|n| n.to_str()) {
      Some(f) => f,
      None => continue,
    };

    if !icon_included(filename) {
      continue;
    }

    if let Some(stem) = path.file_stem().and_then(|n| n.to_str()) {
      map.insert(to_snake_case(stem), path);
    }
  }

  Ok(map)
}

/// Verify that `stem` can be turned into a valid Rust identifier.
fn validate_stem(stem: &str) -> Result<()> {
  let first = stem
    .chars()
    .next()
    .ok_or_else(|| Error::new(Span::call_site(), "icon filename stem is empty"))?;

  if !(first.is_ascii_alphabetic() || first == '_') {
    return Err(Error::new(
      Span::call_site(),
      format!("`{stem}`: stem must begin with an ASCII letter or `_`"),
    ));
  }

  if !stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
    return Err(Error::new(
      Span::call_site(),
      format!(
        "`{stem}`: stem must contain only ASCII letters, digits, and `_`; \
         rename the file to UpperCamelCase"
      ),
    ));
  }

  Ok(())
}

/// Convert an `UpperCamelCase` (or mixed-case) stem to `snake_case`.
///
/// Rules applied in priority order:
///
/// 1. A `_` in the input is preserved (consecutive underscores are collapsed to one).
/// 2. An uppercase letter that follows a **lowercase letter or ASCII digit** gets a `_` prefix.
/// 3. An uppercase letter that is followed by a **lowercase** letter *and* preceded by another
///    **uppercase** letter also gets a `_` prefix — this handles runs like `HTML` in `HTMLParser`.
///
/// Examples:
///
/// | input | output |
/// |---|---|
/// | `AddRegular` | `add_regular` |
/// | `DismissCircleColor` | `dismiss_circle_color` |
/// | `HTMLParser` | `html_parser` |
/// | `SVGColor` | `svg_color` |
/// | `Add24Filled` | `add24_filled` |
fn to_snake_case(s: &str) -> String {
  let chars: Vec<char> = s.chars().collect();
  let len = chars.len();
  let mut out = String::with_capacity(len + 8);

  for i in 0..len {
    let ch = chars[i];

    if ch == '_' {
      if !out.ends_with('_') {
        out.push('_');
      }
      continue;
    }

    if ch.is_ascii_uppercase() {
      let prev_lower_or_digit =
        i > 0 && chars[i - 1] != '_' && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());

      let prev_upper_next_lower =
        i > 0 && chars[i - 1].is_ascii_uppercase() && chars.get(i + 1).is_some_and(|c| c.is_ascii_lowercase());

      if i > 0 && !out.ends_with('_') && (prev_lower_or_digit || prev_upper_next_lower) {
        out.push('_');
      }

      out.push(ch.to_ascii_lowercase());
    } else {
      out.push(ch);
    }
  }

  if out.is_empty() { "_".to_owned() } else { out }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::to_snake_case;

  macro_rules! case {
    ($input:expr => $expected:expr) => {
      assert_eq!(to_snake_case($input), $expected, "to_snake_case({:?})", $input)
    };
  }

  #[test]
  fn basic_camel_case() {
    case!("AddRegular"                    => "add_regular");
    case!("DocumentAddColor"              => "document_add_color");
    case!("DismissCircleColor"            => "dismiss_circle_color");
    case!("AccessTime"                    => "access_time");
    case!("Filled"                        => "filled");
  }

  #[test]
  fn long_names() {
    case!("AccessibilityCheckmarkFilled"  => "accessibility_checkmark_filled");
    case!("AddSubtractCircleFilled"       => "add_subtract_circle_filled");
    case!("AccessTimeFilled"              => "access_time_filled");
  }

  #[test]
  fn acronym_boundaries() {
    case!("HTMLParser"                    => "html_parser");
    case!("SVGColor"                      => "svg_color");
    case!("AddSVGFilled"                  => "add_svg_filled");
  }

  #[test]
  fn digits_in_name() {
    case!("Add24Filled"                   => "add24_filled");
    case!("Alert20Regular"                => "alert20_regular");
  }

  #[test]
  fn leading_underscore_preserved() {
    case!("_Private"                      => "_private");
  }

  #[test]
  fn consecutive_underscores_collapsed() {
    case!("Some_Name"                     => "some_name");
  }

  // ── icon_included tests ───────────────────────────────────────────────────
  // These always pass because no `only-*` / `no-*` features are active in
  // `cargo test`.  They mainly verify the function compiles and is callable.
  #[test]
  fn icon_included_all_variants_allowed_by_default() {
    use super::icon_included;
    assert!(icon_included("AddFilled.svg"));
    assert!(icon_included("AddRegular.svg"));
    assert!(icon_included("AddColor.svg"));
    assert!(icon_included("AddLight.svg"));
  }
}
