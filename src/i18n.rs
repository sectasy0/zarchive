//! Provides localized labels and pluralized item counts for supported languages.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Selects one of the interface languages supported by the application.
pub enum Language {
    /// English interface strings.
    #[default]
    English,
    /// Polish interface strings.
    Polish,
    /// Simplified Chinese interface strings.
    ChineseSimplified,
    /// Spanish interface strings.
    Spanish,
    /// Japanese interface strings.
    Japanese,
    /// German interface strings.
    German,
}

impl Language {
    /// Lists supported languages in selector order.
    pub const ALL: [Self; 6] = [
        Self::English,
        Self::Polish,
        Self::ChineseSimplified,
        Self::Spanish,
        Self::Japanese,
        Self::German,
    ];

    /// Returns the untranslated name shown for this language in settings.
    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Polish => "Polski",
            Self::ChineseSimplified => "简体中文",
            Self::Spanish => "Español",
            Self::Japanese => "日本語",
            Self::German => "Deutsch",
        }
    }

    #[cfg(test)]
    /// Returns the next or previous language, wrapping at either end of the list.
    pub fn adjacent(self, forward: bool) -> Self {
        let index = Self::ALL
            .iter()
            .position(|language| *language == self)
            .expect("language is available in the selector");
        let offset = if forward { 1 } else { Self::ALL.len() - 1 };
        Self::ALL[(index + offset) % Self::ALL.len()]
    }

    /// Looks up a translation row and selects the column for this language.
    fn lookup(self, key: &str) -> Option<&'static str> {
        TRANSLATIONS
            .iter()
            .find_map(|(english, polish, chinese, spanish, japanese, german)| {
                (*english == key).then_some(match self {
                    Self::English => *english,
                    Self::Polish => *polish,
                    Self::ChineseSimplified => *chinese,
                    Self::Spanish => *spanish,
                    Self::Japanese => *japanese,
                    Self::German => *german,
                })
            })
    }

    /// Translates a static interface key, falling back to the key when absent.
    pub fn text(self, key: &'static str) -> &'static str {
        if self == Self::English {
            return key;
        }
        self.lookup(key).unwrap_or(key)
    }

    /// Translates a dynamically borrowed key into an owned string.
    pub fn text_data(self, key: &str) -> String {
        self.lookup(key).unwrap_or(key).to_owned()
    }

    pub fn error_message(self, error: &str) -> &'static str {
        if let Some(row) = TRANSLATIONS
            .iter()
            .find(|row| [row.0, row.1, row.2, row.3, row.4, row.5].contains(&error))
        {
            return self.text(row.0);
        }
        let reason = error.rsplit(": ").next().unwrap_or(error).trim();
        if Language::ALL.into_iter().any(|language| {
            [
                "Failed to add to archive",
                "Failed to move item",
                "Failed to remove entry from archive",
            ]
            .into_iter()
            .any(|key| error.starts_with(language.text(key)))
        }) {
            if let Some(row) = TRANSLATIONS
                .iter()
                .find(|row| [row.0, row.1, row.2, row.3, row.4, row.5].contains(&reason))
            {
                return self.text(row.0);
            }
        }
        if error
            .to_ascii_lowercase()
            .contains("selected sources contain duplicate names:")
        {
            return self.text("An item with this name already exists in the archive");
        }
        let detail = reason.to_ascii_lowercase();
        let contains =
            |fragments: &[&str]| fragments.iter().any(|fragment| detail.contains(fragment));
        let key = if contains(&[
            "permission denied",
            "operation not permitted",
            "access denied",
        ]) {
            "Permission denied. Check your access to the file or folder."
        } else if contains(&["no space left", "disk full", "not enough space"]) {
            "There is not enough free disk space."
        } else if contains(&["no such file", "not found", "does not exist"]) {
            "The file or folder could not be found."
        } else if contains(&["already exists", "would overwrite", "cannot overwrite"]) {
            "The destination already exists. Choose another location."
        } else if contains(&["password", "decrypt", "authentication"]) {
            "The archive could not be unlocked. Check the password and try again."
        } else if contains(&[
            "unsafe",
            "traversal",
            "outside",
            "symbolic link",
            "hard link",
            "symlink",
        ]) {
            "The archive contains unsafe paths or links and cannot be extracted."
        } else if contains(&["unsupported", "not supported", "not implemented"])
            || error
                .to_ascii_lowercase()
                .starts_with("unsupported archive format:")
        {
            "This archive format or compression method is not supported."
        } else if contains(&["cancelled", "canceled"]) {
            "The operation was cancelled."
        } else if contains(&["limit exceeded", "safety limit", "limit reached"]) {
            "Inspection limit reached"
        } else if contains(&[
            "invalid",
            "corrupt",
            "damaged",
            "checksum",
            "crc",
            "unexpected eof",
            "failed to fill whole buffer",
            "end of file",
            "eocd",
        ]) || error
            .to_ascii_lowercase()
            .starts_with("invalid zip archive:")
        {
            if (detail.contains("zip") && !detail.contains("gzip") && !detail.contains("bzip"))
                || error.to_ascii_lowercase().contains("invalid zip archive")
            {
                "The ZIP archive is invalid or damaged."
            } else {
                "The archive is invalid or damaged."
            }
        } else {
            "The operation could not be completed. Check the file and try again."
        };
        self.text(key)
    }

    /// Formats an item count using the plural rules for this language.
    pub fn item_count(self, count: usize) -> String {
        let noun = match self {
            Self::English if count == 1 => "item",
            Self::English => "items",
            Self::Polish if count == 1 => "element",
            Self::Polish
                if (2..=4).contains(&(count % 10)) && !(12..=14).contains(&(count % 100)) =>
            {
                "elementy"
            }
            Self::Polish => "elementów",
            Self::ChineseSimplified => "项",
            Self::Spanish if count == 1 => "elemento",
            Self::Spanish => "elementos",
            Self::Japanese => "個",
            Self::German if count == 1 => "Element",
            Self::German => "Elemente",
        };
        format!("{count} {noun}")
    }
}

