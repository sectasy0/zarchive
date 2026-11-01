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
    use super::{AccentColor, ThemeMode, UserPreferences};

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
            };
            preferences.save(&path).unwrap();
            assert_eq!(UserPreferences::load(&path), preferences);
        }
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
}
