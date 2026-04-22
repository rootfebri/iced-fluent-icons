//! MCP (Model Context Protocol) server for the iced-fluent-icons crate.
//!
//! Exposes tools so AI coding agents can discover icons, understand feature flags,
//! and get usage examples without needing to read the source code themselves.
//!
//! Protocol: JSON-RPC 2.0 over stdio (newline-delimited JSON).
//!
//! Run: `cargo run --bin fluent-icons-mcp`

use serde_json::{Map, Value, json};
use std::io::{self, BufRead, Write};

// Static icon catalogue generated at build time from the icons/ directory.
include!(concat!(env!("OUT_DIR"), "/icons_data.rs"));

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "fluent-icons-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

fn main() {
  let stdin = io::stdin();
  let stdout = io::stdout();
  let mut out = io::BufWriter::new(stdout.lock());

  for line in stdin.lock().lines() {
    let line = match line {
      Ok(l) => l,
      Err(_) => break,
    };
    if line.trim().is_empty() {
      continue;
    }
    match serde_json::from_str::<Value>(&line) {
      Ok(msg) => {
        if let Some(response) = dispatch(&msg) {
          let _ = serde_json::to_string(&response).map(|s| {
            let _ = writeln!(out, "{s}");
            out.flush()
          });
        }
      }
      Err(e) => {
        // Reply with a parse-error response (no id available, use null).
        let err = json_error(Value::Null, -32700, &format!("Parse error: {e}"));
        if let Ok(s) = serde_json::to_string(&err) {
          let _ = writeln!(out, "{s}");
          let _ = out.flush();
        }
      }
    }
  }
}

// ──────────────────────────────────────────────────────────────────────────────
// Message dispatch
// ──────────────────────────────────────────────────────────────────────────────

fn dispatch(msg: &Value) -> Option<Value> {
  let method = msg.get("method")?.as_str()?;

  // Notifications have no "id" field — handle them silently, no response.
  let id = match msg.get("id") {
    Some(id) => id.clone(),
    None => {
      // e.g. notifications/initialized — nothing to do.
      return None;
    }
  };

  let result = match method {
    "initialize" => handle_initialize(msg.get("params")),
    "tools/list" => handle_tools_list(),
    "tools/call" => handle_tools_call(msg.get("params")),
    "ping" => Ok(json!({})),
    unknown => Err(json!({
        "code": -32601,
        "message": format!("Method not found: {unknown}")
    })),
  };

  Some(match result {
    Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
    Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error":  e }),
  })
}

