//! Release signing for the maintainer. The secret seed (64 hex) comes from CLEARWAY_RELEASE_SIGNING_KEY and is never printed.
//!   keygen        prints a new secret seed on stdout, to pipe straight into a secrets manager
//!   public        prints the public key of CLEARWAY_RELEASE_SIGNING_KEY
//!   sign <file>   writes <file>.sig (hex Ed25519 signature of the file)

use ed25519_compact::{KeyPair, Seed};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key_pair() -> KeyPair {
    let text = std::env::var("CLEARWAY_RELEASE_SIGNING_KEY").expect("CLEARWAY_RELEASE_SIGNING_KEY missing");
    let bytes: Vec<u8> = (0..text.trim().len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text.trim()[i..i + 2], 16).expect("CLEARWAY_RELEASE_SIGNING_KEY: hex expected"))
        .collect();
    KeyPair::from_seed(Seed::from_slice(&bytes).expect("CLEARWAY_RELEASE_SIGNING_KEY: 32 bytes expected"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen"] => {
            let mut seed = [0u8; 32];
            std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").unwrap(), &mut seed).unwrap();
            print!("{}", hex(&seed));
        }
        ["public"] => println!("{}", hex(key_pair().pk.as_ref())),
        ["sign", file] => {
            let data = std::fs::read(file).expect("file unreadable");
            let signature = key_pair().sk.sign(&data, None);
            std::fs::write(format!("{file}.sig"), hex(signature.as_ref()) + "\n").unwrap();
        }
        _ => {
            eprintln!("usage: sign_release keygen | public | sign <file>");
            std::process::exit(2);
        }
    }
}
