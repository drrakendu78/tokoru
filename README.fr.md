<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="public/logo-dark.png" />
    <source media="(prefers-color-scheme: light)" srcset="public/logo-light.png" />
    <img src="public/logo-dark.png" alt="Tokoru" width="240" />
  </picture>
</p>

<p align="center">
  <strong>Le launcher de jeux unifié pour Windows</strong>
</p>

<p align="center">
  Scanne tous tes launchers (Steam, Epic, GOG, Ubisoft, EA, Itch.io), pousse tes jeux dans Steam comme raccourcis non-Steam, et resynchronise ton temps de jeu cross-platform réel dans ton profil Steam.<br/>
  Sans abonnement. Sans télémétrie. 100 % local.
</p>

<p align="center">
  <a href="https://github.com/drrakendu78/tokoru/blob/main/LICENSE"><img src="https://img.shields.io/github/license/drrakendu78/tokoru?style=flat-square" alt="License" /></a>
  <a href="https://github.com/drrakendu78/tokoru/commits/main"><img src="https://img.shields.io/github/last-commit/drrakendu78/tokoru?style=flat-square&color=blue" alt="Last Commit" /></a>
  <img src="https://img.shields.io/badge/statut-alpha-orange?style=flat-square" alt="Alpha" />
  <img src="https://img.shields.io/badge/plateforme-Windows-0078D6?style=flat-square&logo=windows" alt="Windows" />
</p>

<p align="center">
  <a href="README.md">🇬🇧 English</a> · <strong>🇫🇷 Français</strong>
</p>

<p align="center">
  <img src="docs/screenshots/library.png" alt="Tokoru — vue bibliothèque unifiée Steam, Epic, GOG, Ubisoft, EA" width="820" />
</p>

---

## Fonctionnalités

- **Scan multi-launcher** - Détection automatique des jeux installés depuis Steam, Epic Games (Legendary), GOG Galaxy (gogdl), Ubisoft Connect, EA App, Itch.io, Xbox Game Pass et Amazon Games. Ajout manuel d'`.exe` perso aussi.
- **Push vers Steam comme raccourcis** - Écriture de `shortcuts.vdf` avec collections par source (Epic Games / GOG Galaxy / Ubisoft / ...) et badges colorés par plateforme. Push multi-sélection en batch avec redémarrage automatique de Steam.
- **Sync playtime cross-platform** - Tracking session par session local, import du `playtime_forever` depuis les APIs Steam/Epic/GOG, écriture dans `localconfig.vdf` de Steam pour que ton profil reflète enfin le temps de jeu total réel.
- **Picker artwork SteamGridDB** - Choix manuel cover / hero / logo / icon par jeu avec filtres (statique / animé / NSFW). Auto-fetch à l'ajout, mirror dans le dossier grid de Steam pour que Steam lui-même affiche le nouvel art.
- **Enrichissement metadata** - Steam Store + SteamSpy + IGDB + HowLongToBeat + Wikidata pour les jeux Steam. API GOG directe pour les jeux GOG. Fallback RAWG.io (500k+ titres) pour tout le reste, incluant Star Citizen et les jeux Itch-only.
- **Succès** - Vue unifiée cross-launcher : XML community Steam + API GOG Galaxy. Page de stats globales agrège la progression sur toute la bibliothèque.
- **Sync Favoris Steam** - Bi-directionnel entre le cœur ❤️ de Tokoru et la collection Favoris de Steam. Restart auto pour application immédiate.
- **Playtime Star Citizen** - Parse les `logbackups/*.log` sur tous les channels (LIVE / PTU / EPTU) pour calculer le vrai temps joué même quand le RSI Launcher fait tourner les vieux logs.
- **Dashboard stats** - Heatmap d'activité 12 mois (style contributions GitHub), top 10 par playtime, sessions dans le temps, jour le plus actif, jeux jamais touchés, succès globaux.
- **Multi-select + bulk actions** - Push N vers Steam, Récupérer les artworks pour la sélection, Ajouter des tags custom en 1 click.
- **Tags personnalisés** - Tags perso par jeu, éditables via modal chips, accessibles dans le filtre sidebar de la Library.
- **Theme Dark + Light** - Switch complet avec badges artwork theme-aware et chrome acrylique.
- **i18n** - UI 100% traduite en 10 langues : FR / EN / ES / DE / IT / PT / RU / ZH / JA / KO.