fn json_error(id: Value, code: i64, msg: &str) -> Value {
  json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

// ──────────────────────────────────────────────────────────────────────────────
// Protocol handlers
// ──────────────────────────────────────────────────────────────────────────────

fn handle_initialize(_params: Option<&Value>) -> Result<Value, Value> {
  Ok(json!({
      "protocolVersion": PROTOCOL_VERSION,
      "capabilities": { "tools": {} },
      "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
  }))
}

fn handle_tools_list() -> Result<Value, Value> {
  Ok(json!({ "tools": tools_schema() }))
}

fn handle_tools_call(params: Option<&Value>) -> Result<Value, Value> {
  let params = params.ok_or_else(|| json!({ "code": -32602, "message": "Missing params" }))?;

  let name = params
    .get("name")
    .and_then(|n| n.as_str())
    .ok_or_else(|| json!({ "code": -32602, "message": "Missing tool name" }))?;

  let empty = Value::Object(Map::new());
  let args = params.get("arguments").unwrap_or(&empty);

  let text = match name {
    "list_icons" => tool_list_icons(args),
    "get_icon" => tool_get_icon(args),
    "describe_features" => tool_describe_features(),
    "get_crate_info" => tool_get_crate_info(),
    unknown => return Err(json!({ "code": -32601, "message": format!("Unknown tool: {unknown}") })),
  };

  Ok(json!({ "content": [{ "type": "text", "text": text }] }))
}

// ──────────────────────────────────────────────────────────────────────────────
// Tool: list_icons
// ──────────────────────────────────────────────────────────────────────────────

fn tool_list_icons(args: &Value) -> String {
  let variant_filter = args.get("variant").and_then(|v| v.as_str());
  let search = args.get("search").and_then(|v| v.as_str()).map(|s| s.to_lowercase());
  let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
  let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100).min(500) as usize;

  let filtered: Vec<&McpIconEntry> = MCP_ALL_ICONS
    .iter()
    .filter(|icon| {
      if variant_filter.is_some_and(|v| !icon.variant.eq_ignore_ascii_case(v)) {
        return false;
      }
      if let Some(ref q) = search {
        let q = q.as_str();
        if !icon.fn_name.contains(q) && !icon.base_name.to_lowercase().contains(q) {
          return false;
        }
      }
      true
    })
    .collect();

  let total = filtered.len();
  let page: Vec<&McpIconEntry> = filtered.into_iter().skip(offset).take(limit).collect();

  let mut out = String::new();
  out.push_str(&format!(
    "Showing {}/{} icons (offset={offset}, limit={limit})\n\n",
    page.len(),
    total
  ));
  if total == 0 {
    out.push_str("No icons matched your query.\n");
    return out;
  }

  // Header
  out.push_str("fn_name | variant | base_name\n");
  out.push_str("--- | --- | ---\n");
  for icon in &page {
    out.push_str(&format!("`{}` | {} | {}\n", icon.fn_name, icon.variant, icon.base_name));
  }

  let remaining = total.saturating_sub(offset + page.len());
  if remaining > 0 {
    out.push_str(&format!(
      "\n*{remaining} more icons not shown. Fetch next page with `offset={}`.*\n",
      offset + page.len()
    ));
  }
  out
}

// ──────────────────────────────────────────────────────────────────────────────
// Tool: get_icon
// ──────────────────────────────────────────────────────────────────────────────

fn tool_get_icon(args: &Value) -> String {
  let name = match args.get("name").and_then(|v| v.as_str()) {
    Some(n) => n.to_lowercase(),
    None => return "Error: `name` parameter is required.".to_string(),
  };

  let matches: Vec<&McpIconEntry> = MCP_ALL_ICONS
    .iter()
    .filter(|icon| {
      icon.fn_name == name.as_str()
        || icon.base_name.to_lowercase() == name.as_str()
        || icon.fn_name.contains(name.as_str())
        || icon.base_name.to_lowercase().contains(name.as_str())
    })
    .collect();

  if matches.is_empty() {
    return format!(
      "No icons found matching `{name}`.\n\nTip: use `list_icons` with `search` parameter to browse icons."
    );
  }

  // Group by base_name for a tidy presentation.
  let mut by_base: std::collections::BTreeMap<&str, Vec<&McpIconEntry>> = std::collections::BTreeMap::new();
  for icon in &matches {
    by_base.entry(icon.base_name).or_default().push(*icon);
  }

  let mut out = String::new();
  out.push_str(&format!("Found {} icon(s) matching `{name}`:\n\n", matches.len()));

  for (base, variants) in &by_base {
    out.push_str(&format!("### {base}\n\n"));
    for icon in variants {
      out.push_str(&format!(
          "- **`{fn}`** — {var} variant\n  ```rust\n  {fn}()\n  ```\n",
          fn = icon.fn_name,
          var = icon.variant,
      ));
    }
    out.push('\n');
  }

  out
}

// ──────────────────────────────────────────────────────────────────────────────
// Tool: describe_features
// ──────────────────────────────────────────────────────────────────────────────

