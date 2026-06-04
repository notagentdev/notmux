# Rebrand: vryn → notmux

Ziel: Produktname **„notmux"** (auch **„NotMux"** in Display-Kontexten). Repo-Pfad bleibt
`contember/vryn-ws` vorerst — es sei denn, du willst das Repo umbenennen (siehe
„Offene Entscheidungen" am Ende).

> **Hinweis:** Dieser Plan ist **Analyse + Vorgehen**, keine Ausführung. Ich fange
> erst an umzubenennen, wenn du explizit „los" sagst.

## 0. Naming-Mapping (kanonisch)

| Alt | Neu | Begründung |
|---|---|---|
| `Vryn` (Display) | `NotMux` | User-sichtbarer Produktname |
| `Vryn-WS` (Bundle-Name) | `NotMux` | Bundle-Name = Produktname |
| `vryn-ws` (crate, repo, dist, cask, URL-Pfad) | `notmux` | ein Token, durchgängig klein |
| `vrynws` (Binary) | `notmux` | Binary = CLI-Aufruf |
| `vryn_mobile_native` (crate) | `notmux_mobile_native` | analog |
| `vryn.yaml` (User-Datei) | `notmux.yaml` | siehe §6 Migrationsstrategie |
| `vryn.lock` (User-Datei) | `notmux.lock` | siehe §6 |
| `vryn.log` / `vryn.log.1` | `notmux.log` / `notmux.log.1` | siehe §6 |
| `vryn` (Service-Kind in `ApiServiceInfo.kind`) | `notmux` | siehe §6 |
| `vryn-ws` (Cargo crate root) | `notmux` | Workspace root package |
| Crate-Präfix `vryn-*` (19 Stück) | `notmux-*` | siehe §3 |
| `dev.vryn.ws` (Bundle-ID) | `dev.notmux.app` | Bundle-Identifier Konvention |
| `dev.vryn.ws.plist` | `dev.notmux.app.plist` | analog |
| `VRYN_*` (Hook-Env-Vars) | `NOTMUX_*` | siehe §6 |
| `VRYN_CONFIG_DIR` (Env-Override) | `NOTMUX_CONFIG_DIR` | siehe §6 |
| `~/.vryn-ws` (Config-Dir) | `~/.notmux` | siehe §6 (Migrationsstrategie wichtig!) |
| `~/.config/vryn-ws` (Doku/Linux) | `~/.config/notmux` | analog |
| `~/Library/Application Support/vryn-ws` (Cask zap) | `~/Library/Application Support/notmux` | analog |
| `~/Library/Caches/vryn-ws` | `~/Library/Caches/notmux` | analog |
| `contember/vryn-ws` (Repo-URL) | **nicht ändern** (bis du es sagst) | siehe „Offene Entscheidungen" |
| `vryn_token` (Web-LocalStorage) | `notmux_token` | siehe §6 |

## 1. Touch-Point-Inventar

Was alles „vryn" enthält, gruppiert nach Risiko:

### 1.1 Cargo-Workspace (kritisch — Compile-Blocker wenn falsch)
- **22 Crates** mit `name = "vryn-..."` (`vryn-core`, `vryn-files`, `vryn-git`, …).
- **Root package** `name = "vryn-ws"` (`Cargo.toml` Zeile 19).
- **Binary** `[[bin]] name = "vrynws"` (`Cargo.toml` Zeile 24).
- **Mobile crate** `name = "vryn_mobile_native"` (`mobile/native/Cargo.toml`).
- **Workspace members-Liste** (`Cargo.toml` Zeile 2–24).
- **Inter-Crate-`use`-Pfade** im gesamten Codebase:
  `use vryn_core::…`, `use vryn_git::…`, `use vryn_views_sidebar::…` etc.
- **`pub use vryn_*` Re-Exports** in `src/git/mod.rs:3`, `src/services/mod.rs:1`,
  `src/remote/types.rs:2,7`.

### 1.2 Code-Strings (kritisch — User-sichtbar + Logik)
- **CLI-Banner / Version**: `src/main.rs:308` `"vryn {}"`, `src/cli/mod.rs:46`
  `"Usage: vryn <command>"`.
- **Log-Dateinamen**: `src/main.rs:321-322` `vryn.log` / `vryn.log.1`.
- **Service-Kind-Konstante**: `crates/vryn-core/src/api.rs:83`
  `default_service_kind() → "vryn"`, sowie `api.rs:809` `kind: "vryn"`.
- **Settings-Panel-Anzeige**: `src/views/overlays/settings_panel/render_themes.rs:170`
  `.child("vryn")`.
- **Log-Panics-Kommentar**: `src/main.rs:340`.
- **App-Menu Items**: `src/main.rs:211,221,227` („Vryn", „About Vryn", „Quit Vryn").
- **Headless-Mode-Print**: `src/main.rs:263` `"Starting Vryn in headless mode..."`.
- **CLI-Fehler-Strings**: `src/cli/mod.rs:99,110,166,191` (mehrere „Vryn is not running…").
- **Pairing-Mode-Hint**: `src/app/headless.rs:268`.
- **Settings-Migration-Kommentar**: `crates/vryn-workspace/src/persistence.rs:42`
  „The `-ws` suffix is intentional: other unrelated tools called 'vryn' own `~/.vryn`…".
  → wird mit Rebrand obsolet, siehe §5.

### 1.3 macOS Bundle / Packaging
- **`Casks/vryn-ws.rb`** (komplette Cask-Definition, Name `vryn-ws`, Tap `contember/vryn-ws`).
- **`macos/Info.plist`** (`CFBundleName`, `CFBundleDisplayName`, `CFBundleIdentifier`).
- **`scripts/bundle-macos.sh`** (`APP_NAME`, `BUNDLE_ID`, `BIN_NAME`).
- **`macos/Info.plist` Copyright**: enthält `Contember` (bleibt vermutlich — siehe §8).

### 1.4 Linux / Windows Packaging
- **`vryn-ws.desktop`** (Name, Exec, Icon, StartupWMClass).
- **`install.sh`**: `REPO="contember/vryn-ws"`, `dist/vryn-ws-...zip` URLs.
- **`install.ps1`**: `$Repo`, `$InstallDir = "$env:LOCALAPPDATA\Programs\Vryn"`, `$BinName = "vrynws.exe"`.
- **`bi.sh`** (Windows-Installer-Script): falls da Bezug auf `Vryn` enthalten — noch zu prüfen.
- **`install-icon.sh`**: installiert `vryn.desktop` (zu pflegen).

### 1.5 CI / GitHub (kritisch — Build-Pipeline)
- **`.github/workflows/build.yml`** (mind. 25 Vorkommen): Artifact-Namen
  `vryn-ws-{macos,linux,windows,android,ios}-{arm64,x64,…}`, Binary-Copy `vrynws`,
  `homebrew-vryn-ws` Tap, `Casks/vryn-ws.rb` Pfad.
- **Release-URL** in `crates/vryn-ext-updater/src/checker.rs:28` und `status.rs:242`:
  `contember/vryn/releases/latest` und `…/tag/v{version}`.

### 1.6 Doku (User-sichtbar)
- **`README.md`** — fast komplett, Logo `Vryn`-Wortmarke, GitHub-URLs, `~/.local/bin/vryn`.
- **`docs/configuration.md`** — Pfad-Tabelle, Themes-Pfad.
- **`docs/remote.md`** — Token-Pfade `~/.config/vryn-ws/…`.
- **`docs/hooks.md`** — Env-Var-Tabelle `VRYN_*`.
- **`docs/services.md`** — `vryn.yaml` Schemadoku.
- **`docs/mobile-status.md`**, **`docs/worktrees.md`** — TBD-Inventur.
- **6 `CLAUDE.md` Dateien** (Root, `src/`, `crates/vryn-workspace/`, `mobile/`, `web/`,
  wahrscheinlich weitere) — Codebase-Internas, kann später oder parallel.
- **`STATUS.md`** — TBD-Inventur.

### 1.7 Web & Mobile
- **`web/`**: `package.json` (`vryn-web`), `index.html` `<title>Vryn</title>`,
  `bun.lock`, `src/auth/token.ts` (`vryn_token` / `vryn_token_expiry`),
  `src/components/PairingScreen.tsx` (`<h1>Vryn</h1>`).
- **`mobile/`**:
  - **Rust-Crate** `vryn_mobile_native` → `notmux_mobile_native`.
  - **CMakeLists** in `rust_builder/{windows,linux}/` (Paketname-Arg).
  - **Android `build.gradle`**: `libname = "vryn_mobile_native"`.
  - **Dart** `pubspec.yaml` (`name: vryn_mobile` o.ä.), `lib/src/widgets/project_drawer.dart`
    (Anzeige „Vryn"), `lib/src/screens/server_list_screen.dart`, `integration_test/simple_test.dart`.
  - **Icon-Assets** (sofern in `mobile/`).

### 1.8 Drittanbieter-Bezüge (nicht anfassen ohne Erlaubnis)
- GitHub-Repo `contember/vryn-ws` (Umbenennen ist Breaking Change für alle
  Clone-URLs, Forks, `homebrew-vryn-ws` Tap, evtl. bestehende Issues/PRs).
- GitHub-Releases URLs (Tag-History bleibt bestehen — neue Tags wären dann
  `v0.x.0` in `notmux`-Repo, das alte Repo bliebe für Migration erhalten).
- `homebrew-vryn-ws` Tap (Name).
- Apple Bundle-ID `dev.vryn.ws` — bei Notarisation/Code-Signing relevant
  (siehe §7).

## 2. Phasenplan (Reihenfolge)

Phasen sind **konservativ sequenziell** — jede Phase baut auf der vorigen auf
und sollte vor dem Merge buildable sein.

### Phase 1 — Reine String-/Config-Substitution (nichts kaputt)
1. **Workspace-Rename**: `Cargo.toml` Root-Package `vryn-ws` → `notmux`,
   Binary `vrynws` → `notmux`, `Cargo.lock` regenerieren.
2. **Crate-Renames** (alle 22): Verzeichnis `crates/vryn-core/` →
   `crates/notmux-core/` (auch `Cargo.toml` `name = "vryn-core"` → `name = "notmux-core"`).
3. **Interne `use`-Pfade** anpassen: jedes `use vryn_xxx::` → `use notmux_xxx::`,
   inkl. `pub use vryn_xxx::*` Re-Exports in `src/git/mod.rs`,
   `src/services/mod.rs`, `src/remote/types.rs`.
4. **Mobile Crate**: `mobile/native/Cargo.toml` (`vryn_mobile_native` →
   `notmux_mobile_native`), `rust_builder/{windows,linux}/CMakeLists.txt`,
   `rust_builder/android/build.gradle` (`libname`).
5. **Build verifizieren**: `cargo check --workspace --bins --tests`.

**Risiko Phase 1**: Niedrig. Reines Umbenennen, keine Logikänderungen. Wenn hier
was bricht, war es vorher schon kaputt.

### Phase 2 — Code-Strings (User-/Log-sichtbar)
1. `src/main.rs`: Version-Print, App-Menu (`"Vryn"`, `"About Vryn"`, `"Quit Vryn"`),
   Headless-Print, Log-Dateinamen `vryn.log` → `notmux.log`.
2. `src/cli/mod.rs`: Usage-String, Fehler-Strings.
3. `src/app/headless.rs`: Pairing-Hint.
4. `crates/vryn-core/src/api.rs` → `crates/notmux-core/src/api.rs`:
   `default_service_kind() → "notmux"`, `kind: "notmux"`.
5. `src/views/overlays/settings_panel/render_themes.rs:170`: `.child("notmux")`.
6. `crates/notmux-workspace/src/persistence.rs`: Kommentar zu „other tools called
   vryn" aktualisieren (siehe §5 — `-ws`-Suffix-Logik entfällt).

**Risiko Phase 2**: Niedrig, aber **Service-Kind-Wechsel ist Breaking Change für
bestehende `workspace.json`-Dateien**, falls dort jemand `kind: "vryn"` stehen
hat. → Migrationsstrategie: alter Wert „vryn" beim Laden als „notmux" lesen
(serde-`alias` oder Migration-Code, siehe §6).

### Phase 3 — Persistenz-Pfade & Migration
1. **Neue Config-Dir-Logik**: `get_config_dir()` liest
   `NOTMUX_CONFIG_DIR` (Env-Ovr), Default `~/.notmux` (Debug: `~/.notmux-dev`).
2. **Migration** (siehe §6):
   - Beim ersten Start nach Update: prüfen, ob `~/.notmux` fehlt **und**
     `~/.vryn-ws` existiert → einmalig verschieben (`mv`).
   - `vryn.lock`, `vryn.log`, `vryn.log.1`, `workspace.json`, `settings.json`,
     `cli.json`, `remote.json`, `remote_secret`, `remote_tokens.json`, `pair_code`,
     `themes/`, `sessions/` — werden verschoben.
   - `vryn.yaml` (User-Projekt-Datei) wird **nicht migriert** — User-Hinweis in
     Doku / Changelog.
3. **Migration-Reverse**: Falls `~/.notmux` existiert aber User alte Binary
   startet → auf neue Binary verweisen (kein Fallback nötig, wenn wir den
   Versions-Sprung in einem Major-Release machen).

**Risiko Phase 3**: Mittel. Wenn die Migration schiefgeht, verlieren User ihre
Workspaces. → Schreibtests mit gemockten `dirs::home_dir()` und Dry-Run-Modus
zwingend.

### Phase 4 — macOS Bundle + Casks
1. **`Casks/notmux.rb`** (neu) — basierend auf `Casks/vryn-ws.rb`, mit
   `cask "notmux"`, `name "NotMux"`, neuer Repo-URL, neuen Hashes (vom CI
   eingesetzt), `app "NotMux.app"`, neue `zap`-Pfade.
2. **`Casks/vryn-ws.rb`** — als Deprecated-Stub belassen (redirect auf neue
   Cask), damit `brew install --cask vryn-ws` weiterhin funktioniert bis zum
   Cutover. **Vorschlag:** erst in einem Folge-Release entfernen.
3. **`macos/Info.plist`**: `CFBundleName` → `NotMux`, `CFBundleDisplayName` →
   `NotMux`, `CFBundleIdentifier` → `dev.notmux.app` (siehe §7 — Apple-ID
   Switching).
4. **`scripts/bundle-macos.sh`**: `APP_NAME`, `BUNDLE_ID`, `BIN_NAME` anpassen.

**Risiko Phase 4**: Mittel-Hoch. Bundle-ID-Wechsel = App wird vom System als
komplett neue App erkannt (anderes Icon-Slot, andere Notification-Permissions,
anderes Auto-Update-Pfad via Sparkle o.ä.). **Siehe §7.**

### Phase 5 — Linux / Windows
1. **`notmux.desktop`** (neu) + altes `vryn-ws.desktop` löschen oder als
   Compat-Stub behalten.
2. **`install.sh`**: `REPO`, Asset-URLs (`vryn-ws-macos-…zip` →
   `notmux-macos-…zip`), Binary-Name.
3. **`install.ps1`**: `$Repo`, `$InstallDir`, `$BinName`.
4. **`bi.sh`**, **`install-icon.sh`**: Pfade und Binary-Namen.
5. **Updater-Pfade in `vryn-ext-updater`**: `contember/vryn` → `contember/notmux`
   (setzt voraus, dass Phase 7 (Repo-Umbenennung) stattgefunden hat — sonst
   bleiben die URLs auf v0.x.0-Releases im alten Repo).

### Phase 6 — CI & Release-Pipeline
1. **`.github/workflows/build.yml`**:
   - Artifact-Namen `vryn-ws-*` → `notmux-*`.
   - `cp target/.../release/vrynws dist/` → `notmux`.
   - `homebrew-vryn-ws` Tap → `homebrew-notmux`.
   - `Casks/vryn-ws.rb` → `Casks/notmux.rb`.
2. **Tag-Format**: bestehende Tags `v0.x.0` bleiben (im alten Repo), neue
   Tags `v0.x.0` (im neuen Repo) — kein Konflikt, da unterschiedliche Repos.
3. **`scripts/tag-version/run.sh`**: keine direkten `vryn`-Bezüge erkennbar —
   TBD-Verifikation.

### Phase 7 — Repo-Umbenennung (Breaking, separat)
**Nur auf dein Go.** Siehe „Offene Entscheidungen".

### Phase 8 — Doku & Polish
1. `README.md` (komplett).
2. `docs/configuration.md`, `docs/hooks.md`, `docs/remote.md`, `docs/services.md`.
3. 6× `CLAUDE.md` (root, `src/`, `crates/`, `mobile/`, `web/`, `crates/vryn-workspace/`).
4. `STATUS.md`.
5. Web: `web/index.html` `<title>`, `web/src/auth/token.ts` Storage-Keys
   (mit Fallback auf alten Key für existierende Sessions), `PairingScreen.tsx`,
   `package.json` (`name`, `description`), `bun.lock`-Update.
6. Mobile: Dart `pubspec.yaml`, Display-Strings, Integration-Tests.
7. Brand-Assets: `assets/logo.png`, `assets/app-icon-*.png` — **brauchen
   neues Design** (nicht nur String-Replace).

## 3. Konkrete Datei-Operations-Checkliste (Phase 1)

Das ist die mechanische „Search & Replace"-Liste für Phase 1. Ich liste hier
**bewusst** nur `vryn-*` Tokens (Crates) und `vryn` (root package), nicht die
User-Strings (Phase 2) und nicht die Pfade (Phase 3) — sonst vermischen sich
Verantwortlichkeiten.

### 3.1 Verzeichnis-Renames
```text
crates/vryn-core/                      → crates/notmux-core/
crates/vryn-ext-claude/                → crates/notmux-ext-claude/
crates/vryn-ext-codex/                 → crates/notmux-ext-codex/
crates/vryn-ext-updater/               → crates/notmux-ext-updater/
crates/vryn-extensions/                → crates/notmux-extensions/
crates/vryn-files/                     → crates/notmux-files/
crates/vryn-git/                       → crates/notmux-git/
crates/vryn-hooks/                     → crates/notmux-hooks/
crates/vryn-layout/                    → crates/notmux-layout/
crates/vryn-markdown/                  → crates/notmux-markdown/
crates/vryn-remote-client/             → crates/notmux-remote-client/
crates/vryn-services/                  → crates/notmux-services/
crates/vryn-state/                     → crates/notmux-state/
crates/vryn-terminal/                  → crates/notmux-terminal/
crates/vryn-theme/                     → crates/notmux-theme/
crates/vryn-ui/                        → crates/notmux-ui/
crates/vryn-views-git/                 → crates/notmux-views-git/
crates/vryn-views-remote/              → crates/notmux-views-remote/
crates/vryn-views-services/            → crates/notmux-views-services/
crates/vryn-views-sidebar/             → crates/notmux-views-sidebar/
crates/vryn-views-terminal/            → crates/notmux-views-terminal/
crates/vryn-workspace/                 → crates/notmux-workspace/
mobile/native/                         → bleibt (Paketname ändert sich nur intern)
```

### 3.2 Datei-Edits
| Datei | Was ändern |
|---|---|
| `Cargo.toml` (root) | `name = "vryn-ws"` → `name = "notmux"`; `[[bin]] name = "vrynws"` → `name = "notmux"`; alle `vryn-* = { path = "…" }` → `notmux-* = { path = "…" }`; `workspace.members` Liste |
| `crates/notmux-*/Cargo.toml` (×21) | `name = "vryn-…"` → `name = "notmux-…"`; `vryn-xxx = { path = "../vryn-xxx" }` → `notmux-xxx = { path = "../notmux-xxx" }` |
| `mobile/native/Cargo.toml` | `name = "vryn_mobile_native"` → `name = "notmux_mobile_native"`; `vryn-core = { path = "../../crates/vryn-core" }` → `notmux-core = { … }` |
| `Cargo.lock` | Wird durch `cargo build` regeneriert; **nicht manuell** anpassen. |
| `mobile/native/rust_builder/android/build.gradle` | `libname = "vryn_mobile_native"` → `libname = "notmux_mobile_native"` |
| `mobile/native/rust_builder/{windows,linux}/CMakeLists.txt` | Pfad/Name-Argumente in `apply_cargokit(…)` |

### 3.3 Inter-Crate `use`-Pfade (Bulk-Replace)
Im gesamten `src/`, `crates/`, `mobile/native/`, `web/`, `tests/` (per
`fs_search` zu verifizieren — keine Blindpatches):

```text
use vryn_core::       → use notmux_core::
use vryn_git::        → use notmux_git::
use vryn_hooks::      → use notmux_hooks::
use vryn_terminal::   → use notmux_terminal::
… (alle 19 Crates)
pub use vryn_…::*     → pub use notmux_…::*
```

`cargo build` ist hier der Wahrheits-Check — alles, was nicht aufgelöst wird,
wird der Compiler schon sagen.

## 4. Verifikations-Strategie pro Phase

| Phase | Check |
|---|---|
| 1 | `cargo check --workspace --bins --tests`; `cargo build --bin notmux`; `cargo test -p notmux-workspace` |
| 2 | `cargo build`, App starten, alle Menüs klicken, Log-Datei prüfen (`notmux.log`) |
| 3 | Unit-Tests mit `tempfile` und `dirs::home_dir`-Mock; manueller Migration-Smoke-Test mit Backup |
| 4 | `scripts/bundle-macos.sh --dmg`, App in `/Applications` ziehen, Signatur prüfen (`codesign -dv`), Sparkle-Castle falls vorhanden |
| 5 | `./install.sh` auf Linux-Container; `install.ps1` lokal |
| 6 | PR mit Workflow-Änderung → CI grün → Test-Release `v0.20.0-notmux-rc1` |
| 7 | Repo-Rename auf GitHub, alle Clone-URLs in Doku anpassen, bestehende `homebrew-vryn-ws` Tap weiterleiten |
| 8 | Manuell Docs gegenchecken, Brand-Designer für Logo beauftragen (außerhalb Code-Scope) |

## 5. Config-Dir-Logik (`get_config_dir`) — Designentscheidung

**Aktuell:**
```rust
// ~/.vryn-ws (release) bzw. ~/.vryn-ws-dev (debug)
// Override: VRYN_CONFIG_DIR
```

Begründung im Originalkommentar: *„other unrelated tools called 'vryn' own
`~/.vryn`"*. Mit Rebrand auf `notmux` ist die Kollisionsgefahr deutlich
geringer (niemand sonst heißt `notmux`), **aber** — wir sollten trotzdem nicht
in `~/.notmux` schreiben, weil:

1. `~` ist nicht-portabel (Windows: `%USERPROFILE%`, macOS/Linux: `$HOME`).
2. Tooling wie Syncthing, Dotfile-Repos scannen typischerweise `~/.*` und
   würden `notmux/` mitnehmen, was wir nicht wollen.
3. **XDG-Standard** unter Linux: `$XDG_CONFIG_HOME` (Default `~/.config`),
   **Apple-Standard** auf macOS: `~/Library/Application Support/<bundle-id>`.

**Vorschlag** (zur Bestätigung):
- **Linux**: `~/.config/notmux/` (XDG-konform, portabel).
- **macOS**: `~/Library/Application Support/dev.notmux.app/` (Apple-Standard,
   matcht die Bundle-ID — passt zum bestehenden Cask-zap-Pfad).
- **Windows**: `%APPDATA%\NotMux\` (entspricht `install.ps1`).
- **Debug-Suffix**: `…-dev` angehängt.
- **Env-Override**: `NOTMUX_CONFIG_DIR` (höchste Priorität).

Wenn du das so willst, fällt auch der `-ws`-Suffix und der zugehörige Kommentar
in `persistence.rs` weg. Migration siehe §6.

## 6. Migrationsstrategie (User-Daten)

### 6.1 Was wird migriert (Config-Dir, einmalig beim ersten Start)
- `workspace.json`, `settings.json`, `keybindings.json`, `cli.json`,
  `remote.json`, `remote_secret`, `remote_tokens.json`, `pair_code`,
  `themes/`, `sessions/`, `vryn.lock`, `vryn.log`, `vryn.log.1`.

**Mechanik** (in `get_config_dir()` oder `init()` früh aufgerufen):
```rust
fn maybe_migrate_from_vryn() -> io::Result<()> {
    let new_dir = get_config_dir();
    if new_dir.exists() { return Ok(()) } // schon migriert oder Neuinstall
    let old_dir = old_vryn_dir(); // ~/.vryn-ws bzw. ~/Library/Application Support/vryn-ws
    if !old_dir.exists() { return Ok(()) }
    fs::create_dir_all(new_dir.parent())?;
    fs::rename(&old_dir, &new_dir)?;
    log::info!("migrated config from {} to {}", old_dir.display(), new_dir.display());
    Ok(())
}
```

**Sicherheit**: Vor dem `rename` Backup-Kopie (`cp -R`) anlegen, bei Fehler
rollback. Mindestens in `v0.21.0` Warn-Toast anzeigen.

### 6.2 Was wird **nicht** migriert
- `vryn.yaml` (Projekt-Datei) — die muss der User pro Projekt umbenennen.
  → Migrations-Hinweis in Changelog + README.
- Web `localStorage` Keys `vryn_token` / `vryn_token_expiry` — User muss
  sich neu pairen. Alternativ: einmaliger Lese-Fallback in `web/src/auth/token.ts`
  (alter Key → neuer Key + `localStorage.removeItem`).
- `homebrew-vryn-ws` Tap — User muss auf neuen Tap wechseln.

### 6.3 Service-Kind-Migration
In `ApiServiceInfo.kind`: `"vryn"` → `"notmux"`. **Migrations-Code** in
`serde::Deserialize` per `#[serde(alias = "vryn")]` oder expliziter
`Deserialize` mit Fallback.

## 7. macOS Bundle-ID — Breaking-Change-Risiko

Wechsel von `dev.vryn.ws` auf `dev.notmux.app` heißt:
- **System sieht neue App**: separates Dock-Icon, separate Notification-Permissions,
  separate TCC-Permissions, separate Auto-Update-Kanal (sofern via Sparkle/Castle).
- **Code-Signing**: bestehender Developer-ID-Account müsste die neue ID
  signieren (oder wir bleiben beim alten Account).
- **Bestehende Installation**: bleibt unter `dev.vryn.ws` installiert, neue
  Version installiert sich parallel als `dev.notmux.app`. Beide können
  koexistieren.

**Vorschlag**: Neue Bundle-ID **vor dem ersten Release mit neuem Namen**
beantragen, damit es einen sauberen Schnitt gibt. **Davor nicht releasen.**

## 8. Brand-Assets

Was sich nicht per `sed` ersetzen lässt:
- `assets/logo.png` (Wortmarke „Vryn")
- `assets/app-icon-*.png` (16, 32, 48, 64, 128, 256, 512, 1024)
- `assets/app-icon.ico` (Windows)
- `assets/app-icon-1024.png` → `assets/AppIcon.iconset/...icns` (macOS)
- Flutter/Mobile-Assets (in `mobile/`)

→ Designer-Auftrag separat, **nicht** Teil dieses Plans. Bis dahin: altes Logo
mit `NotMux` überdrucken (hacky) oder mit „Vryn"-Icon ausliefern und nur den
Namen ändern (auch hacky). Empfehlung: **erst Assets, dann Release**.

## 9. Was ich **nicht** automatisch mache

- Keine `git mv`/`git reset`/`cargo fmt` ohne deine Bestätigung (AGENTS.md).
- Keine Releases / Tags pushen.
- Keine Homebrew-Tap-Änderungen (leben in separatem Repo
  `contember/homebrew-vryn-ws`).
- Kein GitHub-Repo-Rename (Phase 7).
- Keine Brand-Asset-Erstellung (Phase 8 designerabhängig).
- Keine `Cargo.lock`-Hand-Edits (cargo regelt das).

## 10. Offene Entscheidungen (brauche ich von dir)

1. **Repo umbenennen?** `contember/vryn-ws` → `contember/notmux` (separater
   Breaking-Change). **Mein Default-Vorschlag: ja, in Phase 7**, aber **erst
   nach** erfolgreichem Phase-1-Build, damit User mit bestehenden Clone-URLs
   nicht mitten im Refactor stranden.
2. **Cask-Tap umbenennen?** `contember/homebrew-vryn-ws` →
   `contember/homebrew-notmux`. Ja, wenn Repo umbenannt wird.
3. **Config-Dir-Form** (§5): XDG + Apple-Standard? Oder einfach `~/.notmux`
   lassen? **Mein Default-Vorschlag: XDG/Apple-Standard**, weil's der
   etablierte Weg ist.
4. **Bundle-ID-Form**: `dev.notmux.app` (kebab-case statt dotted) oder
   `dev.notmux.ws` (Suffix beibehalten)? **Mein Vorschlag: `dev.notmux.app`**,
   sauberer.
5. **Copyright-Zeile in Info.plist**: Bleibt `Copyright © 2026 Contember.`?
   Wenn ja, weil nur Produktname wechselt, nicht Inhaber.
6. **Breaking-Release-Version**: Springen wir auf `v0.21.0` (mit
   Migrations-Warnings) oder direkt auf `v1.0.0` (weil Rebrand)? Mein
   Vorschlag: **`v1.0.0`** — Rebrand ist semantisch ein Major-Sprung.
7. **Scope Web/Mobile**: In Phase 8 mit drin? Oder separater Plan?

## 11. Empfohlene Ausführungs-Reihenfolge (Konsolidiert)

```text
Phase 1: Cargo-Renames          ← Kompiliert am Ende
Phase 2: Code-Strings            ← Kompiliert + manueller Smoke-Test
Phase 3: Config-Dir + Migration  ← Unit-Tests, manueller Migration-Test
Phase 4: macOS Bundle + Casks    ← Bundle erstellen, Code-Sign prüfen
Phase 5: Linux/Windows Packaging ← Install-Skripte auf Linux/Win testen
Phase 6: CI                      ← Test-Release
Phase 7: Repo-Umbenennung        ← Separater PR, eigener Merge
Phase 8: Doku + Brand-Assets     ← Designer parallel
```

Jede Phase ist ein eigener PR / eigener Merge, damit jederzeit ein Rollback
möglich ist.

---

**Wenn du grünes Licht gibst, starte ich mit Phase 1 (Cargo-Renames) und
mache `cargo check --workspace` am Ende als Verifikation.** Vor Phase 7
(Repo-Umbenennung) und Phase 8 (Brand-Assets) frag ich nochmal explizit.
