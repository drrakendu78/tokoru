# Tokoru

> Un launcher de jeux desktop pour Windows qui unifie ta bibliothèque cross-platform (Steam, Epic, GOG, Ubisoft, EA, Itch.io) directement **dans Steam**.

Tokoru scanne tes launchers tiers, pousse les jeux comme raccourcis non-Steam dans `shortcuts.vdf`, et synchronise ton playtime réel (incluant les heures jouées hors Steam) dans `localconfig.vdf` — pour que ton profil Steam reflète enfin la totalité de ton temps de jeu.

**Statut** : alpha, build perso. Non distribué publiquement pour l'instant.

---

## Pourquoi

Steam est l'écosystème social-gaming de référence (succès, statistiques, amis, "in-game X"), mais sa bibliothèque ignore tout ce que tu n'as pas acheté chez Valve. Heroic, Playnite et SARM résolvent une partie du problème, mais aucun ne fait **les trois** en même temps :

1. **Scanner** les launchers tiers et leurs jeux installés (Epic Games, GOG Galaxy, Ubisoft Connect, EA App, Itch.io, plus n'importe quel `.exe` perso)
2. **Pousser** ces jeux dans Steam comme raccourcis non-Steam (avec artwork SteamGridDB, collections par plateforme, icônes propres)
3. **Réinjecter** le playtime réel dans Steam (`localconfig.vdf` + `Playtime2wks`) pour que la fierté du compteur de temps de jeu soit honnête

Tokoru fait les trois, sans abonnement, sans télémétrie, en local.

## Features

### Bibliothèque unifiée
- Scan auto : Steam, Epic Games (Legendary), GOG Galaxy (gogdl), Ubisoft Connect, EA App, Itch.io, Xbox Game Pass, Amazon Games
- Détection des jeux installés via registry + manifests + scan exe fallback
- Login API natif Steam (cookie `steamLoginSecure`) / Epic (OAuth via Legendary) / GOG (OAuth Galaxy)
- Multi-select + bulk actions (push N to Steam, fetch artwork, add tags)

### Sync vers Steam
- Écriture binaire de `shortcuts.vdf` (KeyValues1) avec collections par source
- Mirror artwork dans `userdata/<id>/config/grid/` : cover / hero / logo / icon
- Auto-restart Steam au push pour que les shortcuts apparaissent immédiatement
- Steam Favoris bidirectionnel (import + push) via `cloudstorage/cloud-storage-namespace-1.json`

### Playtime tracking
- Watcher de processus toutes les 5s, tracking session par session
- Import du `playtime_forever` Steam + Epic + GOG via API
- Spécial Star Citizen : parse les `logbackups/*.log` (les sessions RSI sont rotated)
- Sync vers `localconfig.vdf` (i32 signed, compat Steam Beta) + `Playtime2wks`
- Sticky manual playtime : valeur saisie main ne sera pas écrasée par les imports auto

### Metadata enrichies
- **Steam games** : Steam Store + SteamSpy + IGDB + HowLongToBeat + Wikidata
- **GOG games** : API GOG directe (`api.gog.com/products`) localisée
- **Autres** : recherche par nom sur Steam Store → fallback **RAWG.io** (500k+ jeux, couvre Star Citizen, Itch, retro)
- Achievements : Steam community XML + GOG Galaxy API
- Artworks : SteamGridDB (Cover, Hero, Logo, Icon) avec auto-fetch + manual picker

### Stats
- Heatmap d'activité 12 mois (style GitHub contributions)
- Top 10 par playtime, sessions over time
- Jour le plus actif, jeux jamais touchés
- Bloc Succès global cross-library

### UI
- React 18 + Tailwind v4 + Tauri 2
- Design système coral/graphite, acrylic Win11
- Dark + Light theme
- i18n complet : FR / EN / ES / DE / IT / PT / RU / ZH / JA / KO

## Stack

- **Frontend** : React 18, TypeScript, Vite, Tailwind v4, react-i18next, @tanstack/react-virtual
- **Backend** : Rust, Tauri 2, rusqlite (SQLite), reqwest, tokio
- **APIs externes** : Steam Web/Store/Community, Epic OAuth, GOG Galaxy, SteamGridDB, IGDB (Twitch OAuth), SteamSpy, HowLongToBeat, Wikidata, RAWG.io
- **CLIs embarqués** : Legendary (Epic), gogdl (GOG)

## Crédits / Inspirations

- [Boilr](https://github.com/PhilipK/BoilR) — la fondation pour comprendre `shortcuts.vdf` et Steam Collections
- [SARM](https://github.com/Tormak9970/Steam-Art-Manager) — patterns d'écriture grid/icon (le truc `.jpg` quoi qu'il arrive)
- [Heroic Launcher](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher) — la lib `gogdl` et les patterns Epic/GOG
- [Playnite](https://playnite.link/) — la philosophie "tout dans une seule lib"
- [aidesigner.ai](https://www.aidesigner.ai/) — pour le design des écrans

## Licence

MIT — voir [LICENSE](LICENSE).

## Non-affilié

Tokoru n'est **pas** affilié à Valve, Epic Games, GOG, Ubisoft, EA, Cloud Imperium Games, Microsoft, Amazon ou aucune des plateformes scannées. Les marques restent la propriété de leurs ayants droit.
