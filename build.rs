fn main() {
  let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo");
  let icons_dir = std::path::Path::new(&manifest_dir)
    .join("icons")
    .to_string_lossy()
    .replace('\\', "/");

  println!("cargo:rustc-env=FLUENTUI_ICONS_DIR={icons_dir}");

  // Re-run this build script whenever an icon is added or removed.
  println!("cargo:rerun-if-changed=icons");
}