const TRANSLATIONS: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "Archives",
        "Archiwa",
        "压缩文件",
        "Archivos comprimidos",
        "アーカイブ",
        "Archive",
    ),
    (
        "Compression streams",
        "Strumienie kompresji",
        "压缩流",
        "Flujos de compresión",
        "圧縮ストリーム",
        "Komprimierungsströme",
    ),
    (
        "Search formats",
        "Szukaj formatów",
        "搜索格式",
        "Buscar formatos",
        "形式を検索",
        "Formate suchen",
    ),
    (
        "Remove from archive",
        "Usuń z archiwum",
        "从压缩文件中删除",
        "Eliminar del archivo comprimido",
        "アーカイブから削除",
        "Aus Archiv entfernen",
    ),
    (
        "Remove this entry from the archive?",
        "Usunąć ten wpis z archiwum?",
        "要从压缩文件中删除此条目吗？",
        "¿Eliminar esta entrada del archivo comprimido?",
        "この項目をアーカイブから削除しますか？",
        "Diesen Eintrag aus dem Archiv entfernen?",
    ),
    (
        "Removing entry from archive...",
        "Usuwanie wpisu z archiwum...",
        "正在从压缩文件中删除条目……",
        "Eliminando entrada del archivo comprimido...",
        "アーカイブから項目を削除中...",
        "Eintrag wird aus dem Archiv entfernt...",
    ),
    (
        "Removed from archive",
        "Usunięto z archiwum",
        "已从压缩文件中删除",
        "Eliminado del archivo comprimido",
        "アーカイブから削除しました",
        "Aus Archiv entfernt",
    ),
    (
        "Failed to remove entry from archive",
        "Nie udało się usunąć wpisu z archiwum",
        "无法从压缩文件中删除条目",
        "No se pudo eliminar la entrada del archivo comprimido",
        "アーカイブから項目を削除できませんでした",
        "Eintrag konnte nicht aus dem Archiv entfernt werden",
    ),
    (
        "Adding files to archive...",
        "Dodawanie plików do archiwum...",
        "正在向压缩文件添加文件……",
        "Añadiendo archivos al archivo comprimido...",
        "アーカイブにファイルを追加中...",
        "Dateien werden zum Archiv hinzugefügt...",
    ),
    (
        "Added to archive",
        "Dodano do archiwum",
        "已添加到压缩文件",
        "Añadido al archivo comprimido",
        "アーカイブに追加しました",
        "Zum Archiv hinzugefügt",
    ),
    (
        "Failed to add to archive",
        "Nie udało się dodać do archiwum",
        "无法添加到压缩文件",
        "No se pudo añadir al archivo comprimido",
        "アーカイブに追加できませんでした",
        "Hinzufügen zum Archiv fehlgeschlagen",
    ),
    (
        "This location cannot accept the dropped files",
        "Nie można dodać plików w tym miejscu",
        "无法将拖放的文件添加到此位置",
        "Esta ubicación no puede aceptar los archivos soltados",
        "この場所にドロップしたファイルを追加できません",
        "Dieser Ort kann die abgelegten Dateien nicht aufnehmen",
    ),
    (
        "This archive format cannot be modified",
        "Ten format archiwum nie może być modyfikowany",
        "无法修改此压缩文件格式",
        "Este formato de archivo comprimido no se puede modificar",
        "このアーカイブ形式は変更できません",
        "Dieses Archivformat kann nicht geändert werden",
    ),
    (
        "Install the rar command-line tool to modify RAR archives",
        "Zainstaluj narzędzie rar, aby modyfikować archiwa RAR",
        "请安装 rar 命令行工具以修改 RAR 压缩文件",
        "Instala la herramienta rar para modificar archivos RAR",
        "RAR アーカイブを変更するには rar コマンドをインストールしてください",
        "Installieren Sie das Kommandozeilenprogramm rar, um RAR-Archive zu ändern",
    ),
    (
        "A single compressed stream cannot contain additional files",
        "Pojedynczy skompresowany strumień nie może zawierać dodatkowych plików",
        "单个压缩流不能包含其他文件",
        "Un flujo comprimido individual no puede contener archivos adicionales",
        "単一の圧縮ストリームに追加のファイルを含めることはできません",
        "Ein einzelner komprimierter Datenstrom kann keine zusätzlichen Dateien enthalten",
    ),
    (
        "The archive contains duplicate names and cannot be rebuilt safely",
        "Archiwum zawiera powtórzone nazwy i nie może zostać bezpiecznie przebudowane",
        "压缩文件包含重复名称，无法安全重建",
        "El archivo comprimido contiene nombres duplicados y no se puede reconstruir de forma segura",
        "アーカイブに重複した名前があるため安全に再構築できません",
        "Das Archiv enthält doppelte Namen und kann nicht sicher neu erstellt werden",
    ),
    (
        "The RAR tool could not update this archive",
        "Narzędzie RAR nie mogło zaktualizować tego archiwum",
        "RAR 工具无法更新此压缩文件",
        "La herramienta RAR no pudo actualizar este archivo comprimido",
        "RAR ツールでこのアーカイブを更新できませんでした",
        "Das RAR-Programm konnte dieses Archiv nicht aktualisieren",
    ),
    (
        "Adding files to encrypted archives is not supported",
        "Dodawanie plików do zaszyfrowanych archiwów nie jest obsługiwane",
        "不支持向加密压缩文件添加文件",
        "No se admite añadir archivos a archivos comprimidos cifrados",
        "暗号化されたアーカイブへのファイル追加には対応していません",
        "Das Hinzufügen zu verschlüsselten Archiven wird nicht unterstützt",
    ),
    (
        "An archive cannot contain itself",
        "Archiwum nie może zawierać samego siebie",
        "压缩文件不能包含自身",
        "Un archivo comprimido no puede contenerse a sí mismo",
        "アーカイブに自身を含めることはできません",
        "Ein Archiv kann sich nicht selbst enthalten",
    ),
    (
        "The archive changed while files were being added",
        "Archiwum zmieniło się podczas dodawania plików",
        "添加文件时压缩文件发生了变化",
        "El archivo comprimido cambió mientras se añadían archivos",
        "ファイルの追加中にアーカイブが変更されました",
        "Das Archiv wurde während des Hinzufügens geändert",
    ),
    (
        "An item with this name already exists in the archive",
        "Element o tej nazwie już istnieje w archiwum",
        "压缩文件中已存在同名项目",
        "Ya existe un elemento con este nombre en el archivo comprimido",
        "同じ名前の項目がアーカイブに既に存在します",
        "Ein Element mit diesem Namen existiert bereits im Archiv",
    ),
    (
        "CSV preview",
        "Podgląd CSV",
        "CSV 表格预览",
        "Vista previa de CSV",
        "CSV プレビュー",
        "CSV-Vorschau",
    ),
    (
        "Text preview",
        "Podgląd tekstu",
        "文本预览",
        "Vista previa de texto",
        "テキストプレビュー",
        "Textvorschau",
    ),
    (
        "Loading text preview...",
        "Wczytywanie podglądu tekstu...",
        "正在加载文本预览……",
        "Cargando vista previa de texto...",
        "テキストプレビューを読み込み中...",
        "Textvorschau wird geladen...",
    ),
    (
        "Text preview is unavailable for this file",
        "Podgląd tekstu jest niedostępny dla tego pliku",
        "此文件不支持文本预览",
        "La vista previa de texto no está disponible para este archivo",
        "このファイルのテキストプレビューは利用できません",
        "Für diese Datei ist keine Textvorschau verfügbar",
    ),
    (
        "This file is empty",
        "Ten plik jest pusty",
        "此文件为空",
        "Este archivo está vacío",
        "このファイルは空です",
        "Diese Datei ist leer",
    ),
    (
        "Copy text",
        "Kopiuj tekst",
        "复制文本",
        "Copiar texto",
        "テキストをコピー",
        "Text kopieren",
    ),
    (
        "Preview truncated to keep the application responsive",
        "Podgląd został skrócony, aby zachować płynność aplikacji",
        "预览已截断以保持应用流畅",
        "La vista previa se ha recortado para mantener la aplicación ágil",
        "アプリの応答性を維持するためプレビューを短縮しました",
        "Die Vorschau wurde gekürzt, damit die Anwendung reaktionsfähig bleibt",
    ),
    (
        "Automatically calculate folder sizes",
        "Automatycznie obliczaj rozmiary folderów",
        "自动计算文件夹大小",
        "Calcular automáticamente el tamaño de las carpetas",
        "フォルダーのサイズを自動的に計算",
        "Ordnergrößen automatisch berechnen",
    ),
    (
        "Remove filter",
        "Usuń filtr",
        "移除筛选",
        "Eliminar filtro",
        "フィルターを削除",
        "Filter entfernen",
    ),
    (
        "Search filters",
        "Filtry wyszukiwania",
        "搜索筛选",
        "Filtros de búsqueda",
        "検索フィルター",
        "Suchfilter",
    ),
    (
        "Filter values",
        "Wartości filtrów",
        "筛选值",
        "Valores de filtro",
        "フィルターの値",
        "Filterwerte",
    ),
    ("Today", "Dzisiaj", "今天", "Hoy", "今日", "Heute"),
    (
        "Last 7 days",
        "Ostatnie 7 dni",
        "最近7天",
        "Últimos 7 días",
        "過去7日間",
        "Letzte 7 Tage",
    ),
    (
        "Last 30 days",
        "Ostatnie 30 dni",
        "最近30天",
        "Últimos 30 días",
        "過去30日間",
        "Letzte 30 Tage",
    ),
    (
        "Filters",
        "Filtry",
        "筛选",
        "Filtros",
        "フィルター",
        "Filter",
    ),
    (
        "Extension",
        "Rozszerzenie",
        "扩展名",
        "Extensión",
        "拡張子",
        "Dateiendung",
    ),
    (
        "Min size (MiB)",
        "Rozmiar od (MiB)",
        "最小大小 (MiB)",
        "Tamaño mín. (MiB)",
        "最小サイズ (MiB)",
        "Min. Größe (MiB)",
    ),
    (
        "Max size (MiB)",
        "Rozmiar do (MiB)",
        "最大大小 (MiB)",
        "Tamaño máx. (MiB)",
        "最大サイズ (MiB)",
        "Max. Größe (MiB)",
    ),
    (
        "Modified from",
        "Modyfikacja od",
        "修改日期起",
        "Modificado desde",
        "更新日（開始）",
        "Geändert ab",
    ),
    (
        "Modified until",
        "Modyfikacja do",
        "修改日期止",
        "Modificado hasta",
        "更新日（終了）",
        "Geändert bis",
    ),
    (
        "All items",
        "Wszystkie",
        "所有项目",
        "Todos",
        "すべて",
        "Alle",
    ),
    (
        "Clear filters",
        "Wyczyść filtry",
        "清除筛选",
        "Borrar filtros",
        "フィルターをクリア",
        "Filter zurücksetzen",
    ),
    (
        "Invalid filter value or range",
        "Nieprawidłowa wartość lub zakres filtra",
        "筛选值或范围无效",
        "Valor o rango de filtro no válido",
        "フィルターの値または範囲が無効です",
        "Ungültiger Filterwert oder Bereich",
    ),
    (
        "No items match the current filters",
        "Brak elementów spełniających wybrane filtry",
        "没有项目符合当前筛选条件",
        "Ningún elemento coincide con los filtros actuales",
        "現在のフィルターに一致する項目がありません",
        "Keine Elemente entsprechen den aktuellen Filtern",
    ),
    (
        "Permission denied. Check your access to the file or folder.",
        "Brak uprawnień. Sprawdź dostęp do pliku lub folderu.",
        "权限不足。请检查文件或文件夹的访问权限。",
        "Permiso denegado. Comprueba el acceso al archivo o la carpeta.",
        "アクセスが拒否されました。ファイルまたはフォルダーの権限を確認してください。",
        "Zugriff verweigert. Prüfen Sie die Zugriffsrechte für die Datei oder den Ordner.",
    ),
    (
        "There is not enough free disk space.",
        "Brak wystarczającej ilości wolnego miejsca na dysku.",
        "磁盘可用空间不足。",
        "No hay suficiente espacio libre en el disco.",
        "ディスクの空き容量が不足しています。",
        "Nicht genügend freier Speicherplatz auf dem Datenträger.",
    ),
    (
        "The file or folder could not be found.",
        "Nie znaleziono pliku lub folderu.",
        "找不到文件或文件夹。",
        "No se encontró el archivo o la carpeta.",
        "ファイルまたはフォルダーが見つかりません。",
        "Die Datei oder der Ordner wurde nicht gefunden.",
    ),
    (
        "The destination already exists. Choose another location.",
        "Element docelowy już istnieje. Wybierz inną lokalizację.",
        "目标已存在。请选择其他位置。",
        "El destino ya existe. Elige otra ubicación.",
        "保存先は既に存在します。別の場所を選択してください。",
        "Das Ziel existiert bereits. Wählen Sie einen anderen Speicherort.",
    ),
    (
        "The archive could not be unlocked. Check the password and try again.",
        "Nie udało się odblokować archiwum. Sprawdź hasło i spróbuj ponownie.",
        "无法解锁压缩文件。请检查密码后重试。",
        "No se pudo desbloquear el archivo comprimido. Comprueba la contraseña e inténtalo de nuevo.",
        "アーカイブを解除できませんでした。パスワードを確認して再試行してください。",
        "Das Archiv konnte nicht entsperrt werden. Prüfen Sie das Passwort und versuchen Sie es erneut.",
    ),
    (
        "The archive contains unsafe paths or links and cannot be extracted.",
        "Archiwum zawiera niebezpieczne ścieżki lub dowiązania i nie może zostać rozpakowane.",
        "压缩文件包含不安全的路径或链接，无法解压。",
        "El archivo comprimido contiene rutas o enlaces no seguros y no se puede extraer.",
        "アーカイブに安全でないパスまたはリンクが含まれているため、展開できません。",
        "Das Archiv enthält unsichere Pfade oder Verknüpfungen und kann nicht entpackt werden.",
    ),
    (
        "This archive format or compression method is not supported.",
        "Ten format archiwum lub metoda kompresji nie są obsługiwane.",
        "不支持此压缩文件格式或压缩方法。",
        "Este formato de archivo comprimido o método de compresión no es compatible.",
        "このアーカイブ形式または圧縮方式には対応していません。",
        "Dieses Archivformat oder Kompressionsverfahren wird nicht unterstützt.",
    ),
    (
        "The operation was cancelled.",
        "Operacja została anulowana.",
        "操作已取消。",
        "La operación se ha cancelado.",
        "操作はキャンセルされました。",
        "Der Vorgang wurde abgebrochen.",
    ),
    (
        "The ZIP archive is invalid or damaged.",
        "Archiwum ZIP jest nieprawidłowe lub uszkodzone.",
        "ZIP 压缩文件无效或已损坏。",
        "El archivo ZIP no es válido o está dañado.",
        "ZIP アーカイブが無効であるか破損しています。",
        "Das ZIP-Archiv ist ungültig oder beschädigt.",
    ),
    (
        "The archive is invalid or damaged.",
        "Archiwum jest nieprawidłowe lub uszkodzone.",
        "压缩文件无效或已损坏。",
        "El archivo comprimido no es válido o está dañado.",
        "アーカイブが無効であるか破損しています。",
        "Das Archiv ist ungültig oder beschädigt.",
    ),
    (
        "The operation could not be completed. Check the file and try again.",
        "Nie udało się ukończyć operacji. Sprawdź plik i spróbuj ponownie.",
        "无法完成操作。请检查文件后重试。",
        "No se pudo completar la operación. Comprueba el archivo e inténtalo de nuevo.",
        "操作を完了できませんでした。ファイルを確認して再試行してください。",
        "Der Vorgang konnte nicht abgeschlossen werden. Prüfen Sie die Datei und versuchen Sie es erneut.",
    ),
    (
        "Notifications",
        "Powiadomienia",
        "通知",
        "Notificaciones",
        "通知",
        "Benachrichtigungen",
    ),
    (
        "Toggle notifications",
        "Pokaż lub ukryj powiadomienia",
        "显示或隐藏通知",
        "Mostrar u ocultar notificaciones",
        "通知の表示を切り替え",
        "Benachrichtigungen ein-/ausblenden",
    ),
    (
        "Background tasks",
        "Zadania w tle",
        "后台任务",
        "Tareas en segundo plano",
        "バックグラウンドタスク",
        "Hintergrundaufgaben",
    ),
    (
        "No background tasks",
        "Brak zadań w tle",
        "没有后台任务",
        "No hay tareas en segundo plano",
        "バックグラウンドタスクはありません",
        "Keine Hintergrundaufgaben",
    ),
    ("Queued", "W kolejce", "排队中", "En cola", "待機中", "In Warteschlange"),
    ("Running", "W toku", "运行中", "En curso", "実行中", "Wird ausgeführt"),
    ("Paused", "Wstrzymane", "已暂停", "En pausa", "一時停止", "Pausiert"),
    ("Completed", "Zakończone", "已完成", "Completada", "完了", "Abgeschlossen"),
    ("Failed", "Niepowodzenie", "失败", "Fallida", "失敗", "Fehlgeschlagen"),
    ("Cancelled", "Anulowane", "已取消", "Cancelada", "キャンセル済み", "Abgebrochen"),
    ("Cancelling...", "Anulowanie...", "正在取消...", "Cancelando...", "キャンセル中...", "Wird abgebrochen..."),
    ("Opening archive...", "Otwieranie archiwum...", "正在打开压缩包...", "Abriendo archivo comprimido...", "アーカイブを開いています...", "Archiv wird geöffnet..."),
    ("Opening file...", "Otwieranie pliku...", "正在打开文件...", "Abriendo archivo...", "ファイルを開いています...", "Datei wird geöffnet..."),
    ("Pause task", "Wstrzymaj zadanie", "暂停任务", "Pausar tarea", "タスクを一時停止", "Aufgabe pausieren"),
    ("Resume task", "Wznów zadanie", "继续任务", "Reanudar tarea", "タスクを再開", "Aufgabe fortsetzen"),
    ("Cancel task", "Anuluj zadanie", "取消任务", "Cancelar tarea", "タスクをキャンセル", "Aufgabe abbrechen"),
    (
        "Clear completed tasks",
        "Wyczyść zakończone zadania",
        "清除已完成的任务",
        "Borrar tareas completadas",
        "完了したタスクを消去",
        "Abgeschlossene Aufgaben löschen",
    ),
    (
        "Hide notifications",
        "Ukryj powiadomienia",
        "隐藏通知",
        "Ocultar notificaciones",
        "通知を非表示",
        "Benachrichtigungen ausblenden",
    ),
    ("Errors", "Błędy", "错误", "Errores", "エラー", "Fehler"),
    (
        "Warnings",
        "Ostrzeżenia",
        "警告",
        "Advertencias",
        "警告",
        "Warnungen",
    ),
    (
        "Inspector",
        "Inspektor",
        "检查器",
        "Inspector",
        "インスペクター",
        "Inspektor",
    ),
    (
        "Test integrity",
        "Test integralności",
        "完整性测试",
        "Probar integridad",
        "整合性をテスト",
        "Integrität testen",
    ),
    (
        "Audit archives",
        "Audyt archiwów",
        "审核压缩文件",
        "Auditar archivos comprimidos",
        "アーカイブを監査",
        "Archive auditieren",
    ),
    (
        "Audit archive",
        "Audyt archiwum",
        "审核压缩文件",
        "Auditar archivo comprimido",
        "アーカイブを監査",
        "Archiv auditieren",
    ),
    (
        "Full audit",
        "Pełny audyt",
        "完整审核",
        "Auditoría completa",
        "完全監査",
        "Vollständiges Audit",
    ),
    (
        "Compare archives",
        "Porównaj archiwa",
        "比较压缩文件",
        "Comparar archivos comprimidos",
        "アーカイブを比較",
        "Archive vergleichen",
    ),
    ("Comparison", "Porównanie", "比较", "Comparación", "比較", "Vergleich"),
    (
        "Comparing archives...",
        "Porównywanie archiwów...",
        "正在比较压缩文件...",
        "Comparando archivos comprimidos...",
        "アーカイブを比較しています...",
        "Archive werden verglichen...",
    ),
    (
        "Could not compare archives",
        "Nie udało się porównać archiwów",
        "无法比较压缩文件",
        "No se pudieron comparar los archivos comprimidos",
        "アーカイブを比較できませんでした",
        "Archive konnten nicht verglichen werden",
    ),
    (
        "Select exactly two archives to compare",
        "Wybierz dokładnie dwa archiwa do porównania",
        "请选择两个压缩文件进行比较",
        "Selecciona exactamente dos archivos comprimidos para comparar",
        "比較するアーカイブを2つ選択してください",
        "Wählen Sie genau zwei Archive zum Vergleichen aus",
    ),
    (
        "Choose two different archives",
        "Wybierz dwa różne archiwa",
        "请选择两个不同的压缩文件",
        "Elige dos archivos comprimidos distintos",
        "異なる2つのアーカイブを選択してください",
        "Wählen Sie zwei unterschiedliche Archive aus",
    ),
    (
        "Both files must be supported archives",
        "Oba pliki muszą być obsługiwanymi archiwami",
        "两个文件都必须是支持的压缩文件",
        "Ambos archivos deben ser archivos comprimidos compatibles",
        "両方のファイルがサポート対象のアーカイブである必要があります",
        "Beide Dateien müssen unterstützte Archive sein",
    ),
    (
        "Save comparison report",
        "Zapisz raport porównania",
        "保存比较报告",
        "Guardar informe de comparación",
        "比較レポートを保存",
        "Vergleichsbericht speichern",
    ),
    (
        "Comparison complete",
        "Porównanie ukończone",
        "比较已完成",
        "Comparación completa",
        "比較完了",
        "Vergleich abgeschlossen",
    ),
    (
        "Comparison incomplete",
        "Porównanie niepełne",
        "比较未完成",
        "Comparación incompleta",
        "比較が未完了です",
        "Vergleich unvollständig",
    ),
    (
        "First different byte",
        "Pierwszy różny bajt",
        "首个不同字节",
        "Primer byte diferente",
        "最初に異なるバイト",
        "Erstes abweichendes Byte",
    ),
    (
        "Text diff preview",
        "Podgląd różnic tekstu",
        "文本差异预览",
        "Vista previa de diferencias de texto",
        "テキスト差分のプレビュー",
        "Textdiff-Vorschau",
    ),
    (
        "Semantic diff preview",
        "Podgląd różnic semantycznych",
        "语义差异预览",
        "Vista previa de diferencias semánticas",
        "意味差分のプレビュー",
        "Vorschau des semantischen Diffs",
    ),
    (
        "Semantic analysis limit reached",
        "Osiągnięto limit analizy semantycznej",
        "已达到语义分析限制",
        "Se alcanzó el límite del análisis semántico",
        "意味解析の上限に達しました",
        "Limit der semantischen Analyse erreicht",
    ),
    (
        "Text diff limit reached",
        "Osiągnięto limit różnic tekstowych",
        "已达到文本差异限制",
        "Se alcanzó el límite de diferencias de texto",
        "テキスト差分の上限に達しました",
        "Limit für Textunterschiede erreicht",
    ),
    (
        "Text diff shortened",
        "Podgląd różnic tekstu skrócono",
        "文本差异预览已缩短",
        "Vista previa de diferencias de texto abreviada",
        "テキスト差分のプレビューを短縮しました",
        "Textdiff-Vorschau gekürzt",
    ),
    (
        "Semantic diff shortened",
        "Podgląd różnic semantycznych skrócono",
        "语义差异预览已缩短",
        "Vista previa de diferencias semánticas abreviada",
        "意味差分のプレビューを短縮しました",
        "Vorschau des semantischen Diffs gekürzt",
    ),
    (
        "Semantic parser error",
        "Błąd parsera semantycznego",
        "语义解析器错误",
        "Error del analizador semántico",
        "意味解析エラー",
        "Fehler des semantischen Parsers",
    ),
    ("SHA-256 hashes", "Skróty SHA-256", "SHA-256 哈希", "Hashes SHA-256", "SHA-256 ハッシュ", "SHA-256-Hashes"),
    ("SHA-512 hashes", "Skróty SHA-512", "SHA-512 哈希", "Hashes SHA-512", "SHA-512 ハッシュ", "SHA-512-Hashes"),
    ("BLAKE3 hashes", "Skróty BLAKE3", "BLAKE3 哈希", "Hashes BLAKE3", "BLAKE3 ハッシュ", "BLAKE3-Hashes"),
    ("Image metadata A", "Metadane obrazu A", "图像元数据 A", "Metadatos de imagen A", "画像メタデータ A", "Bildmetadaten A"),
    ("Image metadata B", "Metadane obrazu B", "图像元数据 B", "Metadatos de imagen B", "画像メタデータ B", "Bildmetadaten B"),
    ("Comparison error A", "Błąd porównania A", "比较错误 A", "Error de comparación A", "比較エラー A", "Vergleichsfehler A"),
    ("Comparison error B", "Błąd porównania B", "比较错误 B", "Error de comparación B", "比較エラー B", "Vergleichsfehler B"),
    (
        "Metadata changed",
        "Zmienione metadane",
        "元数据已更改",
        "Metadatos modificados",
        "メタデータが変更されました",
        "Metadaten geändert",
    ),
    (
        "Modification time",
        "Czas modyfikacji",
        "修改时间",
        "Hora de modificación",
        "変更日時",
        "Änderungszeit",
    ),
    (
        "Creation time",
        "Czas utworzenia",
        "创建时间",
        "Hora de creación",
        "作成日時",
        "Erstellungszeit",
    ),
    (
        "Nested archives analyzed",
        "Przeanalizowane archiwa zagnieżdżone",
        "已分析的嵌套压缩文件",
        "Archivos comprimidos anidados analizados",
        "分析した入れ子のアーカイブ",
        "Analysierte verschachtelte Archive",
    ),
    (
        "Some files could not be compared",
        "Nie udało się porównać części plików",
        "部分文件无法比较",
        "No se pudieron comparar algunos archivos",
        "一部のファイルを比較できませんでした",
        "Einige Dateien konnten nicht verglichen werden",
    ),
    ("All", "Wszystkie", "全部", "Todos", "すべて", "Alle"),
    ("Differences", "Różnice", "差异", "Diferencias", "差分", "Unterschiede"),
    ("Content", "Zawartość", "内容", "Contenido", "内容", "Inhalt"),
    ("Metadata", "Metadane", "元数据", "Metadatos", "メタデータ", "Metadaten"),
    (
        "No comparison results",
        "Brak wyników porównania",
        "没有比较结果",
        "No hay resultados de comparación",
        "比較結果はありません",
        "Keine Vergleichsergebnisse",
    ),
    (
        "Semantically equal",
        "Równoważne semantycznie",
        "语义相同",
        "Semánticamente equivalentes",
        "意味的に同等",
        "Semantisch gleich",
    ),
    (
        "Semantically different",
        "Różne semantycznie",
        "语义不同",
        "Semánticamente diferentes",
        "意味的に異なる",
        "Semantisch unterschiedlich",
    ),
    (
        "Byte-identical",
        "Identyczne bajt po bajcie",
        "字节完全相同",
        "Idénticos byte a byte",
        "バイト単位で同一",
        "Byte-identisch",
    ),
    (
        "Binary content differs",
        "Zawartość binarna różni się",
        "二进制内容不同",
        "El contenido binario es diferente",
        "バイナリ内容が異なります",
        "Binärinhalt unterscheidet sich",
    ),
    (
        "Only in archive A",
        "Tylko w archiwum A",
        "仅在压缩文件 A 中",
        "Solo en el archivo comprimido A",
        "アーカイブ A のみ",
        "Nur in Archiv A",
    ),
    (
        "Only in archive B",
        "Tylko w archiwum B",
        "仅在压缩文件 B 中",
        "Solo en el archivo comprimido B",
        "アーカイブ B のみ",
        "Nur in Archiv B",
    ),
    ("Common file", "Wspólny plik", "共有文件", "Archivo común", "共通ファイル", "Gemeinsame Datei"),
    (
        "Content changed",
        "Zmieniona zawartość",
        "内容已更改",
        "Contenido modificado",
        "内容が変更されました",
        "Inhalt geändert",
    ),
    (
        "File type changed",
        "Zmieniony typ wpisu",
        "条目类型已更改",
        "Tipo de archivo modificado",
        "ファイルの種類が変更されました",
        "Dateityp geändert",
    ),
    ("Moved", "Przeniesiony", "已移动", "Movido", "移動", "Verschoben"),
    ("Renamed", "Zmieniona nazwa", "已重命名", "Renombrado", "名前変更", "Umbenannt"),
    (
        "Moved and renamed",
        "Przeniesiony i przemianowany",
        "已移动并重命名",
        "Movido y renombrado",
        "移動および名前変更",
        "Verschoben und umbenannt",
    ),
    (
        "Directory only in archive A",
        "Katalog tylko w archiwum A",
        "仅在压缩文件 A 中的目录",
        "Directorio solo en el archivo comprimido A",
        "アーカイブ A のみのフォルダー",
        "Verzeichnis nur in Archiv A",
    ),
    (
        "Directory only in archive B",
        "Katalog tylko w archiwum B",
        "仅在压缩文件 B 中的目录",
        "Directorio solo en el archivo comprimido B",
        "アーカイブ B のみのフォルダー",
        "Verzeichnis nur in Archiv B",
    ),
    (
        "Empty directory only in archive A",
        "Pusty katalog tylko w archiwum A",
        "仅在压缩文件 A 中的空目录",
        "Directorio vacío solo en el archivo comprimido A",
        "アーカイブ A のみの空フォルダー",
        "Leeres Verzeichnis nur in Archiv A",
    ),
    (
        "Empty directory only in archive B",
        "Pusty katalog tylko w archiwum B",
        "仅在压缩文件 B 中的空目录",
        "Directorio vacío solo en el archivo comprimido B",
        "アーカイブ B のみの空フォルダー",
        "Leeres Verzeichnis nur in Archiv B",
    ),
    ("File size", "Rozmiar pliku", "文件大小", "Tamaño del archivo", "ファイルサイズ", "Dateigröße"),
    (
        "Permissions and attributes",
        "Uprawnienia i atrybuty",
        "权限和属性",
        "Permisos y atributos",
        "権限と属性",
        "Berechtigungen und Attribute",
    ),
    ("Owner ID", "Identyfikator właściciela", "所有者 ID", "ID de propietario", "所有者 ID", "Eigentümer-ID"),
    ("Group ID", "Identyfikator grupy", "组 ID", "ID de grupo", "グループ ID", "Gruppen-ID"),
    ("File comment", "Komentarz pliku", "文件注释", "Comentario del archivo", "ファイルコメント", "Dateikommentar"),
    ("Extra fields", "Pola dodatkowe", "扩展字段", "Campos adicionales", "追加フィールド", "Zusatzfelder"),
    ("Link target", "Cel łącza", "链接目标", "Destino del enlace", "リンク先", "Linkziel"),
    ("Stored CRC-32", "Zapisane CRC-32", "存储的 CRC-32", "CRC-32 almacenado", "保存済み CRC-32", "Gespeicherte CRC-32"),
    ("Computed CRC-32", "Obliczone CRC-32", "计算的 CRC-32", "CRC-32 calculado", "計算した CRC-32", "Berechnete CRC-32"),
    ("Symbolic or hard link", "Łącze symboliczne lub twarde", "符号链接或硬链接", "Enlace simbólico o duro", "シンボリックリンクまたはハードリンク", "Symbolischer oder harter Link"),
    ("Entry integrity", "Integralność wpisu", "条目完整性", "Integridad de la entrada", "エントリの整合性", "Eintragsintegrität"),
    ("Archive integrity", "Integralność archiwum", "压缩文件完整性", "Integridad del archivo comprimido", "アーカイブの整合性", "Archivintegrität"),
    ("Archive SHA-256", "SHA-256 archiwum", "压缩文件 SHA-256", "SHA-256 del archivo comprimido", "アーカイブ SHA-256", "Archiv-SHA-256"),
    ("Image metadata", "Metadane obrazu", "图像元数据", "Metadatos de imagen", "画像メタデータ", "Bildmetadaten"),
    (
        "Dropped item is unavailable",
        "Upuszczony element jest niedostępny",
        "拖放项目不可用",
        "El elemento soltado no está disponible",
        "ドロップした項目は利用できません",
        "Das abgelegte Element ist nicht verfügbar",
    ),
    (
        "Overview",
        "Przegląd",
        "概览",
        "Resumen",
        "概要",
        "Übersicht",
    ),
    (
        "Entries",
        "Zawartość",
        "条目",
        "Entradas",
        "エントリ",
        "Einträge",
    ),
    (
        "Audit findings",
        "Wyniki audytu",
        "审核结果",
        "Resultados de la auditoría",
        "監査結果",
        "Audit-Ergebnisse",
    ),
    (
        "Previous archive",
        "Poprzednie archiwum",
        "上一个压缩文件",
        "Archivo comprimido anterior",
        "前のアーカイブ",
        "Vorheriges Archiv",
    ),
    (
        "Next archive",
        "Następne archiwum",
        "下一个压缩文件",
        "Archivo comprimido siguiente",
        "次のアーカイブ",
        "Nächstes Archiv",
    ),
    (
        "Inspecting archive...",
        "Sprawdzanie archiwum...",
        "正在检查压缩文件……",
        "Inspeccionando archivo comprimido...",
        "アーカイブを検査しています...",
        "Archiv wird untersucht...",
    ),
    (
        "Only the first 32 archives were inspected",
        "Sprawdzono tylko pierwsze 32 archiwa",
        "仅检查了前 32 个压缩文件",
        "Solo se inspeccionaron los primeros 32 archivos comprimidos",
        "最初の32個のアーカイブのみ検査しました",
        "Nur die ersten 32 Archive wurden untersucht",
    ),
    (
        "Inspection limit details",
        "Szczegóły limitu inspekcji",
        "检查限制详情",
        "Detalles del límite de inspección",
        "検査制限の詳細",
        "Details zum Untersuchungslimit",
    ),
    (
        "Only the first 32 archives were checked. Run the audit again for the remaining archives.",
        "Sprawdzono tylko pierwsze 32 archiwa. Uruchom audyt ponownie dla pozostałych archiwów.",
        "仅检查了前 32 个压缩文件。请对其余压缩文件重新运行审核。",
        "Solo se comprobaron los primeros 32 archivos comprimidos. Vuelve a ejecutar la auditoría para los demás.",
        "最初の32個のアーカイブのみ検査しました。残りのアーカイブについては、監査をもう一度実行してください。",
        "Nur die ersten 32 Archive wurden geprüft. Führen Sie das Audit für die übrigen Archive erneut aus.",
    ),
    (
        "The audit reached a safety limit while reading this archive. Some data may not have been checked, so the findings can be incomplete.",
        "Podczas odczytu archiwum osiągnięto limit bezpieczeństwa. Część danych mogła nie zostać sprawdzona, więc wyniki mogą być niepełne.",
        "读取压缩文件时达到安全限制。部分数据可能尚未检查，因此结果可能不完整。",
        "La auditoría alcanzó un límite de seguridad al leer este archivo comprimido. Es posible que algunos datos no se hayan comprobado, por lo que los resultados pueden estar incompletos.",
        "このアーカイブの読み取り中に監査が安全上の制限に達しました。一部のデータが検査されていない可能性があるため、結果が不完全な場合があります。",
        "Beim Lesen dieses Archivs hat das Audit ein Sicherheitslimit erreicht. Möglicherweise wurden einige Daten nicht geprüft, daher können die Ergebnisse unvollständig sein.",
    ),
    (
        "Not tested",
        "Nie sprawdzono",
        "未测试",
        "Sin probar",
        "未テスト",
        "Nicht getestet",
    ),
    (
        "Integrity passed",
        "Test integralności zakończony pomyślnie",
        "完整性测试通过",
        "Integridad correcta",
        "整合性テストに合格",
        "Integritätstest bestanden",
    ),
    (
        "Integrity failed",
        "Błąd integralności",
        "完整性测试失败",
        "Error de integridad",
        "整合性テストに失敗",
        "Integritätstest fehlgeschlagen",
    ),
    (
        "Inspection incomplete",
        "Sprawdzanie niepełne",
        "检查未完成",
        "Inspección incompleta",
        "検査が未完了",
        "Untersuchung unvollständig",
    ),
    (
        "Password required",
        "Wymagane hasło",
        "需要密码",
        "Contraseña requerida",
        "パスワードが必要",
        "Passwort erforderlich",
    ),
    (
        "Everything looks good",
        "Wszystko wygląda dobrze",
        "一切正常",
        "Todo parece correcto",
        "問題ありません",
        "Alles in Ordnung",
    ),
    (
        "No issues were found in the checked data",
        "Nie znaleziono problemów w sprawdzonych danych",
        "已检查数据未发现问题",
        "No se encontraron problemas en los datos comprobados",
        "検査したデータに問題は見つかりませんでした",
        "In den geprüften Daten wurden keine Probleme gefunden",
    ),
    (
        "Integrity has not been tested",
        "Nie sprawdzono integralności",
        "尚未检查完整性",
        "No se ha probado la integridad",
        "整合性はまだテストされていません",
        "Die Integrität wurde nicht getestet",
    ),
    (
        "Run a test to verify archive contents",
        "Uruchom test, aby zweryfikować zawartość archiwum",
        "运行测试以验证压缩文件内容",
        "Ejecuta una prueba para verificar el contenido del archivo comprimido",
        "テストを実行してアーカイブの内容を検証します",
        "Führen Sie einen Test aus, um den Archivinhalt zu überprüfen",
    ),
    (
        "Findings need review",
        "Wyniki wymagają sprawdzenia",
        "发现的问题需要复核",
        "Los resultados requieren revisión",
        "結果の確認が必要です",
        "Die Ergebnisse müssen überprüft werden",
    ),
    (
        "Integrity check failed",
        "Test integralności nie powiódł się",
        "完整性检查失败",
        "La comprobación de integridad falló",
        "整合性チェックに失敗しました",
        "Integritätsprüfung fehlgeschlagen",
    ),
    (
        "Some archive data could not be checked",
        "Nie udało się sprawdzić części danych archiwum",
        "部分压缩文件数据无法检查",
        "No se pudieron comprobar algunos datos del archivo comprimido",
        "一部のアーカイブデータを検査できませんでした",
        "Einige Archivdaten konnten nicht geprüft werden",
    ),
    (
        "This archive requires a password for verification",
        "To archiwum wymaga hasła do weryfikacji",
        "此压缩文件需要密码才能验证",
        "Este archivo comprimido requiere una contraseña para su verificación",
        "このアーカイブの検証にはパスワードが必要です",
        "Dieses Archiv benötigt zur Überprüfung ein Passwort",
    ),
    (
        "Password requirement",
        "Wymaganie hasła",
        "密码要求",
        "Requisito de contraseña",
        "パスワードの要否",
        "Passwortanforderung",
    ),
    (
        "Not required",
        "Niewymagane",
        "无需密码",
        "No requerida",
        "不要",
        "Nicht erforderlich",
    ),
    (
        "Password verified",
        "Hasło zweryfikowane",
        "密码已验证",
        "Contraseña verificada",
        "パスワード確認済み",
        "Passwort überprüft",
    ),
    (
        "Required to verify",
        "Wymagane do weryfikacji",
        "验证时需要密码",
        "Necesaria para verificar",
        "検証に必要",
        "Zur Überprüfung erforderlich",
    ),
    (
        "Encrypted entries",
        "Zaszyfrowane wpisy",
        "加密条目",
        "Entradas cifradas",
        "暗号化されたエントリ",
        "Verschlüsselte Einträge",
    ),
    (
        "Review the listed findings",
        "Sprawdź wymienione ustalenia",
        "请查看列出的问题",
        "Revisa los resultados enumerados",
        "一覧の結果を確認してください",
        "Überprüfen Sie die aufgeführten Ergebnisse",
    ),
    (
        "Detected format",
        "Wykryty format",
        "检测到的格式",
        "Formato detectado",
        "検出された形式",
        "Erkanntes Format",
    ),
    ("Archive file", "Plik archiwum", "压缩文件", "Archivo comprimido", "アーカイブファイル", "Archivdatei"),
    ("Entry metadata", "Metadane wpisów", "条目元数据", "Metadatos de entradas", "エントリのメタデータ", "Eintragsmetadaten"),
    ("Filesystem permissions", "Uprawnienia systemu plików", "文件系统权限", "Permisos del sistema de archivos", "ファイルシステムの権限", "Dateisystemberechtigungen"),
    ("Filesystem state", "Stan pliku", "文件状态", "Estado del archivo", "ファイルの状態", "Dateistatus"),
    ("Read-only", "Tylko do odczytu", "只读", "Solo lectura", "読み取り専用", "Schreibgeschützt"),
    ("Read/write", "Odczyt i zapis", "读写", "Lectura y escritura", "読み書き可能", "Lesen/Schreiben"),
    ("Owner/group", "Właściciel/grupa", "所有者/组", "Propietario/grupo", "所有者/グループ", "Eigentümer/Gruppe"),
    ("Accessed", "Ostatni dostęp", "访问时间", "Último acceso", "最終アクセス", "Letzter Zugriff"),
    ("Archive comment", "Komentarz archiwum", "压缩文件注释", "Comentario del archivo comprimido", "アーカイブコメント", "Archivkommentar"),
    ("Integrity", "Integralność", "完整性", "Integridad", "整合性", "Integrität"),
    ("Directories", "Katalogi", "目录", "Directorios", "ディレクトリ", "Verzeichnisse"),
    ("Entries with permissions", "Wpisy z uprawnieniami", "包含权限的条目", "Entradas con permisos", "権限のあるエントリ", "Einträge mit Berechtigungen"),
    ("Entries with owner/group", "Wpisy z właścicielem/grupą", "包含所有者/组的条目", "Entradas con propietario/grupo", "所有者/グループ情報のあるエントリ", "Einträge mit Eigentümer/Gruppe"),
    ("Links", "Łącza", "链接", "Enlaces", "リンク", "Links"),
    ("Entry comments", "Komentarze wpisów", "条目注释", "Comentarios de entradas", "エントリのコメント", "Eintragskommentare"),
    ("Compression methods", "Metody kompresji", "压缩方法", "Métodos de compresión", "圧縮方式", "Komprimierungsmethoden"),
    (
        "Expected format",
        "Oczekiwany format",
        "预期格式",
        "Formato esperado",
        "期待される形式",
        "Erwartetes Format",
    ),
    (
        "Unknown",
        "Nieznany",
        "未知",
        "Desconocido",
        "不明",
        "Unbekannt",
    ),
    (
        "Archive size",
        "Rozmiar archiwum",
        "压缩文件大小",
        "Tamaño del archivo comprimido",
        "アーカイブのサイズ",
        "Archivgröße",
    ),
    (
        "Uncompressed size",
        "Rozmiar po rozpakowaniu",
        "解压后大小",
        "Tamaño descomprimido",
        "展開後のサイズ",
        "Unkomprimierte Größe",
    ),
    (
        "Entry count",
        "Liczba wpisów",
        "条目数量",
        "Número de entradas",
        "エントリ数",
        "Anzahl der Einträge",
    ),
    (
        "Compressed size",
        "Rozmiar skompresowany",
        "压缩后大小",
        "Tamaño comprimido",
        "圧縮後のサイズ",
        "Komprimierte Größe",
    ),
    (
        "Compression ratio",
        "Współczynnik kompresji",
        "压缩比",
        "Relación de compresión",
        "圧縮率",
        "Komprimierungsrate",
    ),
    (
        "Compression level",
        "Poziom kompresji",
        "压缩级别",
        "Nivel de compresión",
        "圧縮レベル",
        "Komprimierungsstufe",
    ),
    (
        "Not recorded by this format",
        "Format nie zapisuje tej informacji",
        "该格式未记录此信息",
        "Este formato no lo registra",
        "この形式では記録されません",
        "Wird von diesem Format nicht erfasst",
    ),
    (
        "Compression method",
        "Metoda kompresji",
        "压缩方法",
        "Método de compresión",
        "圧縮方式",
        "Komprimierungsmethode",
    ),
    (
        "GZIP compression",
        "Kompresja GZIP",
        "GZIP 压缩",
        "Compresión GZIP",
        "GZIP 圧縮",
        "GZIP-Komprimierung",
    ),
    (
        "BZIP2 compression",
        "Kompresja BZIP2",
        "BZIP2 压缩",
        "Compresión BZIP2",
        "BZIP2 圧縮",
        "BZIP2-Komprimierung",
    ),
    (
        "XZ compression",
        "Kompresja XZ",
        "XZ 压缩",
        "Compresión XZ",
        "XZ 圧縮",
        "XZ-Komprimierung",
    ),
    (
        "Zstandard compression",
        "Kompresja Zstandard",
        "Zstandard 压缩",
        "Compresión Zstandard",
        "Zstandard 圧縮",
        "Zstandard-Komprimierung",
    ),
    (
        "LZ4 compression",
        "Kompresja LZ4",
        "LZ4 压缩",
        "Compresión LZ4",
        "LZ4 圧縮",
        "LZ4-Komprimierung",
    ),
    (
        "LZIP compression",
        "Kompresja LZIP",
        "LZIP 压缩",
        "Compresión LZIP",
        "LZIP 圧縮",
        "LZIP-Komprimierung",
    ),
    (
        "Brotli compression",
        "Kompresja Brotli",
        "Brotli 压缩",
        "Compresión Brotli",
        "Brotli 圧縮",
        "Brotli-Komprimierung",
    ),
    ("Method", "Metoda", "方法", "Método", "方式", "Methode"),
    (
        "CRC value",
        "CRC32",
        "CRC32 校验值",
        "Valor CRC32",
        "CRC32 値",
        "CRC32-Wert",
    ),
    (
        "Unsupported compression method",
        "Nieobsługiwana metoda kompresji",
        "不支持的压缩方法",
        "Método de compresión no compatible",
        "非対応の圧縮方式",
        "Nicht unterstützte Komprimierungsmethode",
    ),
    ("Name", "Nazwa", "名称", "Nombre", "名前", "Name"),
    (
        "Modified",
        "Modyfikacja",
        "修改时间",
        "Modificado",
        "更新日時",
        "Geändert",
    ),
    (
        "Last modified",
        "Ostatnia modyfikacja",
        "最后修改",
        "Última modificación",
        "最終更新",
        "Zuletzt geändert",
    ),
    (
        "Attributes",
        "Atrybuty",
        "属性",
        "Atributos",
        "属性",
        "Attribute",
    ),
    (
        "Encrypted",
        "Szyfrowanie",
        "加密",
        "Cifrado",
        "暗号化",
        "Verschlüsselt",
    ),
    ("Yes", "Tak", "是", "Sí", "はい", "Ja"),
    ("No", "Nie", "否", "No", "いいえ", "Nein"),
    (
        "Stored CRC32",
        "Zapisane CRC32",
        "已存储 CRC32",
        "CRC32 almacenado",
        "保存済み CRC32",
        "Gespeicherte CRC32",
    ),
    (
        "Computed CRC32",
        "Obliczone CRC32",
        "计算的 CRC32",
        "CRC32 calculado",
        "計算した CRC32",
        "Berechnete CRC32",
    ),
    (
        "Load more",
        "Pokaż więcej",
        "加载更多",
        "Cargar más",
        "さらに読み込む",
        "Mehr laden",
    ),
    (
        "Copy report",
        "Kopiuj raport",
        "复制报告",
        "Copiar informe",
        "レポートをコピー",
        "Bericht kopieren",
    ),
    (
        "Save report",
        "Zapisz raport",
        "保存报告",
        "Guardar informe",
        "レポートを保存",
        "Bericht speichern",
    ),
    (
        "Save inspection report",
        "Zapisz raport z inspekcji",
        "保存检查报告",
        "Guardar informe de inspección",
        "検査レポートを保存",
        "Inspektionsbericht speichern",
    ),
    (
        "Exporting report...",
        "Eksportowanie raportu...",
        "正在导出报告...",
        "Exportando informe...",
        "レポートをエクスポート中...",
        "Bericht wird exportiert...",
    ),
    (
        "Report saved",
        "Zapisano raport",
        "报告已保存",
        "Informe guardado",
        "レポートを保存しました",
        "Bericht gespeichert",
    ),
    (
        "Could not save report",
        "Nie udało się zapisać raportu",
        "无法保存报告",
        "No se pudo guardar el informe",
        "レポートを保存できませんでした",
        "Bericht konnte nicht gespeichert werden",
    ),
    (
        "Report path must differ from archive",
        "Ścieżka raportu musi być inna niż ścieżka archiwum",
        "报告路径必须与压缩文件不同",
        "La ruta del informe debe ser distinta de la del archivo",
        "レポートの保存先はアーカイブと異なる必要があります",
        "Der Berichtspfad muss sich vom Archivpfad unterscheiden",
    ),
    (
        "No findings in checked data",
        "Brak znalezisk w sprawdzonych danych",
        "已检查数据中无发现",
        "Sin resultados en los datos comprobados",
        "検査したデータに指摘事項はありません",
        "Keine Befunde in den geprüften Daten",
    ),
    (
        "Audit not run",
        "Audyt nie został uruchomiony",
        "尚未运行审计",
        "Auditoría no ejecutada",
        "監査は未実行",
        "Audit nicht ausgeführt",
    ),
    (
        "Choose Inspect > Audit archive to check this archive for issues.",
        "Wybierz Inspekcja > Audyt archiwum, aby sprawdzić archiwum pod kątem problemów.",
        "选择“检查”>“审核压缩文件”以检查此压缩文件是否存在问题。",
        "Elige Inspeccionar > Auditar archivo comprimido para buscar problemas en este archivo comprimido.",
        "「検査」>「アーカイブを監査」を選択して、このアーカイブの問題を確認します。",
        "Wählen Sie „Untersuchen“ > „Archiv auditieren“, um dieses Archiv auf Probleme zu prüfen.",
    ),
    (
        "No issues found",
        "Nie znaleziono problemów",
        "未发现问题",
        "No se encontraron problemas",
        "問題は見つかりませんでした",
        "Keine Probleme gefunden",
    ),
    (
        "The audit completed without finding any issues.",
        "Audyt zakończył się bez wykrycia problemów.",
        "审核已完成，未发现任何问题。",
        "La auditoría finalizó sin encontrar problemas.",
        "監査は問題なく完了しました。",
        "Das Audit wurde abgeschlossen, ohne Probleme zu finden.",
    ),
    (
        "Damaged archive",
        "Uszkodzone archiwum",
        "损坏的压缩文件",
        "Archivo comprimido dañado",
        "破損したアーカイブ",
        "Beschädigtes Archiv",
    ),
    (
        "Damaged entry",
        "Uszkodzony wpis",
        "损坏的条目",
        "Entrada dañada",
        "破損したエントリ",
        "Beschädigter Eintrag",
    ),
    (
        "Format mismatch",
        "Niezgodność formatu",
        "格式不匹配",
        "Formato incompatible",
        "形式の不一致",
        "Format stimmt nicht überein",
    ),
    (
        "Nested archive",
        "Zagnieżdżone archiwum",
        "嵌套压缩文件",
        "Archivo comprimido anidado",
        "入れ子のアーカイブ",
        "Verschachteltes Archiv",
    ),
    (
        "Unsafe path",
        "Niebezpieczna ścieżka",
        "不安全路径",
        "Ruta no segura",
        "安全でないパス",
        "Unsicherer Pfad",
    ),
    (
        "Link entry",
        "Dowiązanie",
        "链接条目",
        "Entrada de enlace",
        "リンクエントリ",
        "Link-Eintrag",
    ),
    (
        "Suspicious size",
        "Podejrzany rozmiar",
        "可疑大小",
        "Tamaño sospechoso",
        "疑わしいサイズ",
        "Verdächtige Größe",
    ),
    (
        "Duplicate name",
        "Powtórzona nazwa",
        "重复名称",
        "Nombre duplicado",
        "名前の重複",
        "Doppelter Name",
    ),
    (
        "Duplicate content",
        "Powtórzona zawartość",
        "重复内容",
        "Contenido duplicado",
        "内容の重複",
        "Doppelter Inhalt",
    ),
    (
        "Duplicate archive",
        "Powtórzone archiwum",
        "重复压缩文件",
        "Archivo comprimido duplicado",
        "アーカイブの重複",
        "Doppeltes Archiv",
    ),
    (
        "Inspection limit reached",
        "Osiągnięto limit sprawdzania",
        "已达到检查限制",
        "Límite de inspección alcanzado",
        "検査制限に到達",
        "Untersuchungslimit erreicht",
    ),
    (
        "File signature does not match its extension",
        "Sygnatura pliku nie pasuje do rozszerzenia",
        "文件签名与扩展名不匹配",
        "La firma del archivo no coincide con su extensión",
        "ファイルのシグネチャが拡張子と一致しません",
        "Die Dateisignatur stimmt nicht mit der Erweiterung überein",
    ),
    (
        "Nested archives are reported, not recursively expanded",
        "Zagnieżdżone archiwa są raportowane bez rekurencyjnego rozpakowywania",
        "报告嵌套压缩文件但不递归解压",
        "Los archivos comprimidos anidados se notifican, pero no se expanden de forma recursiva",
        "入れ子のアーカイブは報告のみで、再帰的には展開されません",
        "Verschachtelte Archive werden gemeldet, aber nicht rekursiv entpackt",
    ),
    (
        "Entry does not have an archive signature",
        "Wpis nie ma sygnatury archiwum",
        "条目没有压缩文件签名",
        "La entrada no tiene firma de archivo comprimido",
        "エントリにアーカイブのシグネチャがありません",
        "Der Eintrag hat keine Archivsignatur",
    ),
    (
        "Detected content signature does not match the filename",
        "Sygnatura zawartości nie pasuje do nazwy pliku",
        "内容签名与文件名不匹配",
        "La firma de contenido detectada no coincide con el nombre del archivo",
        "検出された内容のシグネチャがファイル名と一致しません",
        "Die erkannte Inhaltssignatur stimmt nicht mit dem Dateinamen überein",
    ),
    (
        "Entry could escape an extraction folder",
        "Wpis może wyjść poza folder wyodrębniania",
        "条目可能逃逸提取目录",
        "La entrada podría salirse de la carpeta de extracción",
        "エントリが展開先フォルダの外に出る可能性があります",
        "Der Eintrag könnte den Extraktionsordner verlassen",
    ),
    (
        "Symbolic links and reparse points require review",
        "Dowiązania i punkty ponownej analizy wymagają sprawdzenia",
        "符号链接和重解析点需要审核",
        "Los enlaces simbólicos y los puntos de reanálisis requieren revisión",
        "シンボリックリンクとリパースポイントは確認が必要です",
        "Symbolische Links und Reparse-Points müssen überprüft werden",
    ),
    (
        "Large entry or unusually high compression ratio",
        "Duży wpis lub nietypowo wysoki współczynnik kompresji",
        "大条目或异常高压缩比",
        "Entrada grande o relación de compresión inusualmente alta",
        "大きなエントリ、または異常に高い圧縮率",
        "Großer Eintrag oder ungewöhnlich hohe Komprimierungsrate",
    ),
    (
        "Some data was not fully checked",
        "Część danych nie została sprawdzona w całości",
        "部分数据未完全检查",
        "Algunos datos no se comprobaron por completo",
        "一部のデータは完全には検査されていません",
        "Einige Daten wurden nicht vollständig geprüft",
    ),
    (
        "Compression dictionary exceeds the inspection memory limit",
        "Słownik kompresji przekracza limit pamięci inspektora",
        "压缩字典超过检查器内存限制",
        "El diccionario de compresión supera el límite de memoria de inspección",
        "圧縮辞書が検査のメモリ上限を超えています",
        "Das Komprimierungswörterbuch überschreitet das Speicherlimit der Untersuchung",
    ),
    (
        "TAR stores header checksums, not file content checksums",
        "TAR zapisuje sumy nagłówków, a nie zawartości plików",
        "TAR 存储头部校验和而非文件内容校验和",
        "TAR almacena sumas de verificación de las cabeceras, no del contenido de los archivos",
        "TAR はヘッダーのチェックサムを保存しますが、ファイル内容のチェックサムは保存しません",
        "TAR speichert Header-Prüfsummen, keine Prüfsummen der Dateiinhalte",
    ),
    (
        "Settings",
        "Ustawienia",
        "设置",
        "Ajustes",
        "設定",
        "Einstellungen",
    ),
    ("General", "Ogólne", "常规", "General", "一般", "Allgemein"),
    (
        "Appearance",
        "Wygląd",
        "外观",
        "Apariencia",
        "外観",
        "Darstellung",
    ),
    (
        "About",
        "O aplikacji",
        "关于",
        "Acerca de",
        "このアプリについて",
        "Info",
    ),
    ("Close", "Zamknij", "关闭", "Cerrar", "閉じる", "Schließen"),
    ("Language", "Język", "语言", "Idioma", "言語", "Sprache"),
    ("Theme", "Motyw", "主题", "Tema", "テーマ", "Design"),
    (
        "Accent color",
        "Kolor akcentu",
        "强调色",
        "Color de énfasis",
        "アクセントカラー",
        "Akzentfarbe",
    ),
    (
        "Default location",
        "Lokalizacja domyślna",
        "默认位置",
        "Ubicación predeterminada",
        "既定の場所",
        "Standardspeicherort",
    ),
    (
        "System default",
        "Domyślna systemowa",
        "系统默认",
        "Predeterminado del sistema",
        "システム既定",
        "Systemstandard",
    ),
    (
        "Use system default",
        "Użyj domyślnej systemowej",
        "使用系统默认",
        "Usar el valor predeterminado del sistema",
        "システム既定を使用",
        "Systemstandard verwenden",
    ),
    ("Blue", "Niebieski", "蓝色", "Azul", "ブルー", "Blau"),
    ("Teal", "Morski", "青色", "Turquesa", "ティール", "Türkis"),
    ("Green", "Zielony", "绿色", "Verde", "グリーン", "Grün"),
    (
        "Orange",
        "Pomarańczowy",
        "橙色",
        "Naranja",
        "オレンジ",
        "Orange",
    ),
    ("Rose", "Różowy", "玫瑰色", "Rosa", "ローズ", "Rosa"),
    (
        "Could not save settings",
        "Nie udało się zapisać ustawień",
        "无法保存设置",
        "No se pudieron guardar los ajustes",
        "設定を保存できませんでした",
        "Einstellungen konnten nicht gespeichert werden",
    ),
    (
        "System",
        "Systemowy",
        "跟随系统",
        "Sistema",
        "システム",
        "System",
    ),
    ("Light", "Jasny", "浅色", "Claro", "ライト", "Hell"),
    ("Dark", "Ciemny", "深色", "Oscuro", "ダーク", "Dunkel"),
    (
        "Version",
        "Wersja",
        "版本",
        "Versión",
        "バージョン",
        "Version",
    ),
    (
        "License",
        "Licencja",
        "许可证",
        "Licencia",
        "ライセンス",
        "Lizenz",
    ),
    (
        "Supported formats",
        "Obsługiwane formaty",
        "支持的格式",
        "Formatos compatibles",
        "対応形式",
        "Unterstützte Formate",
    ),
    (
        "Read and extract: ZIP, TAR, compressed TAR, CPIO, AR, PAX, XAR, standalone GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z, RAR",
        "Odczyt i rozpakowywanie: ZIP, TAR, skompresowany TAR, CPIO, AR, PAX, XAR, samodzielne GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z, RAR",
        "读取和解压：ZIP、TAR、压缩 TAR、CPIO、AR、PAX、XAR、独立 GZIP、BZIP2、XZ、Zstandard、LZ4、LZIP、Brotli、7z、RAR",
        "Lectura y extracción: ZIP, TAR, TAR comprimido, CPIO, AR, PAX, XAR, GZIP independiente, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z, RAR",
        "読み取りと展開：ZIP、TAR、圧縮 TAR、CPIO、AR、PAX、XAR、単独 GZIP、BZIP2、XZ、Zstandard、LZ4、LZIP、Brotli、7z、RAR",
        "Lesen und Entpacken: ZIP, TAR, komprimiertes TAR, CPIO, AR, PAX, XAR, eigenständiges GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z, RAR",
    ),
    (
        "Create: ZIP",
        "Tworzenie: ZIP",
        "创建：ZIP",
        "Creación: ZIP",
        "作成：ZIP",
        "Erstellen: ZIP",
    ),
    (
        "Password-protected 7z archives are not supported.",
        "Archiwa 7z chronione hasłem nie są obsługiwane.",
        "不支持受密码保护的 7z 压缩文件。",
        "Los archivos 7z protegidos con contraseña no son compatibles.",
        "パスワード保護された 7z アーカイブには対応していません。",
        "Passwortgeschützte 7z-Archive werden nicht unterstützt.",
    ),
    (
        "Password-protected ZIP, 7z, and RAR archives are supported.",
        "Obsługa archiwów ZIP, 7z i RAR chronionych hasłem.",
        "支持受密码保护的 ZIP、7z 和 RAR 压缩文件。",
        "Se admiten archivos ZIP, 7z y RAR protegidos con contraseña.",
        "パスワード保護された ZIP、7z、RAR アーカイブに対応しています。",
        "Passwortgeschützte ZIP-, 7z- und RAR-Archive werden unterstützt.",
    ),
    (
        "Force ZIP64",
        "Wymuś ZIP64",
        "强制 ZIP64",
        "Forzar ZIP64",
        "ZIP64 を強制",
        "ZIP64 erzwingen",
    ),
    (
        "Dictionary size",
        "Rozmiar słownika",
        "字典大小",
        "Tamaño del diccionario",
        "辞書サイズ",
        "Wörterbuchgröße",
    ),
    (
        "Solid compression",
        "Kompresja solid",
        "固实压缩",
        "Compresión sólida",
        "ソリッド圧縮",
        "Solide Komprimierung",
    ),
    (
        "Deterministic metadata",
        "Deterministyczne metadane",
        "确定性元数据",
        "Metadatos deterministas",
        "決定的メタデータ",
        "Deterministische Metadaten",
    ),
    (
        "Encrypt with AES-256",
        "Szyfruj AES-256",
        "使用 AES-256 加密",
        "Cifrar con AES-256",
        "AES-256 で暗号化",
        "Mit AES-256 verschlüsseln",
    ),
    (
        "Encrypt file names",
        "Szyfruj nazwy plików",
        "加密文件名",
        "Cifrar nombres de archivo",
        "ファイル名を暗号化",
        "Dateinamen verschlüsseln",
    ),
    (
        "Password",
        "Hasło",
        "密码",
        "Contraseña",
        "パスワード",
        "Passwort",
    ),
    (
        "Confirm password",
        "Potwierdź hasło",
        "确认密码",
        "Confirmar contraseña",
        "パスワードを確認",
        "Passwort bestätigen",
    ),
    (
        "Archive password",
        "Hasło archiwum",
        "压缩文件密码",
        "Contraseña del archivo comprimido",
        "アーカイブのパスワード",
        "Archivpasswort",
    ),
    (
        "Enter a password",
        "Wpisz hasło",
        "请输入密码",
        "Introduce una contraseña",
        "パスワードを入力してください",
        "Passwort eingeben",
    ),
    (
        "Passwords do not match",
        "Hasła nie są zgodne",
        "两次输入的密码不一致",
        "Las contraseñas no coinciden",
        "パスワードが一致しません",
        "Passwörter stimmen nicht überein",
    ),
    (
        "Incorrect password or damaged archive",
        "Błędne hasło lub uszkodzone archiwum",
        "密码错误或压缩文件已损坏",
        "Contraseña incorrecta o archivo comprimido dañado",
        "パスワードが正しくないか、アーカイブが破損しています",
        "Falsches Passwort oder beschädigtes Archiv",
    ),
    (
        "Unlock",
        "Odblokuj",
        "解锁",
        "Desbloquear",
        "ロック解除",
        "Entsperren",
    ),
    (
        "Unlocking archive...",
        "Odblokowywanie archiwum...",
        "正在解锁压缩文件……",
        "Desbloqueando archivo comprimido...",
        "アーカイブのロックを解除しています...",
        "Archiv wird entsperrt...",
    ),
    (
        "Author & contact",
        "Autor i kontakt",
        "作者与联系方式",
        "Autor y contacto",
        "作者と連絡先",
        "Autor und Kontakt",
    ),
    (
        "Browse, create, verify, and extract archives in one place.",
        "Jedno miejsce do przeglądania, tworzenia, weryfikowania i rozpakowywania archiwów.",
        "一站式浏览、创建、校验和提取压缩文件。",
        "Explora, crea, verifica y extrae archivos comprimidos desde un mismo lugar.",
        "アーカイブの閲覧、作成、整合性確認、展開を一か所で行えます。",
        "Archive an einem Ort durchsuchen, erstellen, prüfen und entpacken.",
    ),
    (
        "Open file",
        "Otwórz plik",
        "打开文件",
        "Abrir archivo",
        "ファイルを開く",
        "Datei öffnen",
    ),
    (
        "Open archive...",
        "Otwórz archiwum...",
        "打开压缩文件…",
        "Abrir archivo comprimido...",
        "アーカイブを開く…",
        "Archiv öffnen...",
    ),
    ("Open", "Otwórz", "打开", "Abrir", "開く", "Öffnen"),
    (
        "Copy path",
        "Kopiuj ścieżkę",
        "复制路径",
        "Copiar ruta",
        "パスをコピー",
        "Pfad kopieren",
    ),
    (
        "Path copied",
        "Skopiowano ścieżkę",
        "已复制路径",
        "Ruta copiada",
        "パスをコピーしました",
        "Pfad kopiert",
    ),
    (
        "Extract file",
        "Wyodrębnij plik",
        "提取文件",
        "Extraer archivo",
        "ファイルを展開",
        "Datei entpacken",
    ),
    (
        "Extract archive to...",
        "Wyodrębnij archiwum do...",
        "解压缩到……",
        "Extraer archivo comprimido en...",
        "アーカイブの展開先...",
        "Archiv entpacken nach...",
    ),
    (
        "Extract here",
        "Wyodrębnij tutaj",
        "解压到当前位置",
        "Extraer aquí",
        "ここに展開",
        "Hier entpacken",
    ),
    (
        "Extract to...",
        "Wyodrębnij do...",
        "解压缩到……",
        "Extraer en...",
        "展開先...",
        "Entpacken nach...",
    ),
    (
        "Move to...",
        "Przenieś do...",
        "移动到……",
        "Mover a...",
        "移動先...",
        "Verschieben nach...",
    ),
    (
        "Select destination folder",
        "Wybierz folder docelowy",
        "选择目标文件夹",
        "Selecciona la carpeta de destino",
        "保存先フォルダを選択",
        "Zielordner auswählen",
    ),
    (
        "Moving item...",
        "Przenoszenie elementu...",
        "正在移动项目……",
        "Moviendo elemento...",
        "項目を移動しています...",
        "Element wird verschoben...",
    ),
    (
        "Extracting archive...",
        "Wyodrębnianie archiwum...",
        "正在解压缩文件……",
        "Extrayendo archivo comprimido...",
        "アーカイブを展開しています...",
        "Archiv wird entpackt...",
    ),
    (
        "Moved to",
        "Przeniesiono do",
        "已移动到",
        "Movido a",
        "移動先：",
        "Verschoben nach",
    ),
    (
        "Failed to move item",
        "Nie udało się przenieść elementu",
        "无法移动项目",
        "No se pudo mover el elemento",
        "項目を移動できませんでした",
        "Element konnte nicht verschoben werden",
    ),
    (
        "Extracted to",
        "Wyodrębniono do",
        "已提取到",
        "Extraído en",
        "展開先：",
        "Entpackt nach",
    ),
    (
        "Move to Trash",
        "Przenieś do kosza",
        "移到废纸篓",
        "Mover a la papelera",
        "ゴミ箱に移動",
        "In den Papierkorb verschieben",
    ),
    (
        "Move this item to the Trash?",
        "Przenieść ten element do kosza?",
        "要将此项目移到废纸篓吗？",
        "¿Mover este elemento a la papelera?",
        "この項目をゴミ箱に移動しますか？",
        "Dieses Element in den Papierkorb verschieben?",
    ),
    (
        "Moving to Trash...",
        "Przenoszenie do kosza...",
        "正在移到废纸篓……",
        "Moviendo a la papelera...",
        "ゴミ箱に移動しています...",
        "Wird in den Papierkorb verschoben...",
    ),
    (
        "Moved to Trash",
        "Przeniesiono do kosza",
        "已移到废纸篓",
        "Movido a la papelera",
        "ゴミ箱に移動しました",
        "In den Papierkorb verschoben",
    ),
    (
        "Failed to move to Trash",
        "Nie udało się przenieść do kosza",
        "无法移到废纸篓",
        "No se pudo mover a la papelera",
        "ゴミ箱に移動できませんでした",
        "Verschieben in den Papierkorb fehlgeschlagen",
    ),
    (
        "Refresh",
        "Odśwież",
        "刷新",
        "Actualizar",
        "更新",
        "Aktualisieren",
    ),
    (
        "Choose folder",
        "Wybierz folder",
        "选择文件夹",
        "Elegir carpeta",
        "フォルダを選択",
        "Ordner auswählen",
    ),
    (
        "Parent folder",
        "Folder nadrzędny",
        "上级文件夹",
        "Carpeta superior",
        "親フォルダ",
        "Übergeordneter Ordner",
    ),
    ("Back", "Wstecz", "后退", "Atrás", "戻る", "Zurück"),
    ("Forward", "Dalej", "前进", "Adelante", "進む", "Weiter"),
    (
        "Create ZIP",
        "Utwórz ZIP",
        "创建 ZIP",
        "Crear ZIP",
        "ZIP を作成",
        "ZIP erstellen",
    ),
    (
        "Create archive",
        "Utwórz archiwum",
        "创建压缩文件",
        "Crear archivo comprimido",
        "アーカイブを作成",
        "Archiv erstellen",
    ),
    (
        "New archive...",
        "Nowe archiwum...",
        "新建压缩文件…",
        "Nuevo archivo comprimido...",
        "新しいアーカイブ…",
        "Neues Archiv...",
    ),
    ("Files", "Pliki", "文件", "Archivos", "ファイル", "Dateien"),
    (
        "Extraction",
        "Rozpakowywanie",
        "解压",
        "Extracción",
        "展開",
        "Entpacken",
    ),
    (
        "Inspect",
        "Inspekcja",
        "检查",
        "Inspeccionar",
        "検査",
        "Untersuchen",
    ),
    (
        "Create: ZIP, TAR, CPIO, AR, PAX, XAR, standalone GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z",
        "Tworzenie: ZIP, TAR, CPIO, AR, PAX, XAR, samodzielne GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z",
        "创建：ZIP、TAR、CPIO、AR、PAX、XAR、独立 GZIP、BZIP2、XZ、Zstandard、LZ4、LZIP、Brotli、7z",
        "Creación: ZIP, TAR, CPIO, AR, PAX, XAR, GZIP independiente, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z",
        "作成：ZIP、TAR、CPIO、AR、PAX、XAR、単独 GZIP、BZIP2、XZ、Zstandard、LZ4、LZIP、Brotli、7z",
        "Erstellen: ZIP, TAR, CPIO, AR, PAX, XAR, eigenständiges GZIP, BZIP2, XZ, Zstandard, LZ4, LZIP, Brotli, 7z",
    ),
    (
        "AR archives support files only. Select individual files instead of folders.",
        "Archiwa AR obsługują tylko pliki. Wybierz pojedyncze pliki zamiast folderów.",
        "AR 归档仅支持文件。请选择单个文件，而不是文件夹。",
        "Los archivos AR solo admiten ficheros. Selecciona archivos individuales en lugar de carpetas.",
        "AR アーカイブはファイルのみ対応しています。フォルダーではなく個別のファイルを選択してください。",
        "AR-Archive unterstützen nur Dateien. Wählen Sie einzelne Dateien statt Ordnern aus.",
    ),
    (
        "Sources",
        "Źródła",
        "源文件",
        "Orígenes",
        "ソース",
        "Quellen",
    ),
    (
        "Add files",
        "Dodaj pliki",
        "添加文件",
        "Añadir archivos",
        "ファイルを追加",
        "Dateien hinzufügen",
    ),
    (
        "Add folders",
        "Dodaj foldery",
        "添加文件夹",
        "Añadir carpetas",
        "フォルダを追加",
        "Ordner hinzufügen",
    ),
    ("Remove", "Usuń", "移除", "Quitar", "削除", "Entfernen"),
    (
        "No files or folders selected",
        "Nie wybrano plików ani folderów",
        "未选择文件或文件夹",
        "No hay archivos ni carpetas seleccionados",
        "ファイルまたはフォルダが選択されていません",
        "Keine Dateien oder Ordner ausgewählt",
    ),
    ("Format", "Format", "格式", "Formato", "形式", "Format"),
    (
        "Compression",
        "Kompresja",
        "压缩",
        "Compresión",
        "圧縮",
        "Komprimierung",
    ),
    (
        "No compression",
        "Bez kompresji",
        "不压缩",
        "Sin compresión",
        "圧縮なし",
        "Keine Komprimierung",
    ),
    ("Fast", "Szybka", "快速", "Rápida", "高速", "Schnell"),
    ("Normal", "Standardowa", "标准", "Normal", "標準", "Normal"),
    (
        "Best compression",
        "Najlepsza kompresja",
        "最佳压缩",
        "Mejor compresión",
        "最高圧縮",
        "Beste Komprimierung",
    ),
    (
        "Include hidden files",
        "Dołącz ukryte pliki",
        "包含隐藏文件",
        "Incluir archivos ocultos",
        "隠しファイルを含める",
        "Versteckte Dateien einbeziehen",
    ),
    (
        "Select folders to archive",
        "Wybierz foldery do archiwizacji",
        "选择要压缩的文件夹",
        "Selecciona las carpetas que quieres comprimir",
        "アーカイブするフォルダを選択",
        "Ordner zum Archivieren auswählen",
    ),
    (
        "Save archive",
        "Zapisz archiwum",
        "保存压缩文件",
        "Guardar archivo comprimido",
        "アーカイブを保存",
        "Archiv speichern",
    ),
    (
        "Creating archive...",
        "Tworzenie archiwum...",
        "正在创建压缩文件……",
        "Creando archivo comprimido...",
        "アーカイブを作成しています...",
        "Archiv wird erstellt...",
    ),
    (
        "The file extension must match the selected format.",
        "Rozszerzenie pliku musi odpowiadać wybranemu formatowi.",
        "文件扩展名必须与所选格式一致。",
        "La extensión del archivo debe coincidir con el formato seleccionado.",
        "ファイルの拡張子は選択した形式と一致している必要があります。",
        "Die Dateierweiterung muss zum ausgewählten Format passen.",
    ),
    ("Create", "Utwórz", "创建", "Crear", "作成", "Erstellen"),
    (
        "Cancel",
        "Anuluj",
        "取消",
        "Cancelar",
        "キャンセル",
        "Abbrechen",
    ),
    (
        "Select files to archive",
        "Wybierz pliki do archiwizacji",
        "选择要压缩的文件",
        "Selecciona los archivos que quieres comprimir",
        "アーカイブするファイルを選択",
        "Dateien zum Archivieren auswählen",
    ),
    (
        "Save ZIP archive",
        "Zapisz archiwum ZIP",
        "保存 ZIP 压缩文件",
        "Guardar archivo ZIP",
        "ZIP アーカイブを保存",
        "ZIP-Archiv speichern",
    ),
    (
        "ZIP archive",
        "Archiwum ZIP",
        "ZIP 压缩文件",
        "Archivo ZIP",
        "ZIP アーカイブ",
        "ZIP-Archiv",
    ),
    (
        "File Browser",
        "Przeglądarka plików",
        "文件浏览器",
        "Explorador de archivos",
        "ファイルブラウザ",
        "Dateibrowser",
    ),
    (
        "No location selected",
        "Nie wybrano lokalizacji",
        "未选择位置",
        "Ninguna ubicación seleccionada",
        "場所が選択されていません",
        "Kein Speicherort ausgewählt",
    ),
    (
        "Unable to open this archive",
        "Nie można otworzyć tego archiwum",
        "无法打开此压缩文件",
        "No se puede abrir este archivo comprimido",
        "このアーカイブを開けません",
        "Dieses Archiv kann nicht geöffnet werden",
    ),
    (
        "This folder is empty",
        "Ten folder jest pusty",
        "此文件夹为空",
        "Esta carpeta está vacía",
        "このフォルダは空です",
        "Dieser Ordner ist leer",
    ),
    (
        "There are no files or folders to display.",
        "Brak plików i folderów do wyświetlenia.",
        "没有可显示的文件或文件夹。",
        "No hay archivos ni carpetas que mostrar.",
        "表示するファイルまたはフォルダがありません。",
        "Keine Dateien oder Ordner zum Anzeigen vorhanden.",
    ),
    (
        "No matching files",
        "Brak pasujących plików",
        "没有匹配的文件",
        "No hay archivos coincidentes",
        "一致するファイルがありません",
        "Keine passenden Dateien",
    ),
    (
        "Try a different search term or clear the search.",
        "Zmień wyszukiwaną frazę lub wyczyść wyszukiwanie.",
        "请尝试其他搜索词或清空搜索。",
        "Prueba con otro término de búsqueda o borra la búsqueda.",
        "別の検索語を試すか、検索をクリアしてください。",
        "Versuchen Sie einen anderen Suchbegriff oder löschen Sie die Suche.",
    ),
    ("PLACES", "MIEJSCA", "位置", "LUGARES", "場所", "ORTE"),
    ("Places", "Miejsca", "位置", "Lugares", "場所", "Orte"),
    (
        "Documents",
        "Dokumenty",
        "文稿",
        "Documentos",
        "ドキュメント",
        "Dokumente",
    ),
    (
        "Home",
        "Katalog domowy",
        "主目录",
        "Carpeta personal",
        "ホーム",
        "Persönlicher Ordner",
    ),
    (
        "FOLDERS",
        "FOLDERY",
        "文件夹",
        "CARPETAS",
        "フォルダ",
        "ORDNER",
    ),
    (
        "Folders",
        "Foldery",
        "文件夹",
        "Carpetas",
        "フォルダ",
        "Ordner",
    ),
    (
        "SUPPORTED FORMATS",
        "OBSŁUGIWANE FORMATY",
        "支持的格式",
        "FORMATOS COMPATIBLES",
        "対応形式",
        "UNTERSTÜTZTE FORMATE",
    ),
    (
        "Location",
        "Lokalizacja",
        "位置",
        "Ubicación",
        "場所",
        "Speicherort",
    ),
    (
        "Search files",
        "Szukaj plików",
        "搜索文件",
        "Buscar archivos",
        "ファイルを検索",
        "Dateien suchen",
    ),
    (
        "Clear search",
        "Wyczyść wyszukiwanie",
        "清除搜索",
        "Limpiar búsqueda",
        "検索をクリア",
        "Suche löschen",
    ),
    (
        "Searching archives...",
        "Przeszukiwanie archiwów...",
        "正在搜索归档…",
        "Buscando en archivos...",
        "アーカイブを検索中...",
        "Archive werden durchsucht...",
    ),
    (
        "Search results limited. Refine your search.",
        "Osiągnięto limit wyników. Doprecyzuj wyszukiwanie.",
        "搜索结果已达上限。请缩小搜索范围。",
        "Se alcanzó el límite de resultados. Afina la búsqueda.",
        "検索結果の上限に達しました。検索条件を絞り込んでください。",
        "Das Ergebnislimit wurde erreicht. Grenzen Sie die Suche ein.",
    ),
    (
        "No matching items",
        "Brak pasujących elementów",
        "没有匹配的项目",
        "No hay elementos coincidentes",
        "一致する項目がありません",
        "Keine passenden Elemente",
    ),
    (
        "Try another search.",
        "Spróbuj innego wyszukiwania.",
        "请尝试其他搜索。",
        "Prueba otra búsqueda.",
        "別の検索をお試しください。",
        "Versuchen Sie eine andere Suche.",
    ),
    ("NAME", "NAZWA", "名称", "NOMBRE", "名前", "NAME"),
    ("TYPE", "TYP", "类型", "TIPO", "種類", "TYP"),
    ("SIZE", "ROZMIAR", "大小", "TAMAÑO", "サイズ", "GRÖSSE"),
    ("Size", "Rozmiar", "大小", "Tamaño", "サイズ", "Größe"),
    (
        "Size unavailable",
        "Rozmiar niedostępny",
        "大小不可用",
        "Tamaño no disponible",
        "サイズを取得できません",
        "Größe nicht verfügbar",
    ),
    (
        "Folder",
        "Folder",
        "文件夹",
        "Carpeta",
        "フォルダ",
        "Ordner",
    ),
    (
        "Calculating folder sizes...",
        "Obliczanie rozmiarów folderów...",
        "正在计算文件夹大小……",
        "Calculando tamaños de carpetas...",
        "フォルダのサイズを計算しています...",
        "Ordnergrößen werden berechnet...",
    ),
    (
        "Could not calculate all folder sizes",
        "Nie udało się obliczyć rozmiarów wszystkich folderów",
        "无法计算所有文件夹的大小",
        "No se pudieron calcular todos los tamaños de carpetas",
        "すべてのフォルダのサイズを計算できませんでした",
        "Nicht alle Ordnergrößen konnten berechnet werden",
    ),
    (
        "Opened",
        "Otwarto",
        "已打开",
        "Abierto",
        "開きました",
        "Geöffnet",
    ),
    (
        "Created",
        "Utworzono",
        "已创建",
        "Creado",
        "作成しました",
        "Erstellt",
    ),
    (
        "Failed to open file",
        "Nie udało się otworzyć pliku",
        "打开文件失败",
        "No se pudo abrir el archivo",
        "ファイルを開けませんでした",
        "Datei konnte nicht geöffnet werden",
    ),
    (
        "Failed to create archive",
        "Nie udało się utworzyć archiwum",
        "创建压缩文件失败",
        "No se pudo crear el archivo comprimido",
        "アーカイブを作成できませんでした",
        "Archiv konnte nicht erstellt werden",
    ),
    (
        "Unable to open archive",
        "Nie można otworzyć archiwum",
        "无法打开压缩文件",
        "No se puede abrir el archivo comprimido",
        "アーカイブを開けません",
        "Archiv kann nicht geöffnet werden",
    ),
    (
        "Failed to open",
        "Nie udało się otworzyć",
        "打开失败",
        "No se pudo abrir",
        "開けませんでした",
        "Öffnen fehlgeschlagen",
    ),
    (
        "Failed to extract",
        "Nie udało się rozpakować",
        "解压失败",
        "No se pudo extraer",
        "展開できませんでした",
        "Entpacken fehlgeschlagen",
    ),
    (
        "with the system default application.",
        "w domyślnej aplikacji systemu.",
        "（使用系统默认应用）。",
        "con la aplicación predeterminada del sistema.",
        "（システム既定のアプリを使用）。",
        "mit der Standardanwendung des Systems.",
    ),
    (
        "No archive path is available.",
        "Brak ścieżki archiwum.",
        "压缩文件路径不可用。",
        "No hay ninguna ruta de archivo comprimido disponible.",
        "アーカイブのパスがありません。",
        "Kein Archivpfad verfügbar.",
    ),
];