fn tool_describe_features() -> String {
  let (filled, color, regular, light) = variant_counts();
  let total = MCP_ICON_COUNT;

  format!(
    r#"# iced-fluent-icons — Cargo Feature Flags

## Icon counts by variant
| Variant  | Count |
|----------|-------|
| Filled   | {filled}  |
| Color    | {color}   |
| Regular  | {regular} |
| Light    | {light}   |
| **Total**| **{total}** |

---

## Exclusion features  *(default: all variants included)*

These features **remove** an entire variant family, shrinking compile time and binary size.

| Feature      | Removes                | Icons removed |
|--------------|------------------------|---------------|
| `no-filled`  | `*Filled.svg` icons    | {filled}       |
| `no-color`   | `*Color.svg` icons     | {color}        |
| `no-regular` | `*Regular.svg` icons   | {regular}      |
| `no-light`   | `*Light.svg` icons     | {light}        |

**Example** — drop Light and Color variants:
```toml
[dependencies]
iced-fluent-icons = {{ version = "1.0", features = ["no-light", "no-color"] }}
```

---

## Inclusion features  *(keep ONLY the named families)*

Multiple `only-*` features may be combined freely.

| Feature        | Keeps only             | Icons kept |
|----------------|------------------------|------------|
| `only-filled`  | `*Filled.svg` icons    | {filled}    |
| `only-color`   | `*Color.svg` icons     | {color}     |
| `only-regular` | `*Regular.svg` icons   | {regular}   |
| `only-light`   | `*Light.svg` icons     | {light}     |

**Example** — keep only Regular + Filled:
```toml
[dependencies]
iced-fluent-icons = {{ version = "1.0", features = ["only-regular", "only-filled"] }}
```

---

## Notes
- `declare!()` and `#[inventory]` both honour the same feature flags.
- Excluded icons are **never** embedded in the binary.
- Exclusion and inclusion flags operate independently; if both apply, *inclusion* wins over exclusion when they agree.
"#
  )
}

// ──────────────────────────────────────────────────────────────────────────────
// Tool: get_crate_info
// ──────────────────────────────────────────────────────────────────────────────

