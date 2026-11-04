//! The action registry from the boot image (docs/registry-design.md):
//! every `etc/actions/<program>.actions` file, read once.

use alloc::string::String;
use alloc::vec::Vec;
use zoo_infer::registry::{DIR, MAX_FILE, Registry, SUFFIX};

/// Every registered app action, and the entries left out with why.
pub fn load() -> (Registry, Vec<String>) {
    let mut reg = Registry::default();
    let mut skipped = Vec::new();
    let mut i = 0;
    while let Ok(e) = crate::fs_stat(i) {
        let name = e.name();
        if name.starts_with(DIR) && name.ends_with(SUFFIX) {
            match crate::fs_map(i).ok().filter(|f| f.len() <= MAX_FILE) {
                Some(f) => match core::str::from_utf8(f) {
                    Ok(text) => reg.add_file(text, &mut skipped),
                    Err(_) => skipped.push(alloc::format!("{name}: not text")),
                },
                None => skipped.push(alloc::format!(
                    "{name}: unreadable or over {MAX_FILE} bytes"
                )),
            }
        }
        i += 1;
    }
    (reg, skipped)
}
