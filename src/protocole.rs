use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TUBE_DEFAUT: &str = "orchestrateur";
pub const MAX_LIGNE: usize = 256 * 1024;

/// Codes de sortie de `orchestrateur.exe relais`.
pub const RELAIS_ABSENT: i32 = 69;
pub const RELAIS_PERDU: i32 = 74;

/// Une requête par ligne JSON : les routes logiques de la section 6, sans HTTP.
/// Avec un `id`, elle est traitée en parallèle des autres et sa réponse porte le même `id`, dans n'importe quel ordre ;
/// sans `id`, elle est traitée avant de lire la suivante (anciens clients).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Requete {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub methode: String,
    pub chemin: String,
    #[serde(default)]
    pub parametres: BTreeMap<String, String>,
    #[serde(default)]
    pub corps: Value,
}

impl Requete {
    pub fn nouvelle(methode: &str, chemin: &str, corps: Value) -> Requete {
        Requete { id: None, methode: methode.into(), chemin: chemin.into(), parametres: BTreeMap::new(), corps }
    }

    pub fn segments(&self) -> Vec<&str> {
        self.chemin.split('/').filter(|s| !s.is_empty()).collect()
    }

    pub fn parametre(&self, nom: &str) -> Option<&str> {
        self.parametres.get(nom).map(String::as_str)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub code: u16,
    #[serde(default)]
    pub corps: Value,
}

/// Lit une ligne d'au plus `MAX_LIGNE` octets ; `None` en fin de flux.
pub fn lire_ligne(r: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut s = String::new();
    let n = r.take(MAX_LIGNE as u64 + 1).read_line(&mut s)?;
    if n == 0 {
        return Ok(None);
    }
    if n > MAX_LIGNE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ligne trop longue"));
    }
    Ok(Some(s.trim_end_matches(['\r', '\n']).to_owned()))
}

/// Hors Windows (tests), le tube nommé est remplacé par une socket Unix.
#[cfg(unix)]
pub fn chemin_socket(tube: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("orchestrateur-{tube}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour() {
        let mut r = Requete::nouvelle("GET", "/tickets/12/attente", Value::Null);
        r.parametres.insert("connu".into(), "attente-1".into());
        let ligne = serde_json::to_string(&r).unwrap();
        let mut entree = format!("{ligne}\n").into_bytes();
        entree.extend_from_slice(b"{\"methode\":\"GET\",\"chemin\":\"/etat\"}\n");
        let mut lecteur = &entree[..];
        let lue: Requete = serde_json::from_str(&lire_ligne(&mut lecteur).unwrap().unwrap()).unwrap();
        assert_eq!(lue.segments(), vec!["tickets", "12", "attente"]);
        assert_eq!(lue.parametre("connu"), Some("attente-1"));
        let deux: Requete = serde_json::from_str(&lire_ligne(&mut lecteur).unwrap().unwrap()).unwrap();
        assert_eq!(deux.chemin, "/etat");
        assert!(lire_ligne(&mut lecteur).unwrap().is_none());
        assert!(!ligne.contains("\"id\""), "sans id, rien n'est ajouté pour les anciens serveurs : {ligne}");
        let avec: Requete = serde_json::from_str("{\"id\":7,\"methode\":\"GET\",\"chemin\":\"/etat\"}").unwrap();
        assert_eq!(avec.id, Some(7));
    }

    #[test]
    fn ligne_trop_longue() {
        let brut = vec![b'a'; MAX_LIGNE + 10];
        assert!(lire_ligne(&mut &brut[..]).is_err());
    }
}
