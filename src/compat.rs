//! Pre-Clearway French dialect of `charge` and `orchestrateur.exe`: requests, responses and the `etat.json` state file,
//! translated to and from the English protocol so that clients installed before the rename keep working.

use serde_json::{Map, Value};

/// JSON keys: French name, English name.
const KEYS: &[(&str, &str)] = &[
    ("accuse", "ack"),
    ("recu", "acked"),
    ("alertes", "alerts"),
    ("quand", "at"),
    ("dispo_accord_mo", "avail_at_grant_mb"),
    ("disponible", "available"),
    ("base_ram_windows_mo", "base_ram_windows_mb"),
    ("base_ram_wsl_mo", "base_ram_wsl_mb"),
    ("base_vmmem_mo", "base_vmmem_mb"),
    ("bloquants", "blockers"),
    ("corps", "body"),
    ("branche", "branch"),
    ("commit_limite_mo", "commit_limit_mb"),
    ("commit_mo", "commit_mb"),
    ("compteur", "counter"),
    ("cree", "created"),
    ("dossier", "dir"),
    ("duree_s", "duration_s"),
    ("epoque", "epoch"),
    ("erreur", "error"),
    ("estimation", "estimate"),
    ("exclusif", "exclusive"),
    ("libre", "free"),
    ("gele", "frozen"),
    ("accorde", "granted"),
    ("identite", "identity"),
    ("cle", "key"),
    ("connu", "known"),
    ("bail_depasse", "lease_overrun"),
    ("liaison", "link"),
    ("ecoutes", "listeners"),
    ("charge1", "load1"),
    ("perdu", "lost"),
    ("manuel", "manual"),
    ("gel_manuel", "manual_freeze"),
    ("marge", "margin"),
    ("mesures", "measures"),
    ("mem_dispo_mo", "mem_avail_mb"),
    ("mem_totale_mo", "mem_total_mb"),
    ("phrase", "message"),
    ("methode", "method"),
    ("nom", "name"),
    ("besoin", "need"),
    ("maintenant", "now"),
    ("ouvrir", "open"),
    ("ordre", "order"),
    ("cause_orphelin", "orphan_cause"),
    ("doublages", "overtakes"),
    ("parametres", "params"),
    ("chemin", "path"),
    ("pic_mo", "peak_mb"),
    ("presence_il_y_a_s", "presence_age_s"),
    ("projet", "project"),
    ("ram_dispo_mo", "ram_avail_mb"),
    ("ram_mo", "ram_mb"),
    ("ram_totale_mo", "ram_total_mb"),
    ("raison", "reason"),
    ("rattachement", "reattach_key"),
    ("rappel", "recalled"),
    ("requete_id", "request_id"),
    ("reserve_actifs", "reserved_by_active"),
    ("repris", "resumed"),
    ("rss_mo", "rss_mb"),
    ("cote", "side"),
    ("etat", "state"),
    ("swap_calme_depuis", "swap_calm_since"),
    ("swap_libre_mo", "swap_free_mb"),
    ("swap_sortie_ps", "swap_out_ps"),
    ("pression_swap", "swap_pressure"),
    ("swap_total_mo", "swap_total_mb"),
    ("delai_s", "timeout_s"),
    ("debloque_vers", "unblocked_at"),
    ("coeurs_windows", "windows_cores"),
];

/// Values that differ, by English key: (key, French value, English value).
const VALUES: &[(&str, &str, &str)] = &[
    ("state", "attente", "waiting"),
    ("state", "actif", "active"),
    ("state", "orphelin", "orphaned"),
    ("state", "termine", "finished"),
    ("state", "expire", "expired"),
    ("state", "libere", "freed"),
    ("state", "annule", "cancelled"),
    ("state", "refuse", "refused"),
    ("decision", "refus", "refused"),
    ("decision", "attente", "waiting"),
    ("decision", "gel", "frozen"),
    ("decision", "feu_vert", "cleared"),
    ("decision", "actif", "active"),
    ("decision", "orphelin", "orphaned"),
    ("decision", "termine", "finished"),
    ("orphan_cause", "bail", "lease"),
    ("orphan_cause", "connexion", "connection"),
    ("reserve", "manuel", "manual"),
    ("reserve", "exclusif", "exclusive"),
];

/// Route segments: French, English.
const ROUTES: &[(&str, &str)] = &[
    ("attente", "wait"),
    ("prolonge", "extend"),
    ("fin", "release"),
    ("recu", "ack"),
    ("etat", "status"),
    ("gel", "freeze"),
    ("fenetre", "window"),
    ("quitter", "quit"),
    ("liaison", "link"),
    ("battement", "heartbeat"),
];

#[derive(Clone, Copy)]
enum Direction {
    ToEnglish,
    ToFrench,
}