fn tool_get_crate_info() -> String {
  let (filled, color, regular, light) = variant_counts();
  let total = MCP_ICON_COUNT;

  format!(
    r#"# iced-fluent-icons

Proc-macro crate that embeds Microsoft Fluent UI SVG icons into [iced](https://iced.rs) applications
with **minimal LSP overhead**.

## Icon catalogue
| Variant  | Count  |
|----------|--------|
| Filled   | {filled}   |
| Color    | {color}    |
| Regular  | {regular}  |
| Light    | {light}    |
| **Total**| **{total}** |

---

## How to use

### 1 — Add the dependency
```toml
[dependencies]
iced-fluent-icons = "1.0"
```

### 2 — Declare stubs once in your icons module
```rust
// src/icons/fluent.rs
iced_fluent_icons::declare!();
```

Each SVG file becomes one `pub fn`. The function:
- Has the correct return type `::iced::widget::Svg<'static>` (IDE completion works)
- Shows a rustdoc image preview on hover in VS Code / JetBrains
- Has a `panic!` body — it is **never executed** at runtime

### 3 — Annotate call sites with `#[inventory]`
```rust
#[iced_fluent_icons::inventory]
fn toolbar() -> iced::Element<'_, Message> {{
    let close = icons::fluent::dismiss_circle_color();
    let home  = icons::fluent::home_filled();
    // ...
}}
```

`#[inventory]` rewrites every zero-argument icon call to inline `include_bytes!` at
**compile time**, never at LSP time.

#### Supported call forms (all equivalent)
```rust
dismiss_circle_color()                      // after `use icons::fluent::*`
fluent::dismiss_circle_color()
icons::fluent::dismiss_circle_color()
crate::icons::fluent::dismiss_circle_color()
```

#### Works inside widget macros
```rust
#[iced_fluent_icons::inventory]
fn sidebar() -> iced::Element<'_, Message> {{
    iced::widget::column![
        icons::fluent::home_filled(),
        icons::fluent::settings_regular(),
    ].into()
}}
```

### Custom size (default 24 × 24 px)
```rust
#[iced_fluent_icons::inventory(size = 32)]              // 32 × 32
#[iced_fluent_icons::inventory(width = 20, height = 48)] // 20 × 48
#[iced_fluent_icons::inventory(width = 20)]              // 20 × 24
```

---

## Naming convention

SVG filename (UpperCamelCase) → Rust function name (snake_case):

| SVG file                              | Function                            |
|---------------------------------------|-------------------------------------|
| `AddRegular.svg`                      | `add_regular()`                     |
| `DismissCircleColor.svg`              | `dismiss_circle_color()`            |
| `AccessibilityCheckmarkFilled.svg`    | `accessibility_checkmark_filled()`  |
| `ArrowTrendingCheckmarkRegular.svg`   | `arrow_trending_checkmark_regular()`|

---

## Variant guide

| Variant  | fn suffix    | Description                   |
|----------|--------------|-------------------------------|
| Filled   | `_filled()`  | Solid/filled style            |
| Color    | `_color()`   | Multicolour style             |
| Regular  | `_regular()` | Outline/regular style         |
| Light    | `_light()`   | Thinner stroke, lighter style |

---

## Feature flags (short version)
- `no-filled` / `no-color` / `no-regular` / `no-light` — exclude a variant family
- `only-filled` / `only-color` / `only-regular` / `only-light` — keep only a family

Call the **`describe_features`** tool for full details and examples.
"#
  )
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn variant_counts() -> (usize, usize, usize, usize) {
  let mut filled = 0usize;
  let mut color = 0usize;
  let mut regular = 0usize;
  let mut light = 0usize;
  for icon in MCP_ALL_ICONS {
    match icon.variant {
      "Filled" => filled += 1,
      "Color" => color += 1,
      "Regular" => regular += 1,
      "Light" => light += 1,
      _ => {}
    }
  }
  (filled, color, regular, light)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tool schema definitions (returned by tools/list)
// ──────────────────────────────────────────────────────────────────────────────

fn tools_schema() -> Value {
  json!([
      {
          "name": "list_icons",
          "description": concat!(
              "List and search all Fluent UI icons available in the iced-fluent-icons crate. ",
              "Returns icon Rust function names (snake_case), variants, and base names. ",
              "Use this to discover icons and find their exact fn names before writing code. ",
              "Supports filtering by variant (Filled/Color/Regular/Light), full-text search, ",
              "and pagination."
          ),
          "inputSchema": {
              "type": "object",
              "properties": {
                  "variant": {
                      "type": "string",
                      "description": "Only return icons of this variant.",
                      "enum": ["Filled", "Color", "Regular", "Light"]
                  },
                  "search": {
                      "type": "string",
                      "description": "Case-insensitive substring to match against fn_name or base_name."
                  },
                  "offset": {
                      "type": "integer",
                      "description": "Pagination offset (default: 0).",
                      "minimum": 0
                  },
                  "limit": {
                      "type": "integer",
                      "description": "Max results per page (default: 100, max: 500).",
                      "minimum": 1,
                      "maximum": 500
                  }
              }
          }
      },
      {
          "name": "get_icon",
          "description": concat!(
              "Look up a specific icon (or icon family) by name. ",
              "Accepts the base name (e.g. 'Dismiss'), the fn_name (e.g. 'dismiss_filled'), ",
              "or a partial match. Returns all matching variants with their exact Rust fn names ",
              "and short code snippets."
          ),
          "inputSchema": {
              "type": "object",
              "required": ["name"],
              "properties": {
                  "name": {
                      "type": "string",
                      "description": "Icon name to search for (base name, fn_name, or partial)."
                  }
              }
          }
      },
      {
          "name": "describe_features",
          "description": concat!(
              "Describe all Cargo feature flags for iced-fluent-icons. ",
              "Shows which icon variants are included/excluded by each feature flag, ",
              "with exact Cargo.toml examples. Also lists icon counts per variant."
          ),
          "inputSchema": {
              "type": "object",
              "properties": {}
          }
      },
      {
          "name": "get_crate_info",
          "description": concat!(
              "Full overview of the iced-fluent-icons crate: ",
              "what it does, the two-macro workflow (declare!/inventory), ",
              "naming conventions, variant guide, feature flag summary, ",
              "and ready-to-paste code examples."
          ),
          "inputSchema": {
              "type": "object",
              "properties": {}
          }
      }
  ])
}