#[cfg(test)]
mod tests {
    use super::{Language, TRANSLATIONS};

    #[test]
    fn archive_errors_are_concise_and_localized() {
        let cases = [
            (
                "Invalid ZIP archive: Invalid ZIP Archive: Invalid zip archive",
                "The ZIP archive is invalid or damaged.",
            ),
            (
                "Failed to extract: Invalid TAR archive: unexpected end of file",
                "The archive is invalid or damaged.",
            ),
            (
                "Failed to open /tmp/password-invalid.zip: Permission denied (os error 13)",
                "Permission denied. Check your access to the file or folder.",
            ),
            (
                "Failed to open /tmp/password.zip: Invalid ZIP archive: Invalid zip archive",
                "The ZIP archive is invalid or damaged.",
            ),
            (
                "Failed to extract: No space left on device (os error 28)",
                "There is not enough free disk space.",
            ),
            (
                "Failed to open: No such file or directory (os error 2)",
                "The file or folder could not be found.",
            ),
            (
                "Failed to extract: The destination already exists",
                "The destination already exists. Choose another location.",
            ),
            (
                "Failed to open archive: Invalid password",
                "The archive could not be unlocked. Check the password and try again.",
            ),
            (
                "Failed to extract: Symbolic links in archives are not extracted",
                "The archive contains unsafe paths or links and cannot be extracted.",
            ),
            (
                "Failed to extract: Unsupported compression method",
                "This archive format or compression method is not supported.",
            ),
            (
                "Failed to extract: unrecognized library failure",
                "The operation could not be completed. Check the file and try again.",
            ),
        ];
        for language in Language::ALL {
            for (raw, key) in cases {
                assert_eq!(language.error_message(raw), language.text(key), "{raw}");
                if language != Language::English {
                    assert_ne!(language.error_message(raw), key);
                }
            }
            assert_eq!(
                language.error_message(
                    "AR archives support files only. Select individual files instead of folders."
                ),
                language.text(
                    "AR archives support files only. Select individual files instead of folders."
                ),
            );
            assert_eq!(
                language
                    .error_message(Language::Polish.text("Incorrect password or damaged archive")),
                language.text("Incorrect password or damaged archive"),
            );
        }
        assert_eq!(
            Language::Polish.error_message(cases[0].0),
            "Archiwum ZIP jest nieprawidłowe lub uszkodzone.",
        );
        for language in Language::ALL {
            for key in [
                "Install the rar command-line tool to modify RAR archives",
                "A single compressed stream cannot contain additional files",
                "The RAR tool could not update this archive",
                "This archive format cannot be modified",
            ] {
                let message = format!(
                    "{}: {}",
                    language.text("Failed to add to archive"),
                    language.text(key)
                );
                assert_eq!(language.error_message(&message), language.text(key));
                assert_eq!(
                    language.error_message(&format!("Failed to add to archive: {key}")),
                    language.text(key)
                );
            }
        }
    }

