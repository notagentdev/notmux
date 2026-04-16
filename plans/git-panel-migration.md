# Plan: Git Commit Log — Popover → Rechtes Seitenpanel

## Context

Der Git Commit Log wird aktuell als Popover/Dropdown über dem `GitHeader`-Button angezeigt (`git_header.rs`). Das ist bei größeren Commit-Graphen unhandlich. Ziel: Den Commit Log in ein dauerhaftes, togglebares Panel auf der rechten Seite verschieben — analog zur linken Sidebar, aber einfacher (kein Auto-Hide nötig).

## Architektur-Überblick

```
Vorher:  [Sidebar] | [Project Columns]          + Commit-Log-Popover (overlay)
Nachher: [Sidebar] | [Project Columns] | [Git Panel]   (persistentes Panel)
```

Das `GitHeader`-Entity bleibt pro Projekt bestehen und behält seine Lade-/State-Logik. Das Panel zeigt den Commit Log des zuletzt aktivierten Projekts.

## Implementierungs-Schritte

### 1. Settings erweitern
**`crates/okena-workspace/src/settings.rs`**
- Neues `GitPanelSettings { is_open: bool, width: f32 }` (Default: `false`, `400.0`)
- Konstanten: `DEFAULT_GIT_PANEL_WIDTH = 400.0`, `MIN_GIT_PANEL_WIDTH = 250.0`, `MAX_GIT_PANEL_WIDTH = 700.0`
- Feld `git_panel: GitPanelSettings` zu `AppSettings` hinzufügen (`#[serde(default)]`)

**`src/settings.rs` (SettingsState)**
- `set_git_panel_open()` und `set_git_panel_width()` Methoden (analog zu Sidebar)

### 2. SidebarController wiederverwenden
`SidebarController` ist generisch genug (open/close, animation, width). Wird als zweite Instanz `git_panel_ctrl` im `RootView` genutzt — kein neuer Controller-Typ nötig.

### 3. DragState-Variante für Resize
**`crates/okena-views-terminal/src/layout/split_pane.rs`**
- `DragState::GitPanel` Variante hinzufügen
- `render_git_panel_divider()` Funktion (analog zu `render_sidebar_divider`, aber auf der linken Seite des Panels)

### 4. GitHeader API anpassen
**`crates/okena-views-git/src/git_header.rs`**
- Neue Methode `render_commit_log_panel_content()` — extrahiert den Inhalt (Header-Bar, Branch-Selector, Compare-Mode, Commit-Liste) **ohne** Popover-Wrapper/Backdrop/Anchoring
- `render_commit_log_popover()` delegiert intern an `render_commit_log_panel_content()` (Rückwärtskompatibilität, kann später entfernt werden)
- Public Getter: `is_commit_log_visible()`, `open_commit_log()`, `close_commit_log()`
- Commit-Log-Button `on_click` → statt direkt `toggle_commit_log()` aufzurufen, wird ein `OverlayRequest::ToggleGitPanel { project_id }` über den `RequestBroker` gesendet

### 5. Request-Broker erweitern
**`crates/okena-workspace/src/requests.rs`**
- Neue Variante: `OverlayRequest::ToggleGitPanel { project_id: String }`

### 6. RootView Integration
**`src/views/root/mod.rs`**
- Neue Felder:
  - `git_panel_ctrl: SidebarController`
  - `git_panel_project_id: Option<String>`
- Initialisierung aus `app_settings.git_panel`

**`src/views/root/git_panel.rs`** (neue Datei)
- `toggle_git_panel(project_id, cx)` — öffnet/schließt das Panel, setzt das aktive Projekt
- `close_git_panel(cx)`
- `animate_git_panel(target, cx)` — analog zu `animate_sidebar`
- `render_git_panel(cx)` — holt das `GitHeader`-Entity des aktiven Projekts aus `project_columns` und ruft `render_commit_log_panel_content()` auf

**`src/views/root/render.rs`**
- Layout erweitern: `sidebar | divider | main-area | git-panel-divider | git-panel`
- Git-Panel-Container: animierte Breite via `git_panel_ctrl.current_width()`, Overflow hidden
- `DragState::GitPanel` im `on_mouse_move`-Handler: `window_width - mouse_x = new_width`

**`src/views/root/handlers.rs`**
- `ToggleGitPanel`-Request verarbeiten → `self.toggle_git_panel()`

### 7. ProjectColumn: Git-Header-Zugriff
**`src/views/panels/project_column.rs`**
- `pub fn git_header(&self) -> Entity<GitHeader>` Accessor hinzufügen
- `render_commit_log_popover()`-Aufruf entfernen (das Popover wird nicht mehr gebraucht)

### 8. Keybinding
**`src/keybindings/`**
- `ToggleGitPanel` Action definieren
- Default-Binding: `cmd-shift-g` (bzw. `ctrl-shift-g` auf Linux/Windows)

## Kritische Dateien

| Datei | Änderung |
|-------|----------|
| `crates/okena-workspace/src/settings.rs` | GitPanelSettings, Konstanten |
| `src/settings.rs` | Setter-Methoden |
| `crates/okena-views-terminal/src/layout/split_pane.rs` | DragState::GitPanel, Divider |
| `crates/okena-views-git/src/git_header.rs` | Panel-Content extrahieren, Public API |
| `crates/okena-workspace/src/requests.rs` | ToggleGitPanel Variante |
| `src/views/root/mod.rs` | Neue Felder, Modul |
| `src/views/root/git_panel.rs` | **Neue Datei** — Toggle/Animate/Render |
| `src/views/root/render.rs` | Layout, Resize-Handler |
| `src/views/root/handlers.rs` | Request-Verarbeitung |
| `src/views/panels/project_column.rs` | Accessor, Popover entfernen |
| `src/keybindings/` | Action + Default-Binding |

## Besonderheiten

- **Entity-Sharing**: `ProjectColumn` besitzt das `GitHeader`-Entity. Das Panel hält einen `Entity<GitHeader>`-Handle (GPUI ist reference-counted) — kein Ownership-Konflikt.
- **Projekt-Wechsel**: Wenn der User auf den Git-Button eines anderen Projekts klickt, wechselt das Panel zum neuen Projekt. Kein automatischer Wechsel bei Fokus-Änderung.
- **Sizing**: Das Popover war 520×420px fix. Das Panel nutzt volle Höhe und konfigurierbare Breite — die `render_commit_log_panel_content()` Methode entfernt feste Größen.

## Verifikation

1. `cargo build` — kompiliert ohne Fehler
2. `cargo test` — alle Tests bestehen
3. Manuell testen:
   - Git-Button in Project-Header klickt → rechtes Panel öffnet/schließt
   - Branch-Selector, Compare-Mode, Commit-Klick → Diff-Viewer funktioniert
   - Panel-Breite per Drag resizen
   - `cmd-shift-g` Keybinding togglet Panel
   - Panel-State wird in `settings.json` persistiert
   - Bei mehreren Projekten: Button wechselt das angezeigte Projekt
