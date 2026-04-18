//! # fluentui-icons
//!
//! Proc-macro crate for embedding Fluent UI SVG icons into an [`iced`](https://iced.rs)
//! application with minimal LSP overhead.
//!
//! ## How it works
//!
//! The crate is split into two complementary macros:
//!
//! ### 1. [`declare!`] — stub generation (LSP-only, compile-time lean)
//!
//! Call this once in your icons module:
//!
//! ```ignore
//! // src/icons/fluent.rs
//! fluentui_icons::declare!();
//! ```
//!
//! For each `Foo.svg` in the `icons/` directory this emits roughly:
//!
//! ```ignore
//! /// ![Foo](file:///…/icons/Foo.svg)   ← rendered in IDE hover / sig-help
//! ///
//! /// `Foo.svg` — annotate the caller with `#[fluentui_icons::inventory]`.
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
//! #[fluentui_icons::inventory]
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
//! [`inventory`] walks the entire item's AST using [`syn::visit_mut`] and replaces every
//! zero-argument call expression whose **last path segment** is a known icon function name.
//! Icon calls inside widget macros (`w::column![]`, `w::row![]`, `w::stack![]`, etc.)
//! are also rewritten — the macro body is parsed as comma-separated expressions and
//! each one is visited recursively.
//! This means all of the following are handled identically:
//!
//! ```ignore
//! dismiss_circle_color()                        // after `use icons::fluent::*`
//! fluent::dismiss_circle_color()
//! icons::fluent::dismiss_circle_color()
//! crate::icons::fluent::dismiss_circle_color()
//! ```

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::{Error, LitStr, Result, parse_macro_input, visit_mut::VisitMut};

/// Absolute path to the `icons/` directory inside this proc-macro crate.
///
/// Set by `build.rs` via `cargo:rustc-env=FLUENTUI_ICONS_DIR=…` so the value is
/// baked into the proc-macro binary and is always correct regardless of which crate
/// invokes the macros.
const ICONS_DIR: &str = env!("FLUENTUI_ICONS_DIR");

// ── declare!() ───────────────────────────────────────────────────────────────

