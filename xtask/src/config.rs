//! Resolves host tool and firmware locations. Environment variables win;
//! otherwise a small set of documented default locations is searched.

use std::env;
use std::path::{Path, PathBuf};

pub struct Tools {
    pub cargo: PathBuf,
    pub qemu: Option<PathBuf>,
    pub ovmf_code: Option<PathBuf>,
    pub ovmf_vars_template: Option<PathBuf>,
}

/// Where a path came from, for doctor output.
pub struct Found {
    pub path: Option<PathBuf>,
    pub source: String,
}

impl Tools {
    pub fn resolve_build_only() -> Self {
        Tools {
            cargo: cargo(),
            qemu: None,
            ovmf_code: None,
            ovmf_vars_template: None,
        }
    }

    /// Resolves everything QEMU needs, or explains what is missing.
    pub fn resolve_for_qemu() -> Result<Self, String> {
        let qemu = find_qemu();
        let code = find_ovmf_code(qemu.path.as_deref());
        let vars = find_ovmf_vars(qemu.path.as_deref());
        let mut missing = Vec::new();
        for (name, f) in [
            ("QEMU_X86_64", &qemu),
            ("OVMF_CODE", &code),
            ("OVMF_VARS_TEMPLATE", &vars),
        ] {
            if f.path.is_none() {
                missing.push(format!("  {name}: {}", f.source));
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "missing prerequisites:\n{}\nrun `cargo xtask doctor` and see tools/firmware/README.md",
                missing.join("\n")
            ));
        }
        Ok(Tools {
            cargo: cargo(),
            qemu: qemu.path,
            ovmf_code: code.path,
            ovmf_vars_template: vars.path,
        })
    }
}

pub fn cargo() -> PathBuf {
    env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| "cargo".into())
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Explicit env var: must point to an existing file, no fallback.
fn from_env(var: &str) -> Option<Found> {
    let v = env::var_os(var)?;
    let p = PathBuf::from(&v);
    Some(if p.is_file() {
        Found {
            path: Some(p),
            source: format!("from ${var}"),
        }
    } else {
        Found {
            path: None,
            source: format!("${var} is set to `{}`, which is not a file", p.display()),
        }
    })
}

fn first_existing(candidates: &[PathBuf], what: &str) -> Found {
    for c in candidates {
        if c.is_file() {
            return Found {
                path: Some(c.clone()),
                source: "found in default location".into(),
            };
        }
    }
    let list: Vec<String> = candidates.iter().map(|c| c.display().to_string()).collect();
    Found {
        path: None,
        source: format!(
            "{what} not found; set the variable or install it. Searched: {}",
            list.join("; ")
        ),
    }
}

pub fn find_qemu() -> Found {
    if let Some(f) = from_env("QEMU_X86_64") {
        return f;
    }
    let name = exe("qemu-system-x86_64");
    let mut candidates: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).map(|d| d.join(&name)).collect())
        .unwrap_or_default();
    if cfg!(windows)
        && let Some(pf) = env::var_os("ProgramFiles")
    {
        candidates.push(Path::new(&pf).join("qemu").join(&name));
    }
    let mut f = first_existing(&candidates, "qemu-system-x86_64");
    if f.path.is_some() {
        f.source = "found on PATH or in the default install directory".into();
    } else {
        f.source = format!(
            "qemu-system-x86_64 not found on PATH{}; set QEMU_X86_64",
            if cfg!(windows) {
                " or in %ProgramFiles%\\qemu"
            } else {
                ""
            }
        );
    }
    f
}

/// Firmware directories to search: QEMU's bundled share/ (Windows installer and
/// most source builds), then common Linux distribution package locations.
fn firmware_dirs(qemu: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(q) = qemu.and_then(Path::parent) {
        dirs.push(q.join("share"));
        if let Some(prefix) = q.parent() {
            dirs.push(prefix.join("share").join("qemu"));
        }
    }
    dirs.push("/usr/share/qemu".into());
    dirs.push("/usr/share/OVMF".into());
    dirs.push("/usr/share/edk2/x64".into());
    dirs
}

pub fn find_ovmf_code(qemu: Option<&Path>) -> Found {
    if let Some(f) = from_env("OVMF_CODE") {
        return f;
    }
    let names = ["edk2-x86_64-code.fd", "OVMF_CODE_4M.fd", "OVMF_CODE.fd"];
    let c: Vec<PathBuf> = firmware_dirs(qemu)
        .iter()
        .flat_map(|d| names.map(|n| d.join(n)))
        .collect();
    first_existing(&c, "OVMF code image")
}

pub fn find_ovmf_vars(qemu: Option<&Path>) -> Found {
    if let Some(f) = from_env("OVMF_VARS_TEMPLATE") {
        return f;
    }
    // QEMU ships one x86 variable-store template, named for i386, which is the
    // documented companion of edk2-x86_64-code.fd.
    let names = ["edk2-i386-vars.fd", "OVMF_VARS_4M.fd", "OVMF_VARS.fd"];
    let c: Vec<PathBuf> = firmware_dirs(qemu)
        .iter()
        .flat_map(|d| names.map(|n| d.join(n)))
        .collect();
    first_existing(&c, "OVMF variable-store template")
}
