use crate::{
    ArchiveManager, Theme,
    browser::{EntryFilters, EntryKind},
};
use gpui::component::{
    Selectable, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui::{Context, Entity, Focusable, Subscription, Window, div, prelude::*, px, rgb};

const LABELS: [&str; 5] = [
    "Extension",
    "Min size (MiB)",
    "Max size (MiB)",
    "Modified from",
    "Modified until",
];
const IDS: [&str; 5] = [
    "filter-extension",
    "filter-min-size",
    "filter-max-size",
    "filter-date-from",
    "filter-date-until",
];

#[derive(Default)]
pub(crate) struct SearchQuery {
    pub text: String,
    pub criteria: EntryFilters,
    pub invalid: bool,
}

pub(crate) fn search_badges(query: &str, commit_last: bool) -> (Vec<String>, String) {
    let Ok(tokens) = shell_words::split(query) else {
        return (Vec::new(), query.to_owned());
    };
    let count = tokens.len();
    let mut badges = Vec::new();
    let mut draft = Vec::new();
    for (index, token) in tokens.into_iter().enumerate() {
        let is_filter = token.split_once(':').is_some_and(|(key, _)| {
            matches!(key.to_ascii_lowercase().as_str(), "ext" | "min" | "max" | "after" | "before" | "type")
        });
        if is_filter && !parse_search(&token).invalid
            && (commit_last || index + 1 < count || query.ends_with(char::is_whitespace))
        {
            badges.push(token);
        } else {
            draft.push(token);
        }
    }
    let mut draft = shell_words::join(draft);
    if !draft.is_empty() && query.ends_with(char::is_whitespace) {
        draft.push(' ');
    }
    (badges, draft)
}

pub(crate) fn compose_search(badges: &[String], draft: &str) -> String {
    let mut query = shell_words::join(badges);
    if !query.is_empty() { query.push(' '); }
    query.push_str(draft);
    query
}

fn parse_bytes(value: &str) -> Result<u64, ()> {
    let value = value.trim().to_ascii_lowercase();
    let units = [
        ("tib", 1_099_511_627_776.0), ("gib", 1_073_741_824.0),
        ("mib", 1_048_576.0), ("kib", 1024.0),
        ("tb", 1_000_000_000_000.0), ("gb", 1_000_000_000.0),
        ("mb", 1_000_000.0), ("kb", 1000.0), ("b", 1.0),
    ];
    let (number, multiplier) = units.iter()
        .find_map(|(unit, multiplier)| value.strip_suffix(unit).map(|number| (number, *multiplier)))
        .unwrap_or((&value, 1.0));
    let bytes = number.replace(',', ".").parse::<f64>().map_err(|_| ())? * multiplier;
    if !bytes.is_finite() || bytes < 0.0 || bytes >= u64::MAX as f64 {
        return Err(());
    }
    Ok(bytes.round() as u64)
}

pub(crate) fn parse_search(value: &str) -> SearchQuery {
    let mut result = SearchQuery::default();
    let Ok(tokens) = shell_words::split(value) else {
        result.invalid = true;
        return result;
    };
    let mut text = Vec::new();
    for token in tokens {
        let Some((key, value)) = token.split_once(':') else {
            text.push(token);
            continue;
        };
        let valid = match key.to_ascii_lowercase().as_str() {
            "ext" => {
                if !result.criteria.extensions.is_empty() {
                    result.criteria.extensions.push(',');
                }
                result.criteria.extensions.push_str(value);
                !value.is_empty()
            }
            "min" | "max" => {
                if let Ok(bytes) = parse_bytes(value) {
                    if key.eq_ignore_ascii_case("min") {
                        result.criteria.minimum_size = Some(result.criteria.minimum_size.unwrap_or(0).max(bytes));
                    } else {
                        result.criteria.maximum_size = Some(result.criteria.maximum_size.unwrap_or(u64::MAX).min(bytes));
                    }
                    true
                } else { false }
            }
            "after" | "before" => {
                if let Ok(Some(date)) = parse_date(value) {
                    if key.eq_ignore_ascii_case("after") {
                        result.criteria.modified_after = Some(result.criteria.modified_after.map_or(date, |old| old.max(date)));
                    } else {
                        result.criteria.modified_before = Some(result.criteria.modified_before.map_or(date, |old| old.min(date)));
                    }
                    true
                } else { false }
            }
            "type" => match value.to_ascii_lowercase().as_str() {
                "file" => { result.criteria.kind = EntryKind::Files; true }
                "folder" => { result.criteria.kind = EntryKind::Folders; true }
                _ => false,
            },
            _ => { text.push(token); continue; }
        };
        result.invalid |= !valid;
    }
    result.text = text.join(" ");
    result.invalid |= matches!((result.criteria.minimum_size, result.criteria.maximum_size), (Some(min), Some(max)) if min > max)
        || matches!((result.criteria.modified_after, result.criteria.modified_before), (Some(after), Some(before)) if after > before);
    result
}

struct Field {
    input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Default)]
