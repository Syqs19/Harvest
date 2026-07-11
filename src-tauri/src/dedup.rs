//! Deduplica dei file scaricati, su due livelli:
//! A) contenuto esatto: due file identici byte-per-byte (stessa foto ricaricata).
//! B) percettiva: la stessa immagine in formati/qualità/dimensioni diverse.
//!
//! Vale per l'intera sessione di download. Prudente per scelta: la soglia
//! percettiva è bassa, così si preferisce tenere un doppione piuttosto che
//! scartare per errore una foto diversa ma simile.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use image_hasher::{HashAlg, HasherConfig, ImageHash};

/// Soglia di distanza (Hamming) sotto la quale due immagini sono considerate
/// la stessa. Bassa = prudente (pochi falsi positivi). 0..=64.
const PHASH_THRESHOLD: u32 = 6;

/// Stato condiviso della deduplica. In `Arc` così `Dedup` è clonabile e può
/// essere spostato in un `spawn_blocking` continuando a condividere i registri.
#[derive(Default)]
struct Inner {
    /// Hash esatti (contenuto byte-per-byte) dei file già tenuti — livello A.
    exact: Mutex<HashSet<u64>>,
    /// Hash percettivi delle immagini già tenute — livello B.
    perceptual: Mutex<Vec<ImageHash>>,
}

#[derive(Default, Clone)]
pub struct Dedup(Arc<Inner>);

impl Dedup {
    pub fn clear(&self) {
        self.0.exact.lock().unwrap().clear();
        self.0.perceptual.lock().unwrap().clear();
    }

    /// Decide se tenere un file di cui si conosce già l'impronta esatta,
    /// calcolata a pezzi mentre veniva scritto su disco (vedi `Fnv`). Serve ai
    /// file grossi (video), che non passano mai interi per la memoria: senza
    /// questo salterebbero del tutto il controllo dei doppioni.
    /// Solo il livello A (uguaglianza esatta): quello percettivo confronta
    /// immagini e su un video non avrebbe senso.
    pub fn keep_exact(&self, hash: u64) -> bool {
        self.0.exact.lock().unwrap().insert(hash)
    }

    /// Decide se TENERE questo file (true) o scartarlo perché duplicato (false).
    /// `bytes` è il contenuto appena scaricato.
    pub fn keep(&self, bytes: &[u8]) -> bool {
        // A) Contenuto esatto: hash veloce dei byte
        let exact = fxhash(bytes);
        {
            let mut set = self.0.exact.lock().unwrap();
            if !set.insert(exact) {
                return false; // già visto identico
            }
        }

        // B) Percettiva: solo se è un'immagine decodificabile
        if let Ok(img) = image::load_from_memory(bytes) {
            let hasher = HasherConfig::new().hash_alg(HashAlg::Gradient).to_hasher();
            let ph = hasher.hash_image(&img);
            let mut seen = self.0.perceptual.lock().unwrap();
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

/// Hash non crittografico veloce (FNV-1a), sufficiente per il confronto di
/// uguaglianza esatta. Progressivo: si può alimentare a pezzi, così un file
/// grosso viene "impronta-to" mentre lo si scrive su disco, senza tenerlo
/// tutto in memoria.
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf29ce484222325)
    }
}

impl Fnv {
    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    pub fn finish(self) -> u64 {
        self.0
    }
}

fn fxhash(bytes: &[u8]) -> u64 {
    let mut h = Fnv::default();
    h.update(bytes);
    h.finish()
}
