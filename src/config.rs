use std::path::Path;

use crate::format::{lire_duree, lire_ram};
use crate::modele::{Besoin, Cote};
use crate::protocole::TUBE_DEFAUT;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub tube: String,
    pub wsl_ram_max_mo: u64,
    pub cpu_total: u32,
    pub cpu_total_windows: Option<u32>,
    pub marge_ram_windows_mo: u64,
    pub marge_ram_wsl_mo: u64,
    pub marge_cpu: u32,
    pub defaut_wsl: Besoin,
    pub defaut_windows: Besoin,
    pub delai_attente_s: u64,
    pub delai_absence_s: u64,
    pub delai_rappel_s: u64,
    pub delai_actif_s: u64,
    pub delai_montee_s: u64,
    pub long_poll_s: u64,
    pub max_doublages: u32,
    pub attente_max_tete_s: u64,
    pub swap_seuil_pct: u64,
    pub swap_seuil_levee_pct: u64,
    pub swap_debit_sortie: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tube: TUBE_DEFAUT.into(),
            wsl_ram_max_mo: 12 * 1024,
            cpu_total: 10,
            cpu_total_windows: None,
            marge_ram_windows_mo: 1536,
            marge_ram_wsl_mo: 1536,
            marge_cpu: 2,
            defaut_wsl: Besoin { ram_mo: 2048, cpu: 2, duree_s: 15 * 60 },
            defaut_windows: Besoin { ram_mo: 4096, cpu: 4, duree_s: 30 * 60 },
            delai_attente_s: 90,
            delai_absence_s: 15 * 60,
            delai_rappel_s: 120,
            delai_actif_s: 60,
            delai_montee_s: 180,
            long_poll_s: 60,
            max_doublages: 3,
            attente_max_tete_s: 20 * 60,
            swap_seuil_pct: 90,
            swap_seuil_levee_pct: 85,
            swap_debit_sortie: 256,
        }
    }
}

pub const MODELE: &str = r#"# Orchestrateur de charge : lu au démarrage de orchestrateur.exe.
# Tube nommé \\.\pipe\<tube> ; `charge` le choisit par la variable CHARGE_TUBE.
tube = "orchestrateur"

# Plafond de la VM WSL (.wslconfig memory=) : une tâche WSL ne prend à Windows que la croissance restante
# de vmmemWSL jusqu'à ce plafond. Cœurs comptés.
wsl.ram_max = "12G"
cpu.total = 10
# Budget CPU Windows : par défaut, les cœurs logiques mesurés au démarrage. Une tâche WSL débite les deux budgets.
# cpu.total_windows = 16

marge.ram_windows = "1.5G"
marge.ram_wsl = "1.5G"
marge.cpu = 2

# Besoins appliqués quand `charge` ne précise pas --ram, --cpu ou --duree.
defaut.wsl.ram = "2G"
defaut.wsl.cpu = 2
defaut.wsl.duree = "15m"
defaut.windows.ram = "4G"
defaut.windows.cpu = 4
defaut.windows.duree = "30m"

# Attente : sautée (place gardée) sans client depuis delai.attente, retirée après delai.absence.
delai.attente = "90s"
delai.absence = "15m"
# Feu vert repris s'il n'a été remis à aucun client dans ce délai.
delai.rappel = "2m"
# `charge run` silencieux devient orphelin ; durée max d'une attente côté serveur.
delai.actif = "60s"
delai.long_poll = "60s"
# Montée en charge d'un actif : réservation entière pendant ce délai, puis seulement ce qu'il n'a pas encore pris
# (sa consommation est alors dans les mesures).
delai.montee = "3m"

# Remplissage : un ticket passe devant la tête bloquée s'il n'utilise pas ce qui lui manque ou finit avant
# son démarrage estimé ; au plus ce nombre de fois, et plus du tout après cette attente de la tête.
remplissage.max_doublages = 3
remplissage.attente_max = "20m"

# Swap WSL : pression seulement si rempli au-delà de swap.seuil (%) ET actif (sortie au-delà de swap.debit_sortie
# pages/s sur 30 s) ou MemAvailable sous marge.ram_wsl + 1 Go. Levée sous swap.seuil_levee ou après 60 s de calme.
swap.seuil = 90
swap.seuil_levee = 85
swap.debit_sortie = 256
"#;