/// Generate lean stub functions for every Fluent UI SVG icon in the `icons/` directory.
///
/// Place this macro call **once** in your icons module to make every icon discoverable
/// by the IDE without any runtime cost:
///
/// ```ignore
/// // src/icons/fluent.rs
/// fluentui_icons::declare!();
/// ```
///
/// Each stub:
///
/// - is a plain `pub fn` — no structs, no `impl` blocks, no traits, no `include_bytes!`
/// - carries a rustdoc image (`![Name](file:///…)`) so VS Code and JetBrains IDEs
///   render the icon in hover and signature-help popups
/// - has the return type `::iced::widget::Svg<'static>` for correct type inference
/// - has a `panic!` body — it is never executed because [`inventory`] rewrites every
///   call site at compile time
///
/// Generated stub shape (one per SVG file):
///
/// ```ignore
/// /// ![DismissCircleColor](file:///…/DismissCircleColor.svg)
/// ///
/// /// `DismissCircleColor.svg` — annotate the caller with `#[fluentui_icons::inventory]`.
/// pub fn dismiss_circle_color() -> ::iced::widget::Svg<'static> {
///     panic!("icon stub …")
/// }
/// ```
#[proc_macro]
pub fn declare(input: TokenStream) -> TokenStream {
  // No arguments today; the `_` suppresses the unused-variable warning and
  // the parameter stays for forward-compatible extension later.
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
    .map(|path| generate_stub(path))
    .collect::<Result<Vec<_>>>()?;

  Ok(quote! { #(#stubs)* })
}

/// Emit the hollow stub for one SVG file.
fn generate_stub(path: &Path) -> Result<proc_macro2::TokenStream> {
  let file_name = path
    .file_name()
    .and_then(|n| n.to_str())
    .and_then(|n| n.strip_suffix(path.extension()?.to_str()?))
    .and_then(|n| n.strip_suffix('.'))
    .ok_or_else(|| Error::new(Span::call_site(), "icon path has a non-UTF-8 filename"))?;

  let stem = path
    .file_stem()
    .and_then(|n| n.to_str())
    .ok_or_else(|| Error::new(Span::call_site(), "icon path has a non-UTF-8 file stem"))?;

  validate_stem(stem)?;

  // Use forward slashes everywhere — works on all platforms for file:// URLs
  // and for include_bytes! on Windows too.
  let svg_path = path.to_string_lossy().replace('\\', "/");

  let fn_ident = format_ident!("{}", to_snake_case(stem));

  // These three strings become the rustdoc that powers IDE hover / signature-help.
  let image_doc = format!("![{stem}](file://{svg_path})");
  let detail_doc = format!("`{file_name}` — annotate the calling function with `#[fluentui_icons::inventory]`.");
  let panic_msg =
    format!("icon stub `{stem}` — the calling function must be annotated with `#[fluentui_icons::inventory]`");

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

/// Replace calls to icon stub functions with inline SVG-loading code.
///
/// Apply this attribute to any `fn`, `impl` block, or `mod` that calls icon stubs
/// generated by [`declare!`]:
///
/// ```ignore
/// #[fluentui_icons::inventory]
/// fn view(&self) -> iced::Element<'_, Message> {
///     let close = icons::fluent::dismiss_circle_color();
///     // …every qualifying call is rewritten at compile time…
/// }
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
/// Can be applied to:
///
/// - a single `fn` — rewrites that function's body
/// - an `impl` block — rewrites all method bodies within it
/// - a `mod` — rewrites all functions inside the module (including nested items)
#[proc_macro_attribute]
pub fn inventory(attr: TokenStream, item: TokenStream) -> TokenStream {
  // No arguments today.
  let _ = attr;

  let icon_map = match build_icon_map() {
    Ok(m) => m,
    Err(e) => return e.to_compile_error().into(),
  };

  let mut ast = parse_macro_input!(item as syn::Item);

  IconCallReplacer { icon_map }.visit_item_mut(&mut ast);

  quote! { #ast }.into()
}

// ── AST visitor ───────────────────────────────────────────────────────────────

struct IconCallReplacer {
  /// `"dismiss_circle_color"` → `/abs/path/to/DismissCircleColor.svg`
  icon_map: HashMap<String, PathBuf>,
}

impl VisitMut for IconCallReplacer {
  fn visit_expr_mut(&mut self, expr: &mut syn::Expr) {
    // Bottom-up: recurse into sub-expressions first so that icon calls nested
    // inside other expressions (closures, if-let arms, etc.) are all replaced.
    // For `Expr::Macro` nodes this dispatches into our `visit_macro_mut` above,
    // which in turn recurses into the macro body.
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

      // Replace the call expression with the actual SVG-loading block.
      // The block evaluates to `iced::widget::Svg<'static>`, matching the stub's return type.
      *expr = syn::parse_quote! {
        {
          let bytes: &'static [u8] = include_bytes!(#path_lit);
          let handle = ::iced::widget::svg::Handle::from_memory(bytes);
          ::iced::widget::svg::<'static, ::iced::Theme>(handle).width(24).height(24)
        }
      };
    }
  }

  /// Parse the macro body as comma-separated expressions and visit each one.
  ///
  /// Without this override, `syn::visit_mut` treats macro bodies as opaque
  /// `TokenStream`s — icon calls inside `w::column![]`, `w::row![]`,
  /// `w::stack![]`, etc. would never be reached by `visit_expr_mut`.
  ///
  /// If the body cannot be parsed as `Expr, Expr, …` (e.g. `matches!`,
  /// `if_chain!`, or any macro with non-expression syntax) parsing fails
  /// silently and the tokens are left untouched.
  fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
    if let Ok(mut args) =
      mac.parse_body_with(syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
    {
      for arg in args.iter_mut() {
        self.visit_expr_mut(arg);
      }
      // Reserialize — idempotent if nothing changed.
      mac.tokens = quote! { #args };
    }
    // If parsing failed, leave mac.tokens untouched.
    // We intentionally skip `syn::visit_mut::visit_macro_mut` because it only
    // visits the macro *path* (e.g. `w::column`), never the body — so there is
    // nothing useful left to delegate to.
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
fn build_icon_map() -> Result<HashMap<String, PathBuf>> {
  let files = collect_svg_files(Path::new(ICONS_DIR))?;
  let mut map = HashMap::with_capacity(files.len());

  for path in files {
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
      // Preserve underscores but never emit consecutive ones.
      if !out.ends_with('_') {
        out.push('_');
      }
      continue;
    }

    if ch.is_ascii_uppercase() {
      // Rule 2: uppercase after lowercase or digit.
      let prev_lower_or_digit =
        i > 0 && chars[i - 1] != '_' && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());

      // Rule 3: uppercase after uppercase when next is lowercase (acronym boundary).
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
    // digit counts as a lowercase boundary trigger
    case!("Add24Filled"                   => "add24_filled");
    case!("Alert20Regular"                => "alert20_regular");
  }

  #[test]
  fn leading_underscore_preserved() {
    case!("_Private"                      => "_private");
  }

  #[test]
  fn consecutive_underscores_collapsed() {
    // underscores in input should not produce runs of underscores
    case!("Some_Name"                     => "some_name");
  }
}
