use std::path::Path;

use crate::format::{parse_duration, parse_ram, ram_option};
use crate::i18n::{self, Lang};
use crate::model::{Need, Side};
use crate::protocol::{DEFAULT_PIPE, LEGACY_PIPE};
use crate::tr;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub pipe: String,
    /// Also listen on the pre-Clearway pipe, for clients installed before the rename.
    pub legacy_pipe: bool,
    pub language: Option<Lang>,
    pub wsl_ram_max_mb: u64,
    pub cpu_total: u32,
    pub cpu_total_windows: Option<u32>,
    pub margin_ram_windows_mb: u64,
    pub margin_ram_wsl_mb: u64,
    pub margin_cpu: u32,
    pub default_wsl: Need,
    pub default_windows: Need,
    pub unattended_delay_s: u64,
    pub absence_delay_s: u64,
    pub recall_delay_s: u64,
    pub silence_delay_s: u64,
    pub rampup_delay_s: u64,
    pub resume_delay_s: u64,
    pub long_poll_s: u64,
    pub max_overtakes: u32,
    pub head_max_wait_s: u64,
    pub swap_threshold_pct: u64,
    pub swap_release_pct: u64,
    pub swap_out_rate: u64,
    pub cpu_pressure_pct: u64,
    pub cpu_release_pct: u64,
    pub cpu_pressure_delay_s: u64,
    /// Look for a new release once a day (otherwise only from the tray menu).
    pub update_check_daily: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            pipe: DEFAULT_PIPE.into(),
            legacy_pipe: true,
            language: None,
            wsl_ram_max_mb: 12 * 1024,
            cpu_total: 10,
            cpu_total_windows: None,
            margin_ram_windows_mb: 1536,
            margin_ram_wsl_mb: 1536,
            margin_cpu: 2,
            default_wsl: Need { ram_mb: 2048, cpu: 2, duration_s: 15 * 60 },
            default_windows: Need { ram_mb: 4096, cpu: 4, duration_s: 30 * 60 },
            unattended_delay_s: 90,
            absence_delay_s: 15 * 60,
            recall_delay_s: 120,
            silence_delay_s: 150,
            rampup_delay_s: 180,
            resume_delay_s: 120,
            long_poll_s: 60,
            max_overtakes: 3,
            head_max_wait_s: 20 * 60,
            swap_threshold_pct: 90,
            swap_release_pct: 85,
            swap_out_rate: 256,
            cpu_pressure_pct: 85,
            cpu_release_pct: 70,
            cpu_pressure_delay_s: 60,
            update_check_daily: false,
        }
    }
}

/// Keys of the French configuration written before the rename, with their English names.
const LEGACY_KEYS: [(&str, &str); 23] = [
    ("tube", "pipe"),
    ("marge.ram_windows", "margin.ram_windows"),
    ("marge.ram_wsl", "margin.ram_wsl"),
    ("marge.cpu", "margin.cpu"),
    ("defaut.wsl.ram", "default.wsl.ram"),
    ("defaut.wsl.cpu", "default.wsl.cpu"),
    ("defaut.wsl.duree", "default.wsl.duration"),
    ("defaut.windows.ram", "default.windows.ram"),
    ("defaut.windows.cpu", "default.windows.cpu"),
    ("defaut.windows.duree", "default.windows.duration"),
    ("delai.attente", "delay.unattended"),
    ("delai.absence", "delay.absence"),
    ("delai.rappel", "delay.recall"),
    ("delai.reprise", "delay.resume"),
    ("delai.actif", "delay.silence"),
    ("delai.montee", "delay.rampup"),
    ("delai.long_poll", "delay.long_poll"),
    ("remplissage.max_doublages", "backfill.max_overtakes"),
    ("remplissage.attente_max", "backfill.max_head_wait"),
    ("swap.seuil", "swap.threshold"),
    ("swap.seuil_levee", "swap.release_threshold"),
    ("swap.debit_sortie", "swap.out_rate"),
    ("langue", "language"),
];

