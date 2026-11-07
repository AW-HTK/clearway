//! New releases: compares versions, reads GitHub's latest-release answer and checks a download against SHA256SUMS.
//! Nothing here installs anything: Clearway only offers the update, the setup asks before changing anything.

use serde_json::Value;

pub const LATEST_RELEASE_API: &str = "https://api.github.com/repos/AW-HTK/clearway/releases/latest";
pub const SETUP_ASSET: &str = "clearway-setup.exe";
/// Ed25519 key that signs SHA256SUMS (`SHA256SUMS.sig`); its secret half stays with the maintainer.
pub const RELEASE_PUBLIC_KEY: &str = "f01c0d9ff901e053663b1013a4e6a88ddbea72ca70cc6a6a046192ca23db2451";

fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// True when `signature_hex` is a valid Ed25519 signature of `data` by `public_key_hex`.
pub fn signed_by(public_key_hex: &str, data: &[u8], signature_hex: &str) -> bool {
    let (Some(pk), Some(sig)) = (from_hex(public_key_hex), from_hex(signature_hex)) else { return false };
    let (Ok(pk), Ok(sig)) = (ed25519_compact::PublicKey::from_slice(&pk), ed25519_compact::Signature::from_slice(&sig)) else {
        return false;
    };
    pk.verify(data, &sig).is_ok()
}

/// `(major, minor, patch)` of `v1.2.3` or `1.2.3`; a pre-release suffix (`-rc.1`) is not a release.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub setup_url: String,
    pub sums_url: String,
    pub signature_url: String,
    pub page_url: String,
}

/// The release described by `GET /repos/{owner}/{repo}/releases/latest`, if it carries the setup and signed sums.
pub fn release_from_api(answer: &Value) -> Option<Release> {
    if answer["draft"].as_bool() == Some(true) || answer["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = answer["tag_name"].as_str()?;
    parse_version(tag)?;
    let asset = |name: &str| {
        answer["assets"].as_array()?.iter().find(|a| a["name"] == name)?["browser_download_url"].as_str().map(str::to_owned)
    };
    Some(Release {
        version: tag.trim_start_matches('v').to_owned(),
        setup_url: asset(SETUP_ASSET)?,
        sums_url: asset("SHA256SUMS")?,
        signature_url: asset("SHA256SUMS.sig")?,
        page_url: answer["html_url"].as_str().unwrap_or_default().to_owned(),
    })
}

/// Expected SHA-256 (lowercase hex) of `name` in a SHA256SUMS file.
pub fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name && hash.len() == 64).then(|| hash.to_lowercase())
    })
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests;
