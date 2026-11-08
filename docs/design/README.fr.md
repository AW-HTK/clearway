# Clearway · référence visuelle

[English](README.md)

Les trois maquettes validées sont conservées ici avec leur implémentation :

- [Application](App.dc.html) : tickets, activité, ressources et sessions reliées.
- [Installateur](Setup.dc.html) : choix de maintenance et changement de version.
- [README GitHub](Main.dc.html) : hiérarchie éditoriale et identité du produit.

Les originaux proviennent de `~/tmp/orchestrateur/clearway/maquette/`. Ce sont des références graphiques avec des
données d'exemple, pas des captures du logiciel en fonctionnement. Les métadonnées du canevas sont dans `canvas.json`.

## Implémentation native

`src/visual.rs` définit la palette, les polices embarquées, les panneaux et les boutons des deux exécutables Windows.
Inter (normal et demi-gras) et JetBrains Mono sont chargées en mémoire pour le processus, sans installation sur le PC.
Segoe UI et Consolas servent de repli si GDI ne peut pas les charger. Sources et licences sont dans `assets/fonts/`.

La fenêtre conserve la barre de titre Windows et de vrais contrôles pour le clavier et les lecteurs d'écran. Les
cartes sont des éléments de liste dessinés sur mesure : sélection et défilement restent natifs ; les détails complets
du ticket sélectionné sont consultables et copiables dans une zone défilante. Annuler, libérer et forcer conservent
leurs confirmations. Ressources, alertes, activité et sessions proviennent du serveur, sans données de maquette.

L'installateur conserve la détection, l'installation, la réparation, la désinstallation et la vérification existantes.
Les cartes de maintenance contiennent des boutons radio natifs ; l'action principale est à droite. Il n'affirme pas
qu'une signature a été vérifiée du seul fait que l'installateur a été ouvert.

## Rendu des README

Les README utilisent le HTML accepté par GitHub pour le logo, le titre en texte et la navigation, puis des tableaux
Markdown et des sections techniques dépliables. Aucun CSS personnalisé, capture de page intégrée ou widget scripté.
Les anciennes bannières SVG restent disponibles comme éléments graphiques, mais ne servent plus d'en-tête.

## Construire un installateur local

Pour voir le rendu sans installer, `clearway-setup.exe --preview` ouvre le vrai écran de maintenance hors écran avec
des versions d'exemple. Installation, réparation, désinstallation et vérification sont bloquées dans ce mode, qui
prime sur les autres options de commande. `tests/visual-preview.ps1 -Project <dépôt> -Output <dossier>` capture les
deux exécutables natifs avec un nouvel état temporaire et un tube dédié, puis ferme ses processus.

Avec les outils de compilation dans le `PATH`, lancez `sh scripts/build-local.sh`. L'installateur produit dans
`dist/local/clearway-setup.exe` embarque les deux exécutables recompilés. C'est une compilation locale, sans signature
de release ; les fichiers signés existants dans `dist/` sont conservés. Le script ne lance aucun test ni installation.

`CLEARWAY_BUNDLE_DIR` permet de choisir un dossier complet à la place de `dist/` lors de la compilation du setup
Windows. Il doit contenir les exécutables Windows et Linux, les scripts d'installation/désinstallation, `VERSION` et
`SHA256SUMS`. Une compilation locale peut ainsi être empaquetée sans écraser les fichiers signés d'une version publiée.
Les versions officielles utilisent toujours `scripts/build-release.sh` et la signature du mainteneur.