pub(crate) struct FilterBar {
    pub visible: bool,
    advanced: bool,
    suggestion_index: usize,
    pub criteria: EntryFilters,
    pub query_invalid: bool,
    source_query: Option<String>,
    fields: Vec<Field>,
    values: [String; 5],
    invalid: [bool; 5],
}

fn parse_size(value: &str) -> Result<Option<u64>, ()> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let number = value
        .trim()
        .replace(',', ".")
        .parse::<f64>()
        .map_err(|_| ())?;
    let bytes = number * 1_048_576.0;
    if !bytes.is_finite() || bytes < 0.0 || bytes >= u64::MAX as f64 {
        return Err(());
    }
    Ok(Some(bytes.round() as u64))
}

fn parse_date(value: &str) -> Result<Option<chrono::NaiveDate>, ()> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ())
    }
}

impl FilterBar {
    fn update(&mut self, index: usize, value: String) {
        self.values[index] = value;
        let minimum = parse_size(&self.values[1]);
        let maximum = parse_size(&self.values[2]);
        let after = parse_date(&self.values[3]);
        let before = parse_date(&self.values[4]);
        self.invalid = [
            false,
            minimum.is_err(),
            maximum.is_err(),
            after.is_err(),
            before.is_err(),
        ];
        if let (Ok(Some(minimum)), Ok(Some(maximum))) = (&minimum, &maximum) {
            if minimum > maximum {
                self.invalid[1] = true;
                self.invalid[2] = true;
            }
        }
        if let (Ok(Some(after)), Ok(Some(before))) = (&after, &before) {
            if after > before {
                self.invalid[3] = true;
                self.invalid[4] = true;
            }
        }
        self.criteria.extensions = self.values[0].clone();
        self.criteria.minimum_size = minimum.unwrap_or(None);
        self.criteria.maximum_size = maximum.unwrap_or(None);
        self.criteria.modified_after = after.unwrap_or(None);
        self.criteria.modified_before = before.unwrap_or(None);
    }

    fn has_values(&self) -> bool {
        self.criteria.is_active() || self.values.iter().any(|value| !value.is_empty())
    }
}

impl ArchiveManager {
    fn accept_search_suggestion(&mut self, token: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut tokens = shell_words::split(&self.toolbar_search.query).unwrap_or_default();
        if !self.toolbar_search.draft.is_empty()
            && !self.toolbar_search.draft.ends_with(char::is_whitespace)
            && tokens.last().and_then(|last| last.split_once(':'))
                .is_some_and(|(key, _)| token.split_once(':').is_some_and(|(new_key, _)| new_key.eq_ignore_ascii_case(key)))
        {
            tokens.pop();
        }
        tokens.push(token.to_owned());
        let mut query = shell_words::join(tokens);
        if !token.ends_with(':') { query.push(' '); }
        self.filters.suggestion_index = 0;
        self.set_search_query(query, window, cx);
        window.focus(&self.toolbar_search.focus_handle(cx), cx);
    }

    fn set_search_filter(&mut self, key: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut tokens = shell_words::split(&self.toolbar_search.query)
            .unwrap_or_else(|_| vec![self.toolbar_search.query.clone()]);
        tokens.retain(|token| !token.split_once(':').is_some_and(|(name, _)| name.eq_ignore_ascii_case(key)));
        if !value.trim().is_empty() {
            tokens.push(format!("{key}:{value}"));
        }
        self.set_search_query(shell_words::join(tokens), window, cx);
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tokens = shell_words::split(&self.toolbar_search.query).unwrap_or_default();
        let text = tokens.into_iter().filter(|token| !token.split_once(':').is_some_and(|(key, _)| {
            matches!(key.to_ascii_lowercase().as_str(), "ext" | "min" | "max" | "after" | "before" | "type")
        }));
        self.set_search_query(shell_words::join(text), window, cx);
    }
}

