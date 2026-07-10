//! Deduplica dei file scaricati, su due livelli:
//! A) contenuto esatto: due file identici byte-per-byte (stessa foto ricaricata).
//! B) percettiva: la stessa immagine in formati/qualità/dimensioni diverse.
//!
//! Vale per l'intera sessione di download. Prudente per scelta: la soglia
//! percettiva è bassa, così si preferisce tenere un doppione piuttosto che
//! scartare per errore una foto diversa ma simile.

use std::collections::HashSet;
use std::sync::Mutex;

use image_hasher::{HashAlg, HasherConfig, ImageHash};

/// Soglia di distanza (Hamming) sotto la quale due immagini sono considerate
/// la stessa. Bassa = prudente (pochi falsi positivi). 0..=64.
const PHASH_THRESHOLD: u32 = 6;

#[derive(Default)]
pub struct Dedup {
    /// Hash esatti (contenuto byte-per-byte) dei file già tenuti — livello A.
    exact: Mutex<HashSet<u64>>,
    /// Hash percettivi delle immagini già tenute — livello B.
    perceptual: Mutex<Vec<ImageHash>>,
}

impl Dedup {
    pub fn clear(&self) {
        self.exact.lock().unwrap().clear();
        self.perceptual.lock().unwrap().clear();
    }

    /// Decide se TENERE questo file (true) o scartarlo perché duplicato (false).
    /// `bytes` è il contenuto appena scaricato.
    pub fn keep(&self, bytes: &[u8]) -> bool {
        // A) Contenuto esatto: hash veloce dei byte
        let exact = fxhash(bytes);
        {
            let mut set = self.exact.lock().unwrap();
            if !set.insert(exact) {
                return false; // già visto identico
            }
        }

        // B) Percettiva: solo se è un'immagine decodificabile
        if let Ok(img) = image::load_from_memory(bytes) {
            let hasher = HasherConfig::new().hash_alg(HashAlg::Gradient).to_hasher();
            let ph = hasher.hash_image(&img);
            let mut seen = self.perceptual.lock().unwrap();
            for other in seen.iter() {
                if ph.dist(other) <= PHASH_THRESHOLD {
                    return false; // percettivamente uguale a una già tenuta
                }
            }
            seen.push(ph);
        }
        true
    }
}

/// Hash non crittografico veloce dei byte (FNV-1a), sufficiente per il confronto
/// di uguaglianza esatta.
fn fxhash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
