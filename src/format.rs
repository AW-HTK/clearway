/// Lit une quantité de RAM (« 3G », « 1,5 Go », « 512M ») en Mio ; sans unité, en Gio.
pub fn lire_ram(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace(',', ".");
    let s = s.trim_end_matches('b').trim_end_matches('o');
    let (nombre, facteur) = if let Some(n) = s.strip_suffix('g') {
        (n, 1024.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 1.0)
    } else {
        (s, 1024.0)
    };
    let v: f64 = nombre.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| (v * facteur).round() as u64)
}

/// Lit une durée (« 15m », « 1h30 », « 90s », « 2min ») en secondes ; un nombre seul compte en minutes.
pub fn lire_duree(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace("min", "m");
    if let Ok(n) = s.parse::<u64>() {
        return (n > 0).then_some(n * 60);
    }
    let mut total = 0u64;
    let mut nombre = String::new();
    let mut derniere = ' ';
    for c in s.chars() {
        if c.is_ascii_digit() {
            nombre.push(c);
            continue;
        }
        let n: u64 = nombre.parse().ok()?;
        nombre.clear();
        total += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        derniere = c;
    }
    if !nombre.is_empty() {
        let n: u64 = nombre.parse().ok()?;
        if derniere != 'h' {
            return None;
        }
        total += n * 60;
    }
    (total > 0).then_some(total)
}

pub fn go(mo: i64) -> String {
    let v = mo as f64 / 1024.0;
    let texte = if (v - v.round()).abs() < 0.05 {
        format!("{:.0}", v.round())
    } else {
        format!("{v:.1}")
    };
    format!("{} Go", texte.replace('.', ","))
}

/// Valeur à recopier dans une option `--ram`, arrondie vers le bas au dixième de Go.
pub fn option_ram(mo: i64) -> String {
    let dixiemes = (mo.max(0) * 10) / 1024;
    if dixiemes % 10 == 0 {
        format!("{}G", dixiemes / 10)
    } else {
        format!("{}.{}G", dixiemes / 10, dixiemes % 10)
    }
}

pub fn duree(s: u64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else if s % 3600 < 60 {
        format!("{} h", s / 3600)
    } else {
        format!("{} h {:02}", s / 3600, (s % 3600) / 60)
    }
}

pub fn coeurs(n: i64) -> String {
    if n.abs() > 1 {
        format!("{n} cœurs")
    } else {
        format!("{n} cœur")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram() {
        assert_eq!(lire_ram("3G"), Some(3072));
        assert_eq!(lire_ram("1,5 Go"), Some(1536));
        assert_eq!(lire_ram("1.5g"), Some(1536));
        assert_eq!(lire_ram("512M"), Some(512));
        assert_eq!(lire_ram("512Mo"), Some(512));
        assert_eq!(lire_ram("2"), Some(2048));
        assert_eq!(lire_ram("0"), None);
        assert_eq!(lire_ram("abc"), None);
    }

    #[test]
    fn durees() {
        assert_eq!(lire_duree("15m"), Some(900));
        assert_eq!(lire_duree("1h30"), Some(5400));
        assert_eq!(lire_duree("1h30m"), Some(5400));
        assert_eq!(lire_duree("90s"), Some(90));
        assert_eq!(lire_duree("2min"), Some(120));
        assert_eq!(lire_duree("45"), Some(2700));
        assert_eq!(lire_duree("3x"), None);
        assert_eq!(lire_duree("30"), Some(1800));
    }

    #[test]
    fn affichage() {
        assert_eq!(go(1536), "1,5 Go");
        assert_eq!(go(3072), "3 Go");
        assert_eq!(option_ram(10752), "10.5G");
        assert_eq!(option_ram(3072), "3G");
        assert_eq!(duree(5400), "1 h 30");
        assert_eq!(duree(900), "15 min");
    }
}