/// Duration as written in config.toml (`90s`, `15m`, `2h`), read back by `parse_duration`.
fn duration_option(s: u64) -> String {
    if s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

impl Config {
    pub fn read(text: &str) -> Result<Config, String> {
        Config::read_flagged(text).map(|(c, _)| c)
    }

    /// Also tells whether the text used pre-Clearway French keys.
    pub fn read_flagged(text: &str) -> Result<(Config, bool), String> {
        let mut c = Config::default();
        let mut legacy = false;
        // Notepad and PowerShell 5 write a BOM at the start of UTF-8 files.
        for (n, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let number = n + 1;
            let error = |what: String| tr!("config.toml line {number}: {what} ({line})", "config.toml ligne {number} : {what} ({line})");
            let (key, value) = line.split_once('=').ok_or_else(|| error(tr!("`=` expected", "« = » attendu")))?;
            let mut key = key.trim();
            let legacy_key = LEGACY_KEYS.iter().find(|(fr, _)| *fr == key).map(|(_, en)| *en);
            if let Some(english) = legacy_key {
                key = english;
                legacy = true;
            }
            let value = value.split('#').next().unwrap_or("").trim().trim_matches('"');
            let ram = || parse_ram(value).ok_or_else(|| error(tr!("unreadable RAM amount", "quantité de RAM illisible")));
            let integer = || value.parse::<u32>().map_err(|_| error(tr!("integer expected", "entier attendu")));
            let seconds = || parse_duration(value).ok_or_else(|| error(tr!("unreadable duration", "durée illisible")));
            let boolean = || match value {
                "true" | "vrai" => Ok(true),
                "false" | "faux" => Ok(false),
                _ => Err(error(tr!("true or false expected", "true ou false attendu"))),
            };
            match key {
                "pipe" => {
                    if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        return Err(error(tr!("pipe: letters, digits, - and _ only", "tube : lettres, chiffres, - et _ seulement")));
                    }
                    // The old default pipe name means "the default": it becomes Clearway's, the old one stays served.
                    c.pipe = if legacy && value == LEGACY_PIPE { DEFAULT_PIPE.to_owned() } else { value.to_owned() };
                }
                "legacy_pipe" => c.legacy_pipe = boolean()?,
                "language" => {
                    c.language = match value {
                        "" | "auto" => None,
                        v => Some(i18n::parse(v).ok_or_else(|| error(tr!("language: en, fr or auto", "langue : en, fr ou auto")))?),
                    }
                }
                "wsl.ram_max" => c.wsl_ram_max_mb = ram()?,
                "cpu.total" => c.cpu_total = integer()?,
                "cpu.total_windows" => c.cpu_total_windows = Some(integer()?),
                "margin.ram_windows" => c.margin_ram_windows_mb = ram()?,
                "margin.ram_wsl" => c.margin_ram_wsl_mb = ram()?,
                "margin.cpu" => c.margin_cpu = integer()?,
                "default.wsl.ram" => c.default_wsl.ram_mb = ram()?,
                "default.wsl.cpu" => c.default_wsl.cpu = integer()?,
                "default.wsl.duration" => c.default_wsl.duration_s = seconds()?,
                "default.windows.ram" => c.default_windows.ram_mb = ram()?,
                "default.windows.cpu" => c.default_windows.cpu = integer()?,
                "default.windows.duration" => c.default_windows.duration_s = seconds()?,
                "delay.unattended" => c.unattended_delay_s = seconds()?,
                "delay.absence" => c.absence_delay_s = seconds()?,
                "delay.recall" => c.recall_delay_s = seconds()?,
                "delay.resume" => c.resume_delay_s = seconds()?,
                "delay.silence" => {
                    // 60 s was the old default, shorter than the client's own reconnection patience: take the new one.
                    let s = seconds()?;
                    if !(legacy_key.is_some() && s == 60) {
                        c.silence_delay_s = s;
                    }
                }
                "delay.rampup" => c.rampup_delay_s = seconds()?,
                "delay.long_poll" => c.long_poll_s = seconds()?,
                "backfill.max_overtakes" => c.max_overtakes = integer()?,
                "backfill.max_head_wait" => c.head_max_wait_s = seconds()?,
                "swap.threshold" => c.swap_threshold_pct = integer()?.into(),
                "swap.release_threshold" => c.swap_release_pct = integer()?.into(),
                "swap.out_rate" => c.swap_out_rate = integer()?.into(),
                "cpu.pressure" => c.cpu_pressure_pct = integer()?.into(),
                "cpu.pressure_release" => c.cpu_release_pct = integer()?.into(),
                "cpu.pressure_delay" => c.cpu_pressure_delay_s = seconds()?,
                "update.check" => {
                    c.update_check_daily = match value {
                        "manual" => false,
                        "daily" => true,
                        _ => return Err(error(tr!("update.check: manual or daily", "update.check : manual ou daily"))),
                    }
                }
                _ => return Err(error(tr!("unknown key", "clé inconnue"))),
            }
        }
        Ok((c, legacy))
    }

    /// The whole configuration as an English config.toml, with every value written out.
    pub fn render(&self) -> String {
        let need = |n: &Need, side: &str| {
            format!(
                "default.{side}.ram = \"{}\"\ndefault.{side}.cpu = {}\ndefault.{side}.duration = \"{}\"\n",
                ram_option(n.ram_mb as i64),
                n.cpu,
                duration_option(n.duration_s)
            )
        };
        let windows_cpu = match self.cpu_total_windows {
            Some(n) => format!("cpu.total_windows = {n}"),
            None => "# cpu.total_windows = 16".to_owned(),
        };
        let language = self.language.map_or("auto", i18n::name);
        format!(
            r#"# Clearway: read when clearway.exe starts.
# Named pipe \\.\pipe\<pipe>; clients choose it with CLEARWAY_PIPE.
pipe = "{pipe}"
# Also serve the pipe of clients installed before Clearway ("orchestrateur").
legacy_pipe = {legacy_pipe}
# Messages: en, fr, or auto (Windows display language).
language = "{language}"

# WSL VM cap (.wslconfig memory=): a WSL task only takes from Windows what vmmemWSL can still grow up to this cap.
wsl.ram_max = "{wsl_ram_max}"
cpu.total = {cpu_total}
# Windows CPU budget: by default, the logical cores measured at start. A WSL task debits both budgets.
{windows_cpu}

margin.ram_windows = "{margin_ram_windows}"
margin.ram_wsl = "{margin_ram_wsl}"
margin.cpu = {margin_cpu}

# Needs used when a request gives no --ram, --cpu or --duration.
{default_wsl}{default_windows}
# Waiting ticket skipped (place kept) after delay.unattended without a listening client, removed after delay.absence.
delay.unattended = "{unattended}"
delay.absence = "{absence}"
# Clearance taken back if no client acknowledged it within this delay.
delay.recall = "{recall}"
# A silent `clearway run` becomes orphaned after delay.silence: keep it above 3 missed presences (10 s apart) and
# the client's reconnection patience (120 s) + 30 s. Longest wait served in one call: delay.long_poll.
delay.silence = "{silence}"
delay.long_poll = "{long_poll}"
# Ramp-up of an active ticket: full reservation during this delay, then only what it has not taken yet.
delay.rampup = "{rampup}"
# Closed session link (MCP server): its tickets wait this long for the same session to come back.
delay.resume = "{resume}"

# Backfill: a ticket goes ahead of the blocked head if it does not use what the head lacks, or ends before the head's
# estimated start; at most this many times, and no more once the head has waited this long.
backfill.max_overtakes = {max_overtakes}
backfill.max_head_wait = "{max_head_wait}"

# WSL swap: pressure only when filled beyond swap.threshold (%) AND active (out rate above swap.out_rate pages/s over
# 30 s) or MemAvailable under margin.ram_wsl + 1 GB. Lifted under swap.release_threshold or after 60 s of calm.
swap.threshold = {threshold}
swap.release_threshold = {release}
swap.out_rate = {out_rate}

# Measured CPU (WSL: 1-min load average over cpu.total; Windows: CPU usage): at or above cpu.pressure (%) for
# cpu.pressure_delay, no new ticket takes cores on that side; lifted under cpu.pressure_release for as long.
cpu.pressure = {cpu_pressure}
cpu.pressure_release = {cpu_release}
cpu.pressure_delay = "{cpu_delay}"

# New releases: "manual" (tray menu only) or "daily" (asks GitHub once a day). Nothing installs by itself: an
# available release is offered, and the setup asks before changing anything.
update.check = "{update_check}"
"#,
            pipe = self.pipe,
            legacy_pipe = self.legacy_pipe,
            wsl_ram_max = ram_option(self.wsl_ram_max_mb as i64),
            cpu_total = self.cpu_total,
            margin_ram_windows = ram_option(self.margin_ram_windows_mb as i64),
            margin_ram_wsl = ram_option(self.margin_ram_wsl_mb as i64),
            margin_cpu = self.margin_cpu,
            default_wsl = need(&self.default_wsl, "wsl"),
            default_windows = need(&self.default_windows, "windows"),
            unattended = duration_option(self.unattended_delay_s),
            absence = duration_option(self.absence_delay_s),
            recall = duration_option(self.recall_delay_s),
            silence = duration_option(self.silence_delay_s),
            long_poll = duration_option(self.long_poll_s),
            rampup = duration_option(self.rampup_delay_s),
            resume = duration_option(self.resume_delay_s),
            max_overtakes = self.max_overtakes,
            max_head_wait = duration_option(self.head_max_wait_s),
            threshold = self.swap_threshold_pct,
            release = self.swap_release_pct,
            out_rate = self.swap_out_rate,
            cpu_pressure = self.cpu_pressure_pct,
            cpu_release = self.cpu_release_pct,
            cpu_delay = duration_option(self.cpu_pressure_delay_s),
            update_check = if self.update_check_daily { "daily" } else { "manual" },
        )
    }

    /// Reads `config.toml` from the state dir; writes the defaults if it is missing and `create` is true.
    /// A pre-Clearway French file is kept as `config.toml.before-clearway` and rewritten in English.
    pub fn load(dir: &Path, create: bool) -> Result<Config, String> {
        let path = dir.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (c, legacy) = Config::read_flagged(&text)?;
                if legacy {
                    let backup = dir.join("config.toml.before-clearway");
                    std::fs::write(&backup, &text)
                        .and_then(|_| std::fs::write(&path, c.render()))
                        .map_err(|e| tr!("writing {}: {e}", "écriture de {} : {e}", path.display()))?;
                }
                Ok(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if create {
                    std::fs::write(&path, Config::default().render())
                        .map_err(|e| tr!("writing {}: {e}", "écriture de {} : {e}", path.display()))?;
                }
                Ok(Config::default())
            }
            Err(e) => Err(tr!("reading {}: {e}", "lecture de {} : {e}", path.display())),
        }
    }

    pub fn default_need(&self, side: Side) -> Need {
        match side {
            Side::Wsl => self.default_wsl,
            Side::Windows => self.default_windows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_gives_back_defaults() {
        assert_eq!(Config::read(&Config::default().render()).unwrap(), Config::default());
        let mut c = Config { cpu_total_windows: Some(12), language: Some(Lang::Fr), legacy_pipe: false, ..Config::default() };
        c.default_wsl.duration_s = 90;
        assert_eq!(Config::read(&c.render()).unwrap(), c);
    }

    #[test]
    fn overrides_and_errors() {
        let c = Config::read("pipe = \"test-1\"\ndelay.silence = \"3s\"\nlanguage = \"fr\"\n").unwrap();
        assert_eq!((c.pipe.as_str(), c.silence_delay_s, c.language), ("test-1", 3, Some(Lang::Fr)));
        assert_eq!(Config::read("\u{feff}pipe = \"test-2\"\n").unwrap().pipe, "test-2");
        i18n::with(Lang::En, || {
            assert!(Config::read("prot = 1").unwrap_err().contains("unknown key"));
        });
        assert!(Config::read("margin.cpu = two").is_err());
        assert!(Config::read("pipe = \"a b\"").is_err());
        assert!(Config::read("legacy_pipe = maybe").is_err());
    }

    #[test]
    fn legacy_french_config_is_read_and_flagged() {
        let old = "tube = \"orchestrateur\"\nmarge.ram_wsl = \"2G\"\ndelai.actif = \"3s\"\nremplissage.max_doublages = 5\n\
                   swap.seuil = 80\n# commentaire\ndefaut.wsl.duree = \"20m\"\n";
        let (c, legacy) = Config::read_flagged(old).unwrap();
        assert!(legacy);
        assert_eq!(c.pipe, DEFAULT_PIPE, "the old default pipe becomes Clearway's");
        assert_eq!((c.margin_ram_wsl_mb, c.silence_delay_s, c.max_overtakes), (2048, 3, 5));
        assert_eq!(Config::read("delai.actif = \"60s\"").unwrap().silence_delay_s, 150, "old default replaced");
        assert_eq!(Config::read("delay.silence = \"60s\"").unwrap().silence_delay_s, 60, "explicit new key kept");
        assert_eq!((c.swap_threshold_pct, c.default_wsl.duration_s), (80, 20 * 60));
        assert_eq!(Config::read_flagged("tube = \"essai-3\"").unwrap().0.pipe, "essai-3", "a custom pipe is kept");
        assert!(!Config::read_flagged(&c.render()).unwrap().1);
    }
}