impl Config {
    pub fn lire(texte: &str) -> Result<Config, String> {
        let mut c = Config::default();
        for (n, ligne) in texte.lines().enumerate() {
            let ligne = ligne.trim();
            if ligne.is_empty() || ligne.starts_with('#') || ligne.starts_with('[') {
                continue;
            }
            let erreur = |quoi: &str| format!("config.toml ligne {} : {quoi} ({ligne})", n + 1);
            let (cle, valeur) = ligne.split_once('=').ok_or_else(|| erreur("« = » attendu"))?;
            let cle = cle.trim();
            let valeur = valeur.split('#').next().unwrap_or("").trim().trim_matches('"');
            let ram = || lire_ram(valeur).ok_or_else(|| erreur("quantité de RAM illisible"));
            let entier = || valeur.parse::<u32>().map_err(|_| erreur("entier attendu"));
            let secondes = || lire_duree(valeur).ok_or_else(|| erreur("durée illisible"));
            match cle {
                "tube" => {
                    if valeur.is_empty() || !valeur.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        return Err(erreur("tube : lettres, chiffres, - et _ seulement"));
                    }
                    c.tube = valeur.to_owned();
                }
                "wsl.ram_max" => c.wsl_ram_max_mo = ram()?,
                "cpu.total" => c.cpu_total = entier()?,
                "cpu.total_windows" => c.cpu_total_windows = Some(entier()?),
                "marge.ram_windows" => c.marge_ram_windows_mo = ram()?,
                "marge.ram_wsl" => c.marge_ram_wsl_mo = ram()?,
                "marge.cpu" => c.marge_cpu = entier()?,
                "defaut.wsl.ram" => c.defaut_wsl.ram_mo = ram()?,
                "defaut.wsl.cpu" => c.defaut_wsl.cpu = entier()?,
                "defaut.wsl.duree" => c.defaut_wsl.duree_s = secondes()?,
                "defaut.windows.ram" => c.defaut_windows.ram_mo = ram()?,
                "defaut.windows.cpu" => c.defaut_windows.cpu = entier()?,
                "defaut.windows.duree" => c.defaut_windows.duree_s = secondes()?,
                "delai.attente" => c.delai_attente_s = secondes()?,
                "delai.absence" => c.delai_absence_s = secondes()?,
                "delai.rappel" => c.delai_rappel_s = secondes()?,
                "delai.actif" => c.delai_actif_s = secondes()?,
                "delai.montee" => c.delai_montee_s = secondes()?,
                "delai.long_poll" => c.long_poll_s = secondes()?,
                "remplissage.max_doublages" => c.max_doublages = entier()?,
                "remplissage.attente_max" => c.attente_max_tete_s = secondes()?,
                "swap.seuil" => c.swap_seuil_pct = entier()?.into(),
                "swap.seuil_levee" => c.swap_seuil_levee_pct = entier()?.into(),
                "swap.debit_sortie" => c.swap_debit_sortie = entier()?.into(),
                _ => return Err(erreur("clé inconnue")),
            }
        }
        Ok(c)
    }

    /// Lit `config.toml` du dossier d'état ; l'écrit avec les valeurs par défaut s'il manque et que `creer` est vrai.
    pub fn charger(dossier: &Path, creer: bool) -> Result<Config, String> {
        let chemin = dossier.join("config.toml");
        match std::fs::read_to_string(&chemin) {
            Ok(texte) => Config::lire(&texte),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if creer {
                    std::fs::write(&chemin, MODELE)
                        .map_err(|e| format!("écriture de {} : {e}", chemin.display()))?;
                }
                Ok(Config::default())
            }
            Err(e) => Err(format!("lecture de {} : {e}", chemin.display())),
        }
    }

    pub fn defaut(&self, cote: Cote) -> Besoin {
        match cote {
            Cote::Wsl => self.defaut_wsl,
            Cote::Windows => self.defaut_windows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_modele_redonne_les_valeurs_par_defaut() {
        assert_eq!(Config::lire(MODELE).unwrap(), Config::default());
    }

    #[test]
    fn surcharge_et_erreurs() {
        let c = Config::lire("tube = \"essai-1\"\ndelai.actif = \"3s\"\n").unwrap();
        assert_eq!(c.tube, "essai-1");
        assert_eq!(c.delai_actif_s, 3);
        assert!(Config::lire("prot = 1").unwrap_err().contains("clé inconnue"));
        assert!(Config::lire("marge.cpu = deux").is_err());
        assert!(Config::lire("tube = \"a b\"").is_err());
    }
}