    #[test]
    fn invalid_zip_library_error_is_localized() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("damaged.zip");
        std::fs::write(&path, b"not an archive").unwrap();
        let error = crate::archive::list_archive(&path)
            .err()
            .expect("invalid ZIP must fail to open");
        let raw = format!("{error:#}");
        for language in Language::ALL {
            assert_eq!(
                language.error_message(&raw),
                language.text("The ZIP archive is invalid or damaged."),
                "{raw}",
            );
        }
    }

    #[test]
    /// Ensures every translation row provides a value for every supported language.
    fn all_translations_are_available_in_all_languages() {
        let mut keys = std::collections::BTreeSet::new();
        for &(english, polish, chinese, spanish, japanese, german) in TRANSLATIONS {
            assert!(keys.insert(english), "duplicate translation: {english}");
            assert!(!polish.is_empty());
            assert!(!chinese.is_empty());
            assert!(!spanish.is_empty());
            assert!(!japanese.is_empty());
            assert!(!german.is_empty());
            assert_eq!(Language::English.text(english), english);
            assert_eq!(Language::Polish.text(english), polish);
            assert_eq!(Language::ChineseSimplified.text(english), chinese);
            assert_eq!(Language::Spanish.text(english), spanish);
            assert_eq!(Language::Japanese.text(english), japanese);
            assert_eq!(Language::German.text(english), german);
            assert_ne!(chinese, english, "missing Chinese translation: {english}");
            assert_ne!(japanese, english, "missing Japanese translation: {english}");
        }
    }

    #[test]
    /// Checks that selector navigation wraps in both directions for every language.
    fn selector_cycles_through_all_languages_in_both_directions() {
        for language in Language::ALL {
            assert!(!language.name().is_empty());
            assert_eq!(language.adjacent(true).adjacent(false), language);
            assert_eq!(language.adjacent(false).adjacent(true), language);
            let mut next = language;
            let mut visited = Vec::new();
            for _ in Language::ALL {
                assert!(!visited.contains(&next));
                visited.push(next);
                next = next.adjacent(true);
            }
            assert_eq!(next, language);
        }
    }

    #[test]
    /// Verifies language-specific plural forms for item counts.
    fn item_counts_use_language_plural_rules() {
        assert_eq!(Language::English.item_count(1), "1 item");
        assert_eq!(Language::English.item_count(2), "2 items");
        assert_eq!(Language::Spanish.item_count(0), "0 elementos");
        assert_eq!(Language::Spanish.item_count(1), "1 elemento");
        assert_eq!(Language::Spanish.item_count(2), "2 elementos");
        assert_eq!(Language::German.item_count(1), "1 Element");
        assert_eq!(Language::German.item_count(0), "0 Elemente");
        assert_eq!(Language::German.item_count(2), "2 Elemente");
        for count in [0, 1, 2, 12, 100] {
            assert_eq!(
                Language::ChineseSimplified.item_count(count),
                format!("{count} 项")
            );
            assert_eq!(Language::Japanese.item_count(count), format!("{count} 個"));
        }
        for (count, noun) in [
            (0, "elementów"),
            (1, "element"),
            (2, "elementy"),
            (5, "elementów"),
            (12, "elementów"),
            (22, "elementy"),
            (112, "elementów"),
        ] {
            assert_eq!(
                Language::Polish.item_count(count),
                format!("{count} {noun}")
            );
        }
    }
}
