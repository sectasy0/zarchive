//! Defines persisted appearance settings and their filesystem storage helpers.

use crate::i18n::Language;
use gpui::WindowAppearance;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Selects whether the app follows the system appearance or forces a theme.
pub(crate) enum ThemeMode {
    /// Follows the current operating-system appearance.
    #[default]
    System,
    /// Always uses the light palette.
    Light,
    /// Always uses the dark palette.
    Dark,
}

impl ThemeMode {
    /// Resolves this setting against the current window appearance.
    pub(crate) fn is_dark(self, appearance: WindowAppearance) -> bool {
        match self {
            Self::System => matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Names one of the accent palettes offered in the appearance settings.
pub(crate) enum AccentColor {
    /// Blue accent used by default.
    #[default]
    Blue,
    /// Teal accent.
    Teal,
    /// Green accent.
    Green,
    /// Orange accent.
    Orange,
    /// Rose accent.
    Rose,
}

impl AccentColor {
    /// Lists accent choices in the order shown by the settings UI.
    pub(crate) const ALL: [Self; 5] = [
        Self::Blue,
        Self::Teal,
        Self::Green,
        Self::Orange,
        Self::Rose,
    ];

    /// Returns the stable key used when persisting this accent.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Teal => "teal",
            Self::Green => "green",
            Self::Orange => "orange",
            Self::Rose => "rose",
        }
    }

    /// Returns the untranslated display label for this accent.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Teal => "Teal",
            Self::Green => "Green",
            Self::Orange => "Orange",
            Self::Rose => "Rose",
        }
    }

    /// Parses a persisted accent key, defaulting unknown values to blue.
    fn from_key(key: &str) -> Self {
        match key {
            "teal" => Self::Teal,
            "green" => Self::Green,
            "orange" => Self::Orange,
            "rose" => Self::Rose,
            _ => Self::Blue,
        }
    }

    /// Returns the accent's light- or dark-mode RGB value.
    pub(crate) fn color(self, dark: bool) -> u32 {
        match (self, dark) {
            (Self::Blue, true) => 0x78b7ff,
            (Self::Blue, false) => 0x2563eb,
            (Self::Teal, true) => 0x2dd4bf,
            (Self::Teal, false) => 0x0f766e,
            (Self::Green, true) => 0x4ade80,
            (Self::Green, false) => 0x15803d,
            (Self::Orange, true) => 0xfb923c,
            (Self::Orange, false) => 0xc2410c,
            (Self::Rose, true) => 0xfb7185,
            (Self::Rose, false) => 0xbe123c,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Resource limits that can be adjusted in Advanced settings.
pub(crate) struct AdvancedSettings {
    /// Maximum bytes read for text and binary previews, in KiB.
    pub(crate) preview_limit_kib: u32,
    /// Maximum CSV data rows rendered in the preview table.
    pub(crate) csv_max_rows: u32,
    /// Maximum CSV columns rendered in the preview table.
    pub(crate) csv_max_columns: u32,
    pub(crate) audit_max_depth: u32,
}

impl AdvancedSettings {
    pub(crate) const PREVIEW_LIMIT_RANGE: std::ops::RangeInclusive<u32> = 16..=1024;
    pub(crate) const CSV_MAX_ROWS_RANGE: std::ops::RangeInclusive<u32> = 500..=50_000;
    pub(crate) const CSV_MAX_COLUMNS_RANGE: std::ops::RangeInclusive<u32> = 8..=512;
    pub(crate) const AUDIT_MAX_DEPTH_RANGE: std::ops::RangeInclusive<u32> = 0..=8;

    pub(crate) fn preview_max_bytes(self) -> usize {
        self.preview_limit_kib as usize * 1024
    }

    fn bounded_u32(value: Option<&serde_json::Value>, default: u32, range: std::ops::RangeInclusive<u32>) -> u32 {
        value
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(default)
            .clamp(*range.start(), *range.end())
    }
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            preview_limit_kib: 128,
            csv_max_rows: 5_000,
            csv_max_columns: 64,
            audit_max_depth: 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// User-configurable settings saved between application launches.
pub(crate) struct UserPreferences {
    /// Interface language.
    pub(crate) language: Language,
    /// Theme selection policy.
    pub(crate) theme_mode: ThemeMode,
    /// Selected interface accent.
    pub(crate) accent_color: AccentColor,
    /// Optional startup directory override.
    pub(crate) default_location: Option<PathBuf>,
    pub(crate) auto_calculate_folder_sizes: bool,
    pub(crate) advanced: AdvancedSettings,
    pub(crate) shortcuts: crate::shortcuts::ShortcutSettings,
}

impl Default for UserPreferences {
    /// Returns the app's built-in language, theme, accent, and location defaults.
    fn default() -> Self {
        Self {
            language: Language::default(),
            theme_mode: ThemeMode::default(),
            accent_color: AccentColor::default(),
            default_location: None,
            auto_calculate_folder_sizes: true,
            advanced: AdvancedSettings::default(),
            shortcuts: crate::shortcuts::ShortcutSettings::default(),
        }
    }
}

impl UserPreferences {
    /// Loads settings from JSON, falling back to defaults for missing or invalid data.
    pub(crate) fn load(path: &Path) -> Self {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
            return Self::default();
        };
        Self {
            language: match value.get("language").and_then(serde_json::Value::as_str) {
                Some("pl") => Language::Polish,
                Some("zh-CN") => Language::ChineseSimplified,
                Some("es") => Language::Spanish,
                Some("ja") => Language::Japanese,
                Some("de") => Language::German,
                _ => Language::English,
            },
            theme_mode: match value.get("theme").and_then(serde_json::Value::as_str) {
                Some("light") => ThemeMode::Light,
                Some("dark") => ThemeMode::Dark,
                _ => ThemeMode::System,
            },
            accent_color: AccentColor::from_key(
                value
                    .get("accent")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("blue"),
            ),
            default_location: value
                .get("default_location")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from),
            auto_calculate_folder_sizes: value
                .get("auto_calculate_folder_sizes")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            advanced: {
                let advanced = value.get("advanced");
                AdvancedSettings {
                    preview_limit_kib: AdvancedSettings::bounded_u32(
                        advanced.and_then(|value| value.get("preview_limit_kib")),
                        AdvancedSettings::default().preview_limit_kib,
                        AdvancedSettings::PREVIEW_LIMIT_RANGE,
                    ),
                    csv_max_rows: AdvancedSettings::bounded_u32(
                        advanced.and_then(|value| value.get("csv_max_rows")),
                        AdvancedSettings::default().csv_max_rows,
                        AdvancedSettings::CSV_MAX_ROWS_RANGE,
                    ),
                    csv_max_columns: AdvancedSettings::bounded_u32(
                        advanced.and_then(|value| value.get("csv_max_columns")),
                        AdvancedSettings::default().csv_max_columns,
                        AdvancedSettings::CSV_MAX_COLUMNS_RANGE,
                    ),
                    audit_max_depth: AdvancedSettings::bounded_u32(
                        advanced.and_then(|value| value.get("audit_max_depth")),
                        AdvancedSettings::default().audit_max_depth,
                        AdvancedSettings::AUDIT_MAX_DEPTH_RANGE,
                    ),
                }
            },
            shortcuts: crate::shortcuts::ShortcutSettings::from_json(value.get("shortcuts")),
        }
    }

    /// Creates the parent directory and writes the settings as pretty JSON.
    pub(crate) fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let value = serde_json::json!({
            "language": match self.language {
                Language::English => "en",
                Language::Polish => "pl",
                Language::ChineseSimplified => "zh-CN",
                Language::Japanese => "ja",
                Language::Spanish => "es",
                Language::German => "de"
            },
            "theme": match self.theme_mode {
                ThemeMode::System => "system",
                ThemeMode::Light => "light",
                ThemeMode::Dark => "dark",
            },
            "accent": self.accent_color.key(),
            "default_location": self.default_location.as_ref().map(|path| path.to_string_lossy()),
            "auto_calculate_folder_sizes": self.auto_calculate_folder_sizes,
            "shortcuts": self.shortcuts.json(),
            "advanced": {
                "preview_limit_kib": self.advanced.preview_limit_kib,
                "csv_max_rows": self.advanced.csv_max_rows,
                "csv_max_columns": self.advanced.csv_max_columns,
                "audit_max_depth": self.advanced.audit_max_depth,
            },
        });
        std::fs::write(path, serde_json::to_vec_pretty(&value)?)?;
        Ok(())
    }
}

