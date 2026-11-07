use serde_json::json;

use super::*;

#[test]
fn versions_compare_as_numbers() {
    assert_eq!(parse_version("v0.10.2"), Some((0, 10, 2)));
    assert_eq!(parse_version("1.2"), None);
    assert_eq!(parse_version("1.2.3-rc.1"), None);
    assert!(is_newer("v0.10.0", "0.9.9"));
    assert!(!is_newer("v0.1.0", "0.1.0"));
    assert!(!is_newer("garbage", "0.1.0"));
}

#[test]
fn latest_release_needs_the_setup_and_the_sums() {
    let answer = json!({
        "tag_name": "v0.2.0", "draft": false, "prerelease": false,
        "html_url": "https://github.com/AW-HTK/clearway/releases/tag/v0.2.0",
        "assets": [
            { "name": "clearway-setup.exe", "browser_download_url": "https://x/clearway-setup.exe" },
            { "name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS" },
            { "name": "SHA256SUMS.sig", "browser_download_url": "https://x/SHA256SUMS.sig" }
        ]
    });
    let r = release_from_api(&answer).unwrap();
    assert_eq!((r.version.as_str(), r.setup_url.as_str()), ("0.2.0", "https://x/clearway-setup.exe"));
    let mut without = answer.clone();
    without["assets"] = json!([]);
    assert_eq!(release_from_api(&without), None);
    let mut pre = answer;
    pre["prerelease"] = json!(true);
    assert_eq!(release_from_api(&pre), None);
}

#[test]
fn sums_file_lines_with_or_without_star() {
    let sums = format!("{}  clearway.exe\n{} *clearway-setup.exe\n", "a".repeat(64), "B".repeat(64));
    assert_eq!(expected_sum(&sums, "clearway-setup.exe"), Some("b".repeat(64)));
    assert_eq!(expected_sum(&sums, "clearway.exe"), Some("a".repeat(64)));
    assert_eq!(expected_sum(&sums, "install.sh"), None);
}

#[test]
fn sha256_known_vectors() {
    assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let long = vec![b'a'; 1_000];
    assert_eq!(sha256_hex(&long), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
}

#[test]
fn signature_of_the_sums_is_checked() {
    let pair = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7; 32]));
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let public = hex(pair.pk.as_ref());
    let sums = b"abc  clearway-setup.exe\n";
    let signature = hex(pair.sk.sign(sums, None).as_ref());
    assert!(signed_by(&public, sums, &signature));
    assert!(!signed_by(&public, b"abd  clearway-setup.exe\n", &signature), "other content");
    assert!(!signed_by(RELEASE_PUBLIC_KEY, sums, &signature), "other key");
    assert!(!signed_by(&public, sums, "zz"), "garbage");
}