pub(crate) fn prepare(
    manager: &mut ArchiveManager,
    window: &mut Window,
    cx: &mut Context<ArchiveManager>,
) {
        if manager.filters.fields.is_empty() {
            for index in 0..LABELS.len() {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if index >= 3 { "YYYY-MM-DD" } else { "" })
        });
        let subscription = cx.subscribe_in(&input, window, move |manager, state, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                let value = state.read(cx).value().to_string();
                if manager.filters.values[index] == value { return; }
                manager
                    .filters
                    .update(index, value.clone());
                let key = ["ext", "min", "max", "after", "before"][index];
                let value = if (index == 1 || index == 2) && !value.trim().is_empty() {
                    format!("{}MiB", value.trim())
                } else if index == 0 {
                    value.split([',', ';', ' ']).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(",")
                } else { value };
                manager.set_search_filter(key, &value, window, cx);
            }
        });
        manager.filters.fields.push(Field {
            input,
            _subscription: subscription,
        });
      }
    }
    if manager.filters.source_query.as_ref() == Some(&manager.toolbar_search.query) { return; }
    let parsed = parse_search(&manager.toolbar_search.query);
    manager.filters.suggestion_index = 0;
    manager.filters.values = [
        parsed.criteria.extensions.clone(),
        parsed.criteria.minimum_size.map(|size| (size as f64 / 1_048_576.0).to_string()).unwrap_or_default(),
        parsed.criteria.maximum_size.map(|size| (size as f64 / 1_048_576.0).to_string()).unwrap_or_default(),
        parsed.criteria.modified_after.map(|date| date.to_string()).unwrap_or_default(),
        parsed.criteria.modified_before.map(|date| date.to_string()).unwrap_or_default(),
    ];
    manager.filters.query_invalid = parsed.invalid;
    manager.filters.invalid = [false; 5];
    manager.filters.criteria = parsed.criteria;
    manager.filters.source_query = Some(manager.toolbar_search.query.clone());
    for (index, field) in manager.filters.fields.iter().enumerate() {
        let state = field.input.read(cx);
        if !state.focus_handle(cx).is_focused(window) && state.value().as_str() != manager.filters.values[index] {
            let value = manager.filters.values[index].clone();
            field.input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }
}

pub(crate) fn toggle(
    manager: &ArchiveManager,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let view = cx.entity();
    let focus = manager.toolbar_search.focus_handle(cx);
    gpui::component::popover::Popover::new("unified-search-popover")
        .anchor(gpui::Anchor::TopRight)
        .track_focus(&focus)
        .open(manager.filters.visible)
        .on_open_change(cx.listener(|manager, open: &bool, _, cx| {
            manager.filters.visible = *open;
            cx.notify();
        }))
        .trigger(Button::new("toggle-filters")
        .debug_selector(|| "toggle-filters".to_owned())
        .ghost()
        .small()
        .icon(gpui::assets::IconName::ListFilter)
        .selected(manager.filters.visible || manager.filters.has_values())
        .tooltip(manager.language.text("Filters"))
        .accessibility_label(manager.language.text("Filters"))
        )
        .content(move |_, window, cx| {
            view.update(cx, |manager, cx| {
                let dark = manager.theme_mode.is_dark(window.appearance());
                let theme = Theme::new(dark).with_accent(manager.accent_color.color(dark));
                div().id("unified-search-popup")
                    .debug_selector(|| "unified-search-popup".to_owned())
                    .w((window.viewport_size().width - px(40.0)).min(px(420.0)))
                    .max_h((window.viewport_size().height - px(100.0)).min(px(500.0)))
                    .overflow_y_scroll()
                    .child(
                        div().flex().items_center().justify_between().pb_2()
                            .child(div().text_xs().text_color(rgb(theme.muted_text)).child(manager.language.text("Search filters")))
                            .child(Button::new("search-filter-values").ghost().xsmall()
                                .debug_selector(|| "search-filter-values".to_owned())
                                .icon(gpui::assets::IconName::SlidersHorizontal)
                                .selected(manager.filters.advanced)
                                .tooltip(manager.language.text("Filter values"))
                                .accessibility_label(manager.language.text("Filter values"))
                                .on_click(cx.listener(|manager, _, _, cx| {
                                    manager.filters.advanced = !manager.filters.advanced;
                                    cx.notify();
                                }))),
                    )
                    .when(manager.filters.advanced, |popup| popup.child(self::view(manager, theme, cx)))
                    .when(!manager.filters.advanced, |popup| popup.child(suggestion_list(manager, theme, cx)))
                    .into_any_element()
            })
        })
}

