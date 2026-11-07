/// Parses a RAM amount (`3G`, `1,5 Go`, `1.5GB`, `512M`) into MiB; without a unit, GiB.
pub fn parse_ram(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace(',', ".");
    let s = s.trim_end_matches('b').trim_end_matches('o');
    let (number, factor) = if let Some(n) = s.strip_suffix('g') {
        (n, 1024.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 1.0)
    } else {
        (s, 1024.0)
    };
    let v: f64 = number.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| (v * factor).round() as u64)
}

/// Parses a duration (`15m`, `1h30`, `90s`, `2min`) into seconds; a bare number counts as minutes.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace("min", "m");
    if let Ok(n) = s.parse::<u64>() {
        return (n > 0).then_some(n * 60);
    }
    let mut total = 0u64;
    let mut number = String::new();
    let mut last = ' ';
    for c in s.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let n: u64 = number.parse().ok()?;
        number.clear();
        total += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        last = c;
    }
    if !number.is_empty() {
        let n: u64 = number.parse().ok()?;
        if last != 'h' {
            return None;
        }
        total += n * 60;
    }
    (total > 0).then_some(total)
}

/// RAM amount for humans: `1,5 Go` in French, `1.5 GB` in English.
pub fn gb(mb: i64) -> String {
    let v = mb as f64 / 1024.0;
    let text = if (v - v.round()).abs() < 0.05 {
        format!("{:.0}", v.round())
    } else {
        format!("{v:.1}")
    };
    match crate::i18n::current() {
        crate::i18n::Lang::Fr => format!("{} Go", text.replace('.', ",")),
        crate::i18n::Lang::En => format!("{text} GB"),
    }
}

/// Value to copy into a `--ram` option, rounded down to a tenth of a GB.
pub fn ram_option(mb: i64) -> String {
    let tenths = (mb.max(0) * 10) / 1024;
    if tenths % 10 == 0 {
        format!("{}G", tenths / 10)
    } else {
        format!("{}.{}G", tenths / 10, tenths % 10)
    }
}

pub fn duration(s: u64) -> String {
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

pub fn cores(n: i64) -> String {
    if n.abs() > 1 {
        crate::tr!("{n} cores", "{n} cœurs")
    } else {
        crate::tr!("{n} core", "{n} cœur")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram() {
        assert_eq!(parse_ram("3G"), Some(3072));
        assert_eq!(parse_ram("1,5 Go"), Some(1536));
        assert_eq!(parse_ram("1.5g"), Some(1536));
        assert_eq!(parse_ram("1.5GB"), Some(1536));
        assert_eq!(parse_ram("512M"), Some(512));
        assert_eq!(parse_ram("512Mo"), Some(512));
        assert_eq!(parse_ram("2"), Some(2048));
        assert_eq!(parse_ram("0"), None);
        assert_eq!(parse_ram("abc"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("15m"), Some(900));
        assert_eq!(parse_duration("1h30"), Some(5400));
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("2min"), Some(120));
        assert_eq!(parse_duration("45"), Some(2700));
        assert_eq!(parse_duration("3x"), None);
        assert_eq!(parse_duration("30"), Some(1800));
    }

    #[test]
    fn display() {
        use crate::i18n::{with, Lang};
        with(Lang::Fr, || {
            assert_eq!(gb(1536), "1,5 Go");
            assert_eq!(gb(3072), "3 Go");
            assert_eq!(cores(2), "2 cœurs");
            assert_eq!(cores(1), "1 cœur");
            assert_eq!(ram_option(10752), "10.5G");
            assert_eq!(ram_option(3072), "3G");
            assert_eq!(duration(5400), "1 h 30");
            assert_eq!(duration(3900), "1 h 05");
            assert_eq!(duration(900), "15 min");
        });
        with(Lang::En, || {
            assert_eq!(gb(1536), "1.5 GB");
            assert_eq!(gb(3072), "3 GB");
            assert_eq!(cores(2), "2 cores");
            assert_eq!(cores(1), "1 core");
            assert_eq!(ram_option(10752), "10.5G");
            assert_eq!(duration(5400), "1 h 30");
            assert_eq!(duration(3900), "1 h 05");
            assert_eq!(duration(900), "15 min");
        });
    }
}
