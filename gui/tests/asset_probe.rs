//! Regression guard: every icon used by the GUI must be loadable from the
//! registered asset source. `Assets` only bundles the component subset; the
//! app registers `AllAssets` for the full Lucide catalog.

use gpui_kit::AssetSource;

#[test]
fn ui_icon_assets_load() {
    for path in [
        "icons/film.svg",
        "icons/file-text.svg",
        "icons/type.svg",
        "icons/link.svg",
        "icons/pencil-line.svg",
        "icons/sliders-horizontal.svg",
        "icons/sun.svg",
        "icons/moon.svg",
        "icons/sun-moon.svg",
    ] {
        let loaded = gpui_kit::assets::AllAssets.load(path);
        assert!(
            matches!(&loaded, Ok(Some(bytes)) if !bytes.is_empty()),
            "asset {path} failed to load: {loaded:?}"
        );
    }
}