## Quoi de neuf

- Layout 3 onglets pour Game Detail (Vue d'ensemble / Artworks / Propriétés techniques).
- Barre d'actions multi-sélection dans la Library avec auto-restart au push.
- Fetch metadata native GOG via `api.gog.com/products`.
- Fallback universel RAWG.io qui couvre les jeux absents des stores majeurs.
- Override icône Steam pour les shortcuts (truc SARM `.jpg`-forcé en grid + update du champ icon dans `shortcuts.vdf`).
- Collection Favoris Steam créée automatiquement si elle n'existe pas.

## Installation

### Build manuel (seul chemin pour l'instant — alpha, pas de release publique)

Voir [Compilation depuis les sources](#compilation-depuis-les-sources) plus bas.

Un installeur Windows signé + un auto-updater sont prévus dans la roadmap (Phase 4).

## Démarrage rapide

1. **Lance Tokoru** — premier démarrage = wizard onboarding.
2. **Connecte-toi à Steam / Epic / GOG** depuis la page Sources (chacun à la suite — Steam en premier parce qu'on utilise le cookie pour tout le reste).
3. **Scan ta bibliothèque** — Tokoru détecte les jeux installés + pull ta catalogue owned-but-not-installed depuis Epic et GOG.
4. **Push vers Steam** — multi-sélectionne tes favoris et clique `Envoyer N à Steam`. Steam redémarre tout seul pour prendre en compte les nouveaux shortcuts.
5. **Ouvre un jeu** pour voir les metadata complètes, le picker d'artwork, et la progression succès.

## Stack technique

| Composant | Technologie |
|-----------|-----------|
| Framework | [Tauri 2](https://v2.tauri.app/) |
| Frontend | React 18 + TypeScript + Vite |
| Style | Tailwind CSS v4 |
| Backend | Rust + Tokio |
| Base de données | SQLite (rusqlite) |
| i18n | react-i18next (10 langues) |
| Virtualisation | @tanstack/react-virtual |
| CLIs embarqués | [Legendary](https://github.com/derrod/legendary) (Epic) + [gogdl](https://github.com/Heroic-Games-Launcher/heroic-gogdl) (GOG) |
| APIs externes | Steam Web/Store/Community, Epic, GOG, SteamGridDB, IGDB, SteamSpy, HowLongToBeat, Wikidata, RAWG.io |

## Compilation depuis les sources

### Prérequis

- [Node.js](https://nodejs.org/) (v18+)
- [Rust](https://www.rust-lang.org/tools/install) (stable)
- [Tauri CLI](https://v2.tauri.app/start/prerequisites/)

### Étapes

```bash
# Cloner le dépôt
git clone https://github.com/drrakendu78/tokoru.git
cd tokoru

# Installer les dépendances
npm install

# Lancer en développement
npm run tauri dev

# Compiler pour production
npm run tauri build
```

## Crédits / Inspirations

- [Boilr](https://github.com/PhilipK/BoilR) — format binaire `shortcuts.vdf` + Steam Collections.
- [SARM](https://github.com/Tormak9970/Steam-Art-Manager) — patterns d'écriture grid / icon (le truc `.jpg`-quoi-qu'il-arrive).
- [Heroic Games Launcher](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher) — `gogdl` + patterns Epic/GOG.
- [Playnite](https://playnite.link/) — la philosophie "tout dans une seule bibliothèque".
- [aidesigner.ai](https://www.aidesigner.ai/) — itérations de design des écrans.

## Contribuer

Tokoru est en alpha, développement perso actif. Les issues et PR sont les bienvenues.

## Licence

Ce projet est sous [licence MIT](LICENSE).

## Non-affiliation

Tokoru n'est **pas** affilié à Valve, Epic Games, GOG, Ubisoft, EA, Cloud Imperium Games, Microsoft, Amazon, ni aucune des plateformes scannées. Les marques restent la propriété de leurs ayants droit.

---

<p align="center">
  Fait avec Rust et React par <a href="https://github.com/drrakendu78">Drrakendu78</a>
</p>
