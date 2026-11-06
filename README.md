# Orchestrateur de charge Windows + WSL

`orchestrateur.exe` (Windows, icône de notification, petite fenêtre native) distribue les créneaux de
tâches lourdes dans l'ordre d'arrivée ; `charge` (WSL) les demande. Conception : `~/tmp/orchestrateur/PLAN.md`.

## Construire

```sh
export PATH=$HOME/.cargo/bin:$(echo ~/.local/share/uv/tools/cargo-zigbuild/lib/python*/site-packages/ziglang):$PATH
cargo build --release -j 4 --bin charge                                          # client Linux
cargo zigbuild --release -j 4 --target x86_64-pc-windows-gnu --bin orchestrateur # exe Windows
cargo test -j 4    # unitaires + intégration (serveur en version Linux sur socket Unix, mesures simulées)
```

## Transport

- L'exe écoute sur le tube nommé `\\.\pipe\orchestrateur` (ACL : le compte courant et SYSTEM, clients
  distants refusés). Une requête JSON par ligne, une réponse par ligne ; routes de la section 6 du plan.
- `charge` lance `orchestrateur.exe relais --tube <nom>` par l'interop WSL (stdin/stdout ↔ tube).
  Relais : code 69 si l'orchestrateur est absent (le client le lance alors une fois), 74 si le tube se ferme.
- Hors Windows (tests), le tube est une socket Unix `$XDG_RUNTIME_DIR/orchestrateur-<tube>.sock`.

## Installer

- Exe : `%LOCALAPPDATA%\Programs\orchestrateur\orchestrateur.exe`.
- État : `%LOCALAPPDATA%\orchestrateur\` (`etat.json`, `journal.log`, `config.toml` créé au premier démarrage).
- Client : copier `target/release/charge` dans le `PATH` de WSL.
- `windows/installer-tache.ps1` : tâche planifiée à l'ouverture de session (à lancer par Adel).

## Options

- `orchestrateur --dossier <chemin>` : autre dossier d'état (son `config.toml` choisit le tube).
- `--test` : rien à l'écran (ni icône, ni bulle, fenêtre hors écran) ; `--test-icone` : icône sans bulle.
- Client : `CHARGE_EXE`, `CHARGE_TUBE`, `CHARGE_DOSSIER` (pour le lancement automatique),
  `CHARGE_ARGS_LANCEMENT`, `CHARGE_SESSION`.
- Codes de `charge` : 0, 1 ticket sorti de la file, 2 erreur, 3 refus, 75 toujours en attente après `--max`
  ou feu vert repris avant l'accusé (relancer la même commande garde la place), 130 interrompu.
- Accusé de feu vert : `charge attend` et `charge run` envoient `accuse=1`, puis `POST /tickets/<n>/recu` une
  fois le feu vert transmis ; sans accusé dans `delai.rappel`, le feu vert est repris.

## Essais sous Windows réel (instance de test, rien à l'écran)

- `tests/essai-windows-api.ps1` : exe seul, client PowerShell sur le tube.
- `tests/essai-windows.sh` : bout en bout depuis WSL avec le vrai `charge` et le relais.
- `tests/capture-fenetre.ps1 <sortie.png>` : capture de la fenêtre composée hors écran.
