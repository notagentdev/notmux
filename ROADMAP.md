# Roadmap

Geplante Erweiterungen der Git-/Diff-Integration. Fokus: den bestehenden
Diff-Viewer und die Agent-Hooks zu einem echten Code-Review-Workflow ausbauen.

## Leitprinzip: Power-User-First

NotMux richtet sich an Power-User: nativer Terminal-Multiplexer, CLI-first,
Tastatur- und Kompositionsorientiert. Der Nutzer *baut* seine Arbeitsfläche
selbst, statt durch Assistenten geführt zu werden. Jedes Feature auf dieser
Roadmap wird an diesem Maßstab gemessen:

- **Zeit sparen, nicht entmündigen.** Ein Feature darf Abläufe verdichten und
  beschleunigen — es darf Kontrolle oder Sichtbarkeit nicht wegabstrahieren.
- **CLI- und tastaturfähig.** Alles Wichtige muss ohne Maus und skriptbar
  erreichbar sein.
- **Information verdichten statt verstecken.** Status inline zeigen, wo er
  gebraucht wird — keine parallelen In-App-Boards, die das Terminal ersetzen.
- **Bewusst nicht:** geführte Merge-Assistenten, In-App-Issue-Boards
  (Linear/Jira), Design-Mode-artige Klick-Handoffs. Das ist Hand-Holding für
  ein anderes Publikum und läuft der Positionierung zuwider.

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

## Parallele Agenten in Worktrees

Mehrere Agenten gleichzeitig auf je eigenem Branch/Worktree laufen lassen und
die Ergebnisse nebeneinander im Layout vergleichen — bewusst als *Komposition*,
nicht als geführter Assistent.

- Nutzt das vorhandene `create_worktree` + den Layout-Baum: N Worktrees anlegen,
  je ein Agenten-Pane im Split.
- Ein Prompt optional an alle offenen Agenten-Panes gleichzeitig senden
  (CLI/Tastatur), statt manuell in jedes Pane zu tippen.
- Splitscreen-Vergleich der Diffs; Merge bleibt eine bewusste Nutzeraktion
  (kein „Gewinner"-Assistent, der automatisch merged).