fn suggestions(manager: &ArchiveManager) -> Vec<(String, &'static str)> {
    let tokens = shell_words::split(&manager.toolbar_search.draft).unwrap_or_default();
    let tail = (!manager.toolbar_search.draft.ends_with(char::is_whitespace))
        .then(|| tokens.last().and_then(|token| token.split_once(':'))).flatten();
    if let Some((key, prefix)) = tail {
        let prefix = prefix.to_ascii_lowercase();
        let values: Vec<(String, &'static str)> = match key.to_ascii_lowercase().as_str() {
            "ext" => {
                let mut extensions = std::collections::BTreeSet::new();
                for entry in manager.entries.iter().chain(manager.toolbar_search.matches.values().map(|result| &result.entry)) {
                    if !entry.is_directory {
                        if let Some(extension) = std::path::Path::new(&entry.name).extension() {
                            extensions.insert(extension.to_string_lossy().to_lowercase());
                        }
                    }
                }
                extensions.into_iter().map(|value| (value, "Extension")).collect()
            }
            "min" | "max" => ["1KiB", "1MiB", "10MiB", "100MiB", "1GiB"]
                .into_iter().map(|value| (value.to_owned(), "Size")).collect(),
            "type" => vec![("file".into(), "Files"), ("folder".into(), "Folders")],
            "after" | "before" => {
                let today = chrono::Local::now().date_naive();
                [(0, "Today"), (7, "Last 7 days"), (30, "Last 30 days")]
                    .into_iter().filter_map(|(days, label)| {
                        today.checked_sub_days(chrono::Days::new(days)).map(|date| (date.to_string(), label))
                    }).collect()
            }
            _ => Vec::new(),
        };
        if matches!(key.to_ascii_lowercase().as_str(), "ext" | "min" | "max" | "after" | "before" | "type") {
            return values.into_iter().filter(|(value, _)| value.to_ascii_lowercase().starts_with(&prefix))
                .take(10).map(|(value, label)| (format!("{}:{value}", key.to_ascii_lowercase()), label)).collect();
        }
    }
    [
        ("ext:", "Extension"), ("min:", "Min size (MiB)"), ("max:", "Max size (MiB)"),
        ("after:", "Modified from"), ("before:", "Modified until"), ("type:", "Type"),
    ].into_iter().map(|(token, label)| (token.to_owned(), label)).collect()
}

fn suggestion_list(manager: &ArchiveManager, theme: Theme, cx: &mut Context<ArchiveManager>) -> impl IntoElement {
    div().id("search-suggestions").debug_selector(|| "search-suggestions".to_owned())
        .flex().flex_col().gap_1()
        .children(suggestions(manager).into_iter().enumerate().map(|(index, (token, label))| {
            let id = gpui::SharedString::from(format!("search-suggestion:{token}"));
            Button::new(id).ghost().small().w_full()
                .selected(manager.filters.suggestion_index == index)
                .child(div().flex().w_full().items_center().justify_between().gap_3()
                    .child(div().text_xs().text_color(rgb(theme.text)).child(manager.language.text(label)))
                    .child(div().text_xs().text_color(rgb(theme.muted_text)).child(token.clone())))
                .on_click(cx.listener(move |manager, _, window, cx| manager.accept_search_suggestion(&token, window, cx)))
        }))
        .when(manager.filters.query_invalid, |list| {
            list.child(div().text_xs().text_color(rgb(0xef4444)).p_2()
                .child(manager.language.text("Invalid filter value or range")))
        })
}

