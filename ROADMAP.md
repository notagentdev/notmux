# Roadmap

Geplante Erweiterungen der Git-/Diff-Integration. Fokus: den bestehenden
Diff-Viewer und die Agent-Hooks zu einem echten Code-Review-Workflow ausbauen.

## Agent-Turn-Diff

Diff dessen anzeigen, was der Agent im **letzten Turn** geändert hat, nicht nur
gegen `HEAD`.

- Baseline pro Agent-Session mitschreiben (Repo-Root + Commit/Worktree-Zustand
  zu Beginn des Turns).
- Im Diff-Viewer umschaltbar: „Änderungen dieses Turns" vs. „Änderungen gegen
  HEAD".
- Nutzt die vorhandenen Lifecycle-Hooks (`notmux-hooks`) als Trigger für das
  Setzen der Baseline.

## Review-Kommentare im Diff → an den Agenten

Kommentare direkt im Diff an Zeilenbereiche anheften und gesammelt als
Feedback an den Agenten zurückschicken — ein Review-Loop ohne den Umweg über
ein separates Fenster.

- Kommentar an Zeilenbereich verankern (Seite, Start-/Endzeile).
- Re-Anchoring über den Zeilentext, damit Kommentare bei verschobenen
  Zeilennummern erhalten bleiben.
- Sammel-Submission an die laufende Agent-Session.
- Baut auf dem editierbaren Diff-Editor und dem Sprung zur ersten Änderung auf,
  die es bereits gibt.

## PR-Status in der Sidebar

Neben dem Branch auch den Pull-Request-Status und die PR-Nummer je Projekt
anzeigen.

- GitHub-Anbindung (Status: open / merged / draft, Nummer, ggf. CI-Checks).
- Anzeige neben dem Branch-Namen in der Projektzeile der Sidebar.

## Git-Status über Remotes

Den Porcelain-Status auch für Remote-Projekte (SSH) ermitteln, damit
Statusfärbung in Sidebar und File-Explorer auch bei entfernten Repos greift.

- `git status --porcelain` non-locking über die Remote-Verbindung.
- Ergebnis in dieselbe Statusanzeige einspeisen wie lokale Repos.