/// Returns the platform configuration path used for ZArchive preferences.
pub(crate) fn user_preferences_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("zarchive").join("settings.json"))
}

/// Loads preferences from the platform path or returns their defaults.
pub(crate) fn load_user_preferences() -> UserPreferences {
    user_preferences_path()
        .as_deref()
        .map(UserPreferences::load)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{AccentColor, AdvancedSettings, ThemeMode, UserPreferences};

    #[test]
    /// Ensures system appearance follows the OS while explicit modes override it.
    fn system_theme_tracks_appearance_and_explicit_themes_override_it() {
        use gpui::WindowAppearance;

        assert_eq!(ThemeMode::default(), ThemeMode::System);
        for appearance in [
            WindowAppearance::Light,
            WindowAppearance::VibrantLight,
            WindowAppearance::Dark,
            WindowAppearance::VibrantDark,
        ] {
            assert_eq!(
                ThemeMode::System.is_dark(appearance),
                matches!(
                    appearance,
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                )
            );
            assert!(!ThemeMode::Light.is_dark(appearance));
            assert!(ThemeMode::Dark.is_dark(appearance));
        }
    }

    #[test]
    /// Round-trips language, theme, accent, and startup location through JSON.
    fn preferences_round_trip_all_settings() {
        let directory = tempfile::tempdir().unwrap();
        let location = directory.path().join("startup");
        std::fs::create_dir(&location).unwrap();
        let path = directory.path().join("config/settings.json");
        for language in crate::i18n::Language::ALL {
            let preferences = UserPreferences {
                language,
                theme_mode: ThemeMode::Dark,
                accent_color: AccentColor::Rose,
                default_location: Some(location.clone()),
                auto_calculate_folder_sizes: false,
                advanced: AdvancedSettings::default(),
                shortcuts: crate::shortcuts::ShortcutSettings::default(),
            };
            preferences.save(&path).unwrap();
            assert_eq!(UserPreferences::load(&path), preferences);
        }
    }

    #[test]
    fn keyboard_shortcuts_persist_and_invalid_saved_bindings_fall_back() {
        use crate::shortcuts::Command;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        let mut preferences = UserPreferences::default();
        preferences.shortcuts.set(Command::Create, "ctrl-shift-k").unwrap();
        preferences.shortcuts.set(Command::Audit, "").unwrap();
        preferences.save(&path).unwrap();
        assert_eq!(UserPreferences::load(&path), preferences);
        std::fs::write(&path, br#"{"shortcuts":{"create":"escape","preview":"tab","unknown":"ctrl-a"}}"#).unwrap();
        assert_eq!(UserPreferences::load(&path).shortcuts, crate::shortcuts::ShortcutSettings::default());
    }

    #[test]
    fn folder_size_calculation_defaults_to_enabled_for_existing_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        assert!(UserPreferences::load(&path).auto_calculate_folder_sizes);
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        assert!(UserPreferences::load(&path).auto_calculate_folder_sizes);
        for enabled in [false, true] {
            let preferences = UserPreferences {
                auto_calculate_folder_sizes: enabled,
                ..UserPreferences::default()
            };
            preferences.save(&path).unwrap();
            assert_eq!(UserPreferences::load(&path).auto_calculate_folder_sizes, enabled);
        }
    }

    #[test]
    fn security_audit_depth_defaults_and_roundtrips_with_bounds() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        assert_eq!(UserPreferences::load(&path).advanced.audit_max_depth, 2);
        for depth in [0, 2, 8] {
            let mut preferences = UserPreferences::default();
            preferences.advanced.audit_max_depth = depth;
            preferences.save(&path).unwrap();
            assert_eq!(UserPreferences::load(&path).advanced.audit_max_depth, depth);
        }
        std::fs::write(&path, br#"{"advanced":{"audit_max_depth":999}}"#).unwrap();
        assert_eq!(UserPreferences::load(&path).advanced.audit_max_depth, 8);
        std::fs::write(&path, br#"{"advanced":{"audit_max_depth":-1}}"#).unwrap();
        assert_eq!(UserPreferences::load(&path).advanced.audit_max_depth, 2);
    }

    #[test]
    fn advanced_preview_limits_round_trip_and_are_clamped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let advanced = AdvancedSettings {
            preview_limit_kib: 256,
            csv_max_rows: 12_000,
            csv_max_columns: 128,
            audit_max_depth: 4,
        };
        let preferences = UserPreferences {
            advanced,
            ..UserPreferences::default()
        };
        preferences.save(&path).unwrap();
        assert_eq!(UserPreferences::load(&path).advanced, advanced);

        std::fs::write(
            &path,
            r#"{"advanced":{"preview_limit_kib":1,"csv_max_rows":999999,"csv_max_columns":0,"audit_max_depth":999}}"#,
        )
        .unwrap();
        assert_eq!(
            UserPreferences::load(&path).advanced,
            AdvancedSettings {
                preview_limit_kib: *AdvancedSettings::PREVIEW_LIMIT_RANGE.start(),
                csv_max_rows: *AdvancedSettings::CSV_MAX_ROWS_RANGE.end(),
                csv_max_columns: *AdvancedSettings::CSV_MAX_COLUMNS_RANGE.start(),
                audit_max_depth: *AdvancedSettings::AUDIT_MAX_DEPTH_RANGE.end(),
            }
        );
    }
}
