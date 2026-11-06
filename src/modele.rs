use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cote {
    Wsl,
    Windows,
}

impl Cote {
    pub fn nom(self) -> &'static str {
        match self {
            Cote::Wsl => "wsl",
            Cote::Windows => "windows",
        }
    }

    pub fn lire(s: &str) -> Option<Cote> {
        match s {
            "wsl" => Some(Cote::Wsl),
            "windows" => Some(Cote::Windows),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EtatTicket {
    Attente,
    Actif,
    Orphelin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CauseOrphelin {
    Silence,
    Bail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Besoin {
    pub ram_mo: u64,
    pub cpu: u32,
    pub duree_s: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ticket {
    pub id: u64,
    pub requete_id: String,
    pub cote: Cote,
    pub besoin: Besoin,
    pub description: String,
    pub session: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<u32>,
    pub dossier: String,
    pub signature: String,
    pub etat: EtatTicket,
    pub cree: u64,
    pub accorde: Option<u64>,
    pub dispo_accord_mo: Option<u64>,
    /// Ticket de `charge demande` : pas de superviseur, la durée déclarée sert de bail.
    #[serde(default)]
    pub manuel: bool,
    /// Le feu vert a été remis à un client (réponse de création, d'attente ou signal de présence).
    #[serde(default)]
    pub recu: bool,
    /// Feu vert rappelé faute d'avoir été utilisé : la place est gardée, le ticket est sauté jusqu'au retour du client.
    #[serde(default)]
    pub rappel: Option<u64>,
    #[serde(default)]
    pub cause_orphelin: Option<CauseOrphelin>,
    #[serde(default)]
    pub rattachement: String,
    /// Racine git (ou dossier courant) et branche / worktree, calculés par le client à la création.
    #[serde(default)]
    pub projet: Option<String>,
    #[serde(default)]
    pub branche: Option<String>,
    /// Relevés au feu vert, pour mesurer ce que le ticket a fait monter : RAM disponible des deux côtés et vmmemWSL.
    #[serde(default)]
    pub base_ram_windows_mo: Option<i64>,
    #[serde(default)]
    pub base_ram_wsl_mo: Option<i64>,
    #[serde(default)]
    pub base_vmmem_mo: Option<u64>,
    /// Feu vert donné à la main par Adel depuis la fenêtre, hors de l'ordre de la file.
    #[serde(default)]
    pub force: bool,
    /// Tickets passés devant celui-ci pendant qu'il était tête de file bloquée (anti-famine).
    #[serde(default)]
    pub doublages: u32,
    #[serde(skip)]
    pub presence: u64,
    #[serde(skip)]
    pub ecoutes: u32,
}

impl Ticket {
    pub fn fin_prevue(&self) -> Option<u64> {
        self.accorde.map(|a| a + self.besoin.duree_s)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Etat {
    pub epoque: u64,
    pub compteur: u64,
    pub vmmem_pid: Option<u32>,
    #[serde(default)]
    pub gel_manuel: Option<u64>,
    pub tickets: Vec<Ticket>,
    /// Hystérésis de la pression due au swap : déclarée, et calme depuis quand.
    #[serde(skip)]
    pub pression_swap: bool,
    #[serde(skip)]
    pub swap_calme_depuis: Option<u64>,
}

impl Etat {
    pub fn ticket(&self, id: u64) -> Option<&Ticket> {
        self.tickets.iter().find(|t| t.id == id)
    }

    pub fn ticket_mut(&mut self, id: u64) -> Option<&mut Ticket> {
        self.tickets.iter_mut().find(|t| t.id == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vmmem {
    pub pid: u32,
    pub rss_mo: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MesuresWsl {
    pub mem_dispo_mo: u64,
    pub mem_totale_mo: u64,
    pub charge1: f32,
    #[serde(default)]
    pub swap_total_mo: u64,
    #[serde(default)]
    pub swap_libre_mo: u64,
    /// Compteurs cumulés de /proc/vmstat (pages).
    #[serde(default)]
    pub pswpin: u64,
    #[serde(default)]
    pub pswpout: u64,
    #[serde(default)]
    pub quand: u64,
}

impl MesuresWsl {
    /// Part du swap utilisée, en pour cent ; 0 sans swap.
    pub fn swap_pct(&self) -> u64 {
        if self.swap_total_mo == 0 {
            return 0;
        }
        self.swap_total_mo.saturating_sub(self.swap_libre_mo) * 100 / self.swap_total_mo
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mesures {
    pub ram_totale_mo: u64,
    pub ram_dispo_mo: u64,
    pub commit_mo: u64,
    pub commit_limite_mo: u64,
    pub cpu_pct: f32,
    #[serde(default)]
    pub coeurs_windows: u32,
    /// Débit de sortie du swap WSL (pages/s sur ~30 s), calculé par le serveur à partir des compteurs du client.
    #[serde(default)]
    pub swap_sortie_ps: f32,
    pub vmmem: Option<Vmmem>,
    #[serde(default)]
    pub wsl: Option<MesuresWsl>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Demande {
    pub requete_id: String,
    pub cote: Cote,
    pub ram_mo: Option<u64>,
    pub cpu: Option<u32>,
    pub duree_s: Option<u64>,
    pub description: String,
    pub session: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<u32>,
    pub dossier: String,
    pub signature: String,
    pub mesures: Option<MesuresWsl>,
    #[serde(default)]
    pub manuel: bool,
    /// Même valeur pour une commande relancée à l'identique : elle reprend son ticket au lieu d'en créer un.
    #[serde(default)]
    pub rattachement: String,
    #[serde(default)]
    pub projet: Option<String>,
    #[serde(default)]
    pub branche: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prolonge {
    pub duree_s: u64,
    pub raison: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Presence {
    pub mesures: Option<MesuresWsl>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Fin {
    pub code: Option<i32>,
    pub pic_mo: Option<u64>,
    pub duree_s: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeDecision {
    Refus,
    Attente,
    Gel,
    FeuVert,
    Actif,
    Orphelin,
    Termine,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Raison {
    pub decision: TypeDecision,
    #[serde(default)]
    pub position: Option<usize>,
    #[serde(default)]
    pub reserve: Option<String>,
    #[serde(default)]
    pub besoin: Option<i64>,
    #[serde(default)]
    pub disponible: Option<i64>,
    #[serde(default)]
    pub reserve_actifs: Option<i64>,
    #[serde(default)]
    pub marge: Option<i64>,
    #[serde(default)]
    pub bloquants: Vec<u64>,
    #[serde(default)]
    pub debloque_vers: Option<u64>,
    pub phrase: String,
}

impl Raison {
    pub fn simple(decision: TypeDecision, phrase: String) -> Raison {
        Raison {
            decision,
            position: None,
            reserve: None,
            besoin: None,
            disponible: None,
            reserve_actifs: None,
            marge: None,
            bloquants: Vec::new(),
            debloque_vers: None,
            phrase,
        }
    }

    /// Identifiant stable d'une raison : ignore les valeurs mesurées qui bougent à chaque relevé.
    pub fn cle(&self) -> String {
        let decision = serde_json::to_value(self.decision)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let bloquants: Vec<String> = self.bloquants.iter().map(u64::to_string).collect();
        format!(
            "{}-{}-{}-{}-{}",
            decision,
            self.position.unwrap_or(0),
            self.reserve.as_deref().unwrap_or("x"),
            bloquants.join("_"),
            self.debloque_vers.unwrap_or(0)
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vue {
    pub id: u64,
    pub etat: String,
    pub epoque: u64,
    pub raison: Raison,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VueReserve {
    pub cle: String,
    pub nom: String,
    pub total: i64,
    pub disponible: i64,
    pub reserve: i64,
    pub marge: i64,
    pub libre: i64,
    /// Ticket WSL exclusif en cours, qui explique un libre CPU WSL négatif.
    #[serde(default)]
    pub exclusif: Option<u64>,
}

impl VueReserve {
    /// Libre tel qu'on l'affiche : jamais sous zéro, avec le ticket exclusif qui l'explique.
    pub fn libre_lisible(&self) -> String {
        let libre = self.libre.max(0);
        let texte = if self.cle.starts_with("cpu") { crate::format::coeurs(libre) } else { crate::format::go(libre) };
        match self.exclusif {
            Some(id) => format!("{texte} (exclusif #{id})"),
            None => texte,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VueTicket {
    pub id: u64,
    pub etat: EtatTicket,
    pub cote: Cote,
    pub besoin: Besoin,
    pub description: String,
    pub session: String,
    pub cree: u64,
    pub accorde: Option<u64>,
    pub manuel: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub projet: Option<String>,
    #[serde(default)]
    pub branche: Option<String>,
    pub presence_il_y_a_s: u64,
    pub phrase: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Statut {
    pub epoque: u64,
    pub maintenant: u64,
    pub mesures: Mesures,
    pub reserves: Vec<VueReserve>,
    pub tickets: Vec<VueTicket>,
    pub alertes: Vec<String>,
    #[serde(default)]
    pub gel_manuel: Option<u64>,
}
