<p align="center">
  <img src="assets/gitbro.svg" width="96" alt="GitBro-Logo">
</p>

<h1 align="center">GitBro</h1>

<p align="center">
  Ein schlanker, schneller Git-Client – <strong>explizit mit und für <a href="https://omarchy.org">Omarchy</a> gebaut</strong>.<br>
  Den ganzen Graphen sehen, dann in einem Schritt committen und pushen.
</p>

<p align="center">
  <a href="README.md">English</a> · <a href="LICENSE">0BSD-Lizenz</a>
</p>

![Der Commit-Graph mit Branches, Tags, Stash und den aktuellen Änderungen (WIP)](docs/screenshots/history.png)

GitBro ist ein natives Desktop-Programm in Rust mit [gpui](https://www.gpui.rs) (dem UI-Framework hinter Zed).
Es ist **für Omarchy und Hyprland** gemacht: Es folgt live deinem Omarchy-Theme, nutzt deine Omarchy-Monospace-Schrift in Diffs,
startet sofort und fühlt sich an wie ein Teil des Desktops statt wie eine schwere IDE.

GitBro ist bewusst meinungsstark. Git hat viel Zeremonie, GitBro fasst den Alltag in möglichst wenige Schritte zusammen:

> **WIP-Zeile auswählen → Nachricht schreiben → _Commit & Push_.**

Keine Staging Area, kein separater Push.

> **Hinweis:** Die Oberfläche ist auf Englisch.

## Funktionen

- **Alles auf einen Blick** – Graph aller Branches, Remote-Branches, Tags und Stashes mit Ahead/Behind-Zähler. Nicht committete Änderungen erscheinen oben als **WIP**-Zeile.
- **Commit in einem Schritt** – alles Geänderte (auch neue Dateien) wird committet und sofort gepusht. Beim ersten Push eines neuen Branches wird der Upstream gesetzt. Ohne Remote wird nur committet.
- **Amend** – Häkchen bei „Amend last commit“, die letzte Nachricht wird vorausgefüllt. Liegt der Commit schon auf dem Remote, wird der Button gelb und sagt es dir: Der Push nutzt dann `--force-with-lease` und überschreibt nie Arbeit, die jemand anderes inzwischen gepusht hat.
- **Alles verwerfen** – ein Button, immer mit Bestätigung. Setzt getrackte Dateien zurück, löscht neue und beendet einen offenen Merge. Ignorierte Dateien bleiben.
- **Stash** – inklusive neuer Dateien und mit optionalem Namen; Anwenden, Entnehmen und Löschen in der Sidebar.
- **Branches** – anlegen im ⋯-Menü des aktuellen Branches, Doppelklick zum Auschecken, Mergen und Löschen im ⋯-Menü. Vor dem Löschen siehst du, wie viele Commits _nur_ auf diesem Branch existieren.
- **Sicherer Pull** – holt alles, räumt auf und spult vor. Es entsteht nie ein überraschender Merge-Commit; sind die Historien auseinandergelaufen, sagt GitBro es dir.
- **Diff im Vollbild** – Dateiliste, Zeilennummern, Binärerkennung und „Newer“/„Older“ zum Blättern.
- **Kleinigkeiten** – zuletzt geöffnete Repos (`Strg+R`), das letzte Repo öffnet automatisch, ein normaler Ordner wird mit einem Klick zum Repository (Standard-Branch `main`), und die Ansicht aktualisiert sich, sobald das Fenster wieder den Fokus bekommt.

| Commit & Push in einem Schritt | Letzten Commit ändern |
| --- | --- |
| ![Commit-Nachricht, geänderte Dateien, Button „Commit & Push“](docs/screenshots/commit-and-push.png) | ![Amend mit vorausgefüllter Nachricht](docs/screenshots/amend.png) |

| Commit ansehen | Neuer Branch |
| --- | --- |
| ![Diff eines Commits im Vollbild](docs/screenshots/commit-diff.png) | ![Der Dialog für einen neuen Branch](docs/screenshots/new-branch.png) |

## Omarchy-Integration

- **Live-Theme** – die Farben kommen aus dem aktiven Omarchy-Theme (`~/.local/state/omarchy/current/theme/colors.toml`). Theme wechseln, und GitBro zieht innerhalb einer Sekunde nach, ohne Neustart. Ohne Omarchy gilt Catppuccin Mocha.
- **Schrift** – Diffs nutzen die Schrift aus `omarchy font current`.
- **Hyprland-tauglich** – Wayland-nativ, mit eigener App-ID (`gitbro`), Icon und `.desktop`-Eintrag; Starter und Window-Rules funktionieren.
- **Eckig und flach** – eckige Kanten und dünne Rahmen passend zum Omarchy-Look.

## Installation

Du brauchst eine Rust-Toolchain ([rustup](https://rustup.rs) oder [mise](https://mise.jdx.dev), das bringt Omarchy mit) und `git` im `PATH`.

```bash
git clone https://github.com/pixelriot/GitBro.git
cd GitBro
./install.sh
```

Das Skript baut eine Release-Version und installiert sie für den aktuellen Benutzer:

- `~/.local/bin/gitbro`
- Icons unter `~/.local/share/icons/hicolor/`
- `~/.local/share/applications/gitbro.desktop`

Du kannst es jederzeit für Updates erneut ausführen, auch bei laufendem GitBro. Starten über den Launcher oder mit `gitbro [Pfad-zum-Repo]`.

Nur ausprobieren: `cargo run --release -- /pfad/zum/repo`.

## Tastenkürzel

| Taste | Aktion |
| --- | --- |
| `↑` / `↓` | Durch Commits (bzw. Dateien im Diff) wandern |
| `Enter` | Ausgewählten Commit öffnen / Dialog bestätigen |
| `Esc` | Dialog oder Menü schließen, dann den Diff |
| `Strg+Enter` | Committen & Pushen aus dem Nachrichtenfeld |
| `Strg+R` | Zuletzt geöffnete Repositories |
| `Strg+O` | Repository öffnen |

## Wie es funktioniert

- **Lesen** läuft über [libgit2](https://libgit2.org) (Crate `git2`), das hält Graph und Diffs auch bei großen Repos schnell.
- **Schreiben** (Commit, Push, Stash, Checkout, Merge, …) ruft das echte `git` auf. SSH-Agent, Credential-Helper, Hooks und Filter wie Git LFS verhalten sich exakt wie im Terminal.
- Git läuft nicht interaktiv: Es fragt nie im Terminal nach und öffnet nie einen Editor. Ein fehlendes Passwort wird zur Fehlermeldung statt zum Hänger.

## Stand

GitBro ist jung und zuerst für den eigenen Alltag gebaut. Es kann, was oben steht, und lässt bewusst vieles weg: kein Rebase, Cherry-Pick, keine Submodule-Oberfläche und kein Hunk-Staging. Eine **Oberfläche für Merge-Konflikte gibt es noch nicht** (ein Merge mit Konflikten lässt sich in der Toolbar abbrechen, Committen ist bis zur Lösung der Konflikte gesperrt).
Ideen und Pull Requests sind willkommen.

## Entwicklung

```bash
cargo run -- /pfad/zum/repo   # Debug-Build (Abhängigkeiten sind optimiert, bleibt flott)
cargo test                    # Git-Verhalten wird gegen echte temporäre Repos getestet
```

## Lizenz

[0BSD](LICENSE) – mach damit, was du willst, ohne Namensnennung.

Gemeinsam mit [Claude Code](https://claude.com/claude-code) entstanden.