pub(crate) fn search_key_down(manager: &mut ArchiveManager, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<ArchiveManager>) {
    if !manager.filters.visible { return; }
    match event.keystroke.key.as_str() {
        "escape" => { manager.filters.visible = false; cx.stop_propagation(); cx.notify(); }
        "up" | "down" if !manager.filters.advanced => {
            let count = suggestions(manager).len();
            if count > 0 {
                let offset = if event.keystroke.key == "up" { count - 1 } else { 1 };
                manager.filters.suggestion_index = (manager.filters.suggestion_index + offset) % count;
                cx.stop_propagation();
                cx.notify();
            }
        }
        "enter" if !manager.filters.advanced => {
            if let Some((token, _)) = suggestions(manager).get(manager.filters.suggestion_index).cloned() {
                manager.accept_search_suggestion(&token, window, cx);
                cx.stop_propagation();
            }
        }
        _ => {}
    }
}

pub(crate) fn view(
    manager: &ArchiveManager,
    theme: Theme,
    cx: &mut Context<ArchiveManager>,
) -> impl IntoElement {
    let language = manager.language;
    div()
        .id("filter-bar")
        .debug_selector(|| "filter-bar".to_owned())
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_2()
        .p_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.sidebar))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div().flex().items_center().gap_1().children(
                        [
                            (EntryKind::All, "All items", "filter-all"),
                            (EntryKind::Files, "Files", "filter-files"),
                            (EntryKind::Folders, "Folders", "filter-folders"),
                        ]
                        .into_iter()
                        .map(|(kind, label, id)| {
                            Button::new(id)
                                .ghost()
                                .xsmall()
                                .debug_selector(move || id.to_owned())
                                .label(language.text(label))
                                .selected(manager.filters.criteria.kind == kind)
                                .on_click(cx.listener(move |manager, _, window, cx| {
                                    let value = match kind { EntryKind::All => "", EntryKind::Files => "file", EntryKind::Folders => "folder" };
                                    manager.set_search_filter("type", value, window, cx);
                                }))
                        }),
                    ),
                )
                .child(
                    Button::new("clear-filters")
                        .ghost()
                        .xsmall()
                        .debug_selector(|| "clear-filters".to_owned())
                        .icon(gpui::assets::IconName::RotateCcw)
                        .tooltip(language.text("Clear filters"))
                        .accessibility_label(language.text("Clear filters"))
                        .on_click(
                            cx.listener(|manager, _, window, cx| manager.clear_filters(window, cx)),
                        ),
                ),
        )
        .child(
            div().flex().flex_wrap().gap_2().children(
                manager
                    .filters
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| {
                        div()
                            .id(("filter-field", index))
                            .debug_selector(move || IDS[index].to_owned())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(130.0))
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme.muted_text))
                                    .child(language.text(LABELS[index])),
                            )
                            .child(
                                Input::new(&field.input)
                                    .id(IDS[index])
                                    .small()
                                    .w_full()
                                    .aria_label(language.text(LABELS[index]))
                                    .when(manager.filters.invalid[index], |input| {
                                        input.border_color(rgb(0xef4444))
                                    }),
                            )
                    }),
            ),
        )
        .when(
            manager.filters.query_invalid || manager.filters.invalid.iter().any(|invalid| *invalid),
            |bar| {
                bar.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xef4444))
                        .child(language.text("Invalid filter value or range")),
                )
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_badges_keep_incomplete_filters_and_free_text_editable() {
        let (badges, draft) = search_badges("annual report ext:zip min:1MiB ", false);
        assert_eq!(badges, ["ext:zip", "min:1MiB"]);
        assert_eq!(draft, "annual report ");
        assert_eq!(compose_search(&badges, &draft), "ext:zip min:1MiB annual report ");
        let (badges, draft) = search_badges("annual report ext:", true);
        assert!(badges.is_empty());
        assert_eq!(draft, "annual report ext:");
        let (badges, draft) = search_badges("ext:zip", false);
        assert!(badges.is_empty());
        assert_eq!(draft, "ext:zip");
        assert_eq!(search_badges("ext:zip", true).0, ["ext:zip"]);
    }

    #[gpui::test]
    fn unified_search_suggestions_support_clicks_keyboard_and_filter_reset(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let (view, cx) = crate::tests::component_window(cx, |_, cx| crate::tests::test_manager(cx));
        let header = cx.debug_bounds("sort-name").unwrap();
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("search-suggestions").is_some());
        assert_eq!(cx.debug_bounds("sort-name").unwrap(), header);
        cx.update(|window, cx| window.click("search-suggestion:ext:", cx));
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).toolbar_search.query, "ext:"));
        cx.update(|window, cx| window.click("search-suggestion:ext:zip", cx));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert_eq!(manager.toolbar_search.query, "ext:zip ");
            assert!(!manager.filters.query_invalid);
        });
        assert!(cx.debug_bounds("file:alpha.zip").is_some());
        assert!(cx.debug_bounds("file:Beta.txt").is_none());
        cx.simulate_input("alpha");
        cx.run_until_parked();
        cx.update(|window, cx| window.click("search-filter-values", cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.click("clear-filters", cx));
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(view.read(cx).toolbar_search.query, "alpha");
            assert!(!view.read(cx).filters.criteria.is_active());
        });
        assert!(cx.debug_bounds("file:alpha.zip").is_some());
        cx.update(|window, cx| window.click("clear-search", cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.click("search-filter-values", cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(view.read(cx).toolbar_search.query, "min:"));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("unified-search-popup").is_none());
    }

    #[test]
    fn unified_search_separates_text_and_combines_filters() {
        let query = parse_search("\"annual report\" ext:PDF ext:zip min:1MiB max:10MB after:2026-01-01 before:2026-10-29 type:file");
        assert_eq!(query.text, "annual report");
        assert_eq!(query.criteria.extensions, "PDF,zip");
        assert_eq!(query.criteria.minimum_size, Some(1_048_576));
        assert_eq!(query.criteria.maximum_size, Some(10_000_000));
        assert!(query.criteria.kind == EntryKind::Files);
        assert_eq!(query.criteria.modified_after, chrono::NaiveDate::from_ymd_opt(2026, 1, 1));
        assert!(!query.invalid);
        assert_eq!(parse_search("report:2026 ext:txt").text, "report:2026");
        for invalid in ["ext:", "min:-1", "max:NaN", "after:2026-02-29", "type:other", "min:2MiB max:1MiB", "\"unfinished"] {
            assert!(parse_search(invalid).invalid, "{invalid}");
        }
    }

    #[gpui::test]
    fn filter_controls_combine_with_search_and_reset(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let (view, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.entries = [
                ("recent.txt", 2 * 1_048_576, Some(date), false),
                ("old.txt", 2 * 1_048_576, date.pred_opt(), false),
                ("small.txt", 20, Some(date), false),
                ("recent.png", 2 * 1_048_576, Some(date), false),
                ("unknown.txt", 2 * 1_048_576, None, false),
                ("folder", 0, None, true),
            ]
            .into_iter()
            .map(
                |(name, size, modified, is_directory)| crate::archive::ArchiveEntry {
                    name: name.into(),
                    source_path: Some(name.into()),
                    size,
                    modified,
                    is_directory,
                },
            )
            .collect();
            manager
        });
        assert!(cx.debug_bounds("filter-bar").is_none());
        cx.update(|window, cx| window.click("toggle-filters", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("search-suggestions").is_some());
        cx.update(|window, cx| window.click("search-filter-values", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("filter-bar").is_some());

        for (field, value) in [
            ("filter-extension", "TXT"),
            ("filter-min-size", "1"),
            ("filter-date-from", "2026-10-29"),
            ("filter-date-until", "2026-10-29"),
        ] {
            cx.update(|window, cx| window.click(field, cx));
            cx.simulate_input(value);
            cx.run_until_parked();
        }
        cx.read(|cx| {
            let query = &view.read(cx).toolbar_search.query;
            assert!(query.contains("ext:TXT"), "{query}");
            assert!(query.contains("min:1MiB"), "{query}");
        });
        assert!(cx.debug_bounds("file:recent.txt").is_some());
        for hidden in [
            "file:old.txt",
            "file:small.txt",
            "file:recent.png",
            "file:unknown.txt",
            "file:folder",
        ] {
            assert!(cx.debug_bounds(hidden).is_none(), "{hidden}");
        }
        cx.update(|window, cx| window.click("toolbar-search-input", cx));
        cx.simulate_input(" old");
        cx.run_until_parked();
        assert!(cx.debug_bounds("file:recent.txt").is_none());
        assert!(cx.debug_bounds("file:old.txt").is_none());
        cx.update(|window, cx| window.click("clear-search", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("file:recent.txt").is_some());
        assert!(cx.debug_bounds("file:recent.png").is_some());
        cx.read(|cx| assert!(!view.read(cx).filters.criteria.is_active()));

        assert!(cx.debug_bounds("filter-bar").is_none());
        assert!(cx.debug_bounds("file:recent.png").is_some());
        cx.update(|window, cx| window.click("toggle-filters", cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.click("clear-filters", cx));
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(!manager.filters.criteria.is_active());
            assert!(manager.filters.values.iter().all(String::is_empty));
            assert!(
                manager
                    .filters
                    .fields
                    .iter()
                    .all(|field| field.input.read(cx).value().is_empty())
            );
        });
        assert!(cx.debug_bounds("file:recent.png").is_some());
        cx.update(|window, cx| window.click("filter-folders", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("file:folder").is_some());
        assert!(cx.debug_bounds("file:recent.txt").is_none());
        cx.update(|window, cx| window.click("filter-files", cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("file:folder").is_none());
        assert!(cx.debug_bounds("file:recent.txt").is_some());
    }

    #[test]
    fn filter_values_validate_sizes_dates_and_ranges() {
        assert_eq!(parse_size("1.5"), Ok(Some(1_572_864)));
        assert_eq!(parse_size("0,5"), Ok(Some(524_288)));
        assert_eq!(parse_size("0"), Ok(Some(0)));
        assert_eq!(parse_size(""), Ok(None));
        for invalid in ["-1", "NaN", "inf", "18446744073709551616", "abc"] {
            assert!(parse_size(invalid).is_err());
        }
        assert!(parse_date("2026-02-29").is_err());
        assert!(parse_date("2026-10-29").is_ok());
        let mut bar = FilterBar::default();
        bar.update(1, "2".into());
        bar.update(2, "1".into());
        assert!(bar.invalid[1] && bar.invalid[2]);
        bar.update(2, "3".into());
        assert!(!bar.invalid[1] && !bar.invalid[2]);
        bar.update(3, "2026-10-29".into());
        bar.update(4, "2026-10-28".into());
        assert!(bar.invalid[3] && bar.invalid[4]);
    }

    #[gpui::test]
    fn filter_bar_wraps_without_overlapping_in_narrow_windows(cx: &mut gpui::TestAppContext) {
        use gpui::test::TestWindowExt;
        let (_, cx) = crate::tests::component_window(cx, |_, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.filters.visible = true;
            manager.filters.advanced = true;
            manager.language = crate::i18n::Language::Polish;
            manager
        });
        for viewport in [
            gpui::size(px(1100.0), px(720.0)),
            gpui::size(px(560.0), px(440.0)),
        ] {
            cx.simulate_resize(viewport);
            cx.run_until_parked();
            let bar = cx.debug_bounds("filter-bar").unwrap();
            let popup = cx.debug_bounds("unified-search-popup").unwrap();
            let status = cx.debug_bounds("status-bar").unwrap();
            assert!(popup.left() >= px(0.0));
            assert!(popup.right() <= viewport.width);
            assert!(bar.left() >= popup.left());
            assert!(bar.right() <= popup.right());
            assert!(bar.bottom() < status.top());
            let fields: Vec<_> = IDS.iter().map(|id| cx.debug_bounds(id).unwrap()).collect();
            for (index, field) in fields.iter().enumerate() {
                assert!(field.left() >= bar.left());
                assert!(field.right() <= bar.right());
                assert!(field.bottom() <= bar.bottom());
                for other in fields.iter().skip(index + 1) {
                    assert!(
                        field.right() <= other.left()
                            || other.right() <= field.left()
                            || field.bottom() <= other.top()
                            || other.bottom() <= field.top()
                    );
                }
            }
            let folders = cx.debug_bounds("filter-folders").unwrap();
            let clear = cx.debug_bounds("clear-filters").unwrap();
            assert!(folders.right() <= clear.left());
        }
        cx.update(|window, cx| window.click("clear-filters", cx));
    }
}
