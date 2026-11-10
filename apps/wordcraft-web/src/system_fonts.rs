//! Ordering for the bounded browser local-font scan.

// PostScript names are stable across locales. Prefer the macOS Japanese families before
// unrelated faces use up the scan's byte budget.
pub(crate) fn is_macos_japanese(postscript_name: &str) -> bool {
    [
        "HiraginoSans-",
        "HiraMin",
        "HiraKaku",
        "HiraMaru",
        "YuGo-",
        "YuMin-",
        "YuMin_",
        "Klee-",
        "Osaka",
        "ToppanBunkyu",
        "TsukuARdGothic-",
        "TsukuBRdGothic-",
    ]
    .iter()
    .any(|prefix| postscript_name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::is_macos_japanese;

    #[test]
    fn macos_japanese_faces_get_budget_before_earlier_fonts() {
        let mut fonts = ["ArialMT", "HiraMinProN-W3", "HiraginoSansGB-W3", "HiraginoSans-W3", "YuGo-Medium", "YuMin-Medium", "WawatiSC-Regular"];
        fonts.sort_by_key(|name| !is_macos_japanese(name));
        assert_eq!(fonts, ["HiraMinProN-W3", "HiraginoSans-W3", "YuGo-Medium", "YuMin-Medium", "ArialMT", "HiraginoSansGB-W3", "WawatiSC-Regular"]);
    }
}
