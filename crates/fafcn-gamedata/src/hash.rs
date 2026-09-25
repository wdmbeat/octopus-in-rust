//! Streaming SHA-256 helpers shared by the server and the sync client.

use std::{fs::File, io::Read, path::Path};

use sha2::{Digest, Sha256};

/// Compute the lowercase hex SHA-256 digest of a file, streaming in 64 KiB
/// chunks so large game archives never load fully into memory.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    sha256_file_with_progress(path, |_| {})
}

/// Compute the lowercase hex SHA-256 digest of a file, reporting the number
/// of bytes hashed so far after every chunk (for progress bars over
/// multi-hundred-MB patch archives, where per-file events are too coarse).
pub fn sha256_file_with_progress(
    path: &Path,
    mut progress: impl FnMut(u64),
) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut done = 0_u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// Compute the lowercase hex SHA-256 digest of an in-memory byte slice.
pub fn sha256_bytes(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_bytes_known_vector() {
        // SHA-256 of "abc" (well-known test vector).
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_file_matches_bytes() {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("fafcn-gamedata-test-{}", std::process::id()));
        std::fs::write(&tmp, b"abc").unwrap();
        assert_eq!(sha256_file(&tmp).unwrap(), sha256_bytes(b"abc"));
        std::fs::remove_file(&tmp).unwrap();
    }
}