fn key_of(k: &str, direction: Direction) -> &str {
    let found = match direction {
        Direction::ToEnglish => KEYS.iter().find(|(fr, _)| *fr == k).map(|(_, en)| *en),
        Direction::ToFrench => KEYS.iter().find(|(_, en)| *en == k).map(|(fr, _)| *fr),
    };
    found.unwrap_or(k)
}

fn value_of(english_key: &str, v: Value, direction: Direction) -> Value {
    let Value::String(s) = &v else { return v };
    let found = VALUES.iter().find(|(k, fr, en)| {
        *k == english_key
            && match direction {
                Direction::ToEnglish => fr == s,
                Direction::ToFrench => en == s,
            }
    });
    match (found, direction) {
        (Some((_, _, en)), Direction::ToEnglish) => Value::String((*en).to_owned()),
        (Some((_, fr, _)), Direction::ToFrench) => Value::String((*fr).to_owned()),
        (None, _) => v,
    }
}

fn translate(v: Value, direction: Direction) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                let english = match direction {
                    Direction::ToEnglish => key_of(&k, direction).to_owned(),
                    Direction::ToFrench => k.clone(),
                };
                let v = value_of(&english, translate(v, direction), direction);
                out.insert(key_of(&k, direction).to_owned(), v);
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(|v| translate(v, direction)).collect()),
        v => v,
    }
}

/// A request written by a pre-Clearway client (French keys: `methode`, `chemin`…).
pub fn is_legacy_request(v: &Value) -> bool {
    v.get("methode").is_some()
}

/// Legacy request → English request: keys, values, parameter names and route segments.
pub fn request_from_legacy(v: Value) -> Value {
    let mut v = translate(v, Direction::ToEnglish);
    if let Some(Value::String(path)) = v.get_mut("path") {
        *path = path
            .split('/')
            .map(|s| ROUTES.iter().find(|(fr, _)| *fr == s).map_or(s, |(_, en)| *en))
            .collect::<Vec<_>>()
            .join("/");
    }
    v
}

/// English response → the French response a pre-Clearway client expects.
pub fn response_to_legacy(v: Value) -> Value {
    translate(v, Direction::ToFrench)
}

/// Pre-Clearway `etat.json` → the English state file.
pub fn state_from_legacy(v: Value) -> Value {
    translate(v, Direction::ToEnglish)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_request_and_response() {
        let old = json!({
            "methode": "GET", "chemin": "/tickets/12/attente",
            "parametres": { "connu": "attente-1", "delai_s": "60", "accuse": "1", "mem_dispo_mo": "4000" },
            "corps": null
        });
        assert!(is_legacy_request(&old));
        let r = request_from_legacy(old);
        assert_eq!(r["path"], "/tickets/12/wait");
        assert_eq!(r["params"], json!({ "known": "attente-1", "timeout_s": "60", "ack": "1", "mem_avail_mb": "4000" }));
        assert!(!is_legacy_request(&r));
        let creation = request_from_legacy(json!({ "methode": "POST", "chemin": "/tickets",
            "corps": { "requete_id": "r1", "cote": "wsl", "ram_mo": 2048, "duree_s": 600, "manuel": true, "description": "fin de tâche" } }));
        assert_eq!(
            creation["body"],
            json!({ "request_id": "r1", "side": "wsl", "ram_mb": 2048, "duration_s": 600, "manual": true, "description": "fin de tâche" })
        );
        assert_eq!(request_from_legacy(json!({ "methode": "POST", "chemin": "/quitter" }))["path"], "/quit");

        let view = json!({ "code": 200, "body": { "id": 3, "state": "active", "epoch": 2,
            "reason": { "decision": "cleared", "message": "…", "reserve": "exclusive", "blockers": [] } } });
        assert_eq!(
            response_to_legacy(view),
            json!({ "code": 200, "corps": { "id": 3, "etat": "actif", "epoque": 2,
                "raison": { "decision": "feu_vert", "phrase": "…", "reserve": "exclusif", "bloquants": [] } } })
        );
    }

    #[test]
    fn legacy_state_file() {
        let old = json!({ "epoque": 9, "compteur": 44, "vmmem_pid": 20664, "gel_manuel": null, "tickets": [{
            "id": 44, "etat": "orphelin", "cause_orphelin": "bail", "besoin": { "ram_mo": 4096, "cpu": 4, "duree_s": 900 },
            "cote": "wsl", "manuel": true, "recu": true, "doublages": 1 }] });
        let new_state = state_from_legacy(old);
        assert_eq!(new_state["counter"], 44);
        let t = &new_state["tickets"][0];
        assert_eq!((t["state"].as_str(), t["orphan_cause"].as_str()), (Some("orphaned"), Some("lease")));
        assert_eq!(t["need"], json!({ "ram_mb": 4096, "cpu": 4, "duration_s": 900 }));
        assert_eq!((t["manual"].as_bool(), t["acked"].as_bool(), t["overtakes"].as_u64()), (Some(true), Some(true), Some(1)));
    }
}
