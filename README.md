# iced-fluent-icons

A Rust proc-macro crate that embeds [Microsoft Fluent UI](https://github.com/microsoft/fluentui-system-icons) SVG icons into an [`iced`](https://iced.rs) application with **minimal LSP overhead**.

---

## How it works

The crate provides two complementary macros that split the work cleanly between **IDE/LSP time** and **compile time**:

| Macro          | When it runs        | What it does                                                           |
|----------------|---------------------|------------------------------------------------------------------------|
| `declare!()`   | LSP expansion       | Emits lightweight stub functions — no `include_bytes!`, no byte arrays |
| `#[inventory]` | `rustc` compilation | Rewrites every stub call to inline SVG-loading code                    |

---

## Usage

### 1. Add the dependency

```toml
[dependencies]
iced-fluent-icons = "1.0"
```

### 2. Declare icons in a module

Call `declare!()` **once** in your icons module:

```rust
// src/icons/fluent.rs
fluentui_icons::declare!();
```

This generates one `pub fn` per SVG file. Each stub:

- carries a **rustdoc image** so VS Code and JetBrains IDEs show a preview on hover / signature-help
- has the correct return type `::iced::widget::Svg<'static>` for type inference and completion
- has a `panic!` body — it is **never executed** because `#[inventory]` rewrites every call site

### 3. Annotate callers with `#[inventory]`

```rust
#[fluentui_icons::inventory]
fn toolbar() -> iced::Element<'_, Message> {
    let close = icons::fluent::dismiss_circle_color();
    // … more icon calls …
}
```

At compile time every zero-argument call whose last path segment matches a known icon name is replaced with:

```rust,ignore
{
    let bytes: &'static [u8] = include_bytes!("/abs/path/to/DismissCircleColor.svg");
    let handle = ::iced::widget::svg::Handle::from_memory(bytes);
    ::iced::widget::svg::<'static, ::iced::Theme>(handle).width(24).height(24)
}
```

All of the following call forms are handled identically:

```rust,ignore
dismiss_circle_color()                        // after `use icons::fluent::*`
fluent::dismiss_circle_color()
icons::fluent::dismiss_circle_color()
crate::icons::fluent::dismiss_circle_color()
```

`#[inventory]` can be applied to a **`fn`**, an **`impl` block**, or a **`mod`**.

---

## Icon variants

Icons ship in up to four variants. You can opt out of unwanted variants with Cargo features to keep compilation fast:

| Feature      | Excludes             |
|--------------|----------------------|
| `no-filled`  | `*Filled.svg` icons  |
| `no-color`   | `*Color.svg` icons   |
| `no-regular` | `*Regular.svg` icons |
| `no-light`   | `*Light.svg` icons   |

```toml
[dependencies]
iced-fluent-icons = { version = "1.0", features = ["no-light", "no-color"] }
```

---

## Icon naming

SVG filenames (`UpperCamelCase`) are converted to Rust function names (`snake_case`):

| File                               | Function                           |
|------------------------------------|------------------------------------|
| `AddRegular.svg`                   | `add_regular()`                    |
| `DismissCircleColor.svg`           | `dismiss_circle_color()`           |
| `AccessibilityCheckmarkFilled.svg` | `accessibility_checkmark_filled()` |
| `AlertUrgentRegular.svg`           | `alert_urgent_regular()`           |

---

## How macro bodies inside widget macros are handled

`#[inventory]` also rewrites icon calls that appear inside `iced` widget macros such as `column![]`, `row![]`, and `stack![]`. The macro body is parsed as comma-separated expressions and each one is visited recursively, so the following works without extra annotations:

```rust
#[fluentui_icons::inventory]
fn sidebar() -> iced::Element<'_, Message> {
    iced::widget::column![
        icons::fluent::home_filled(),
        icons::fluent::settings_regular(),
    ]
    .into()
}
```

---

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

