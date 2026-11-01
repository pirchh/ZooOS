//! musl's shared C library and dynamic loader (`libc.so`, installed as
//! `/lib/ld-musl-x86_64.so.1`), built from the verified musl source in
//! `tools/musl/` with the pinned Zig (docs/linux8-design.md). Zig's own
//! bundled musl omits the dynamic loader, so it cannot provide this.
//!
//! This follows musl 1.2.5's Makefile and configure for x86_64 with a clang
//! compiler: the same generated headers, source selection (an architecture
//! file replaces the generic one of the same name), compiler flags and link
//! line. The result is cached by version; delete the output directory to
//! rebuild.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const MUSL_VERSION: &str = "1.2.5";
/// Bumped whenever the build itself changes, so a cached `libc.so` from an
/// older build is never reused (2: no Zig compiler runtime; see
/// c/musl/complex-mul.c).
const BUILD_REVISION: u32 = 2;

/// The unpacked source, if it was downloaded (tools/musl/README.md).
pub fn source(root: &Path) -> Option<PathBuf> {
    let dir = root
        .join("tools")
        .join("musl")
        .join(format!("musl-{MUSL_VERSION}"));
    dir.join("ldso").join("dynlink.c").is_file().then_some(dir)
}

/// musl's `tools/mkalltypes.sed`: `TYPEDEF`, `STRUCT` and `UNION` lines
/// become guarded definitions; everything else is copied.
fn mkalltypes(input: &str) -> String {
    let mut out = String::new();
    for line in input.lines() {
        let guarded = |kind: &str, name: &str, decl: String| {
            format!(
                "#if defined(__NEED_{kind}{name}) && !defined(__DEFINED_{kind}{name})\n{decl}\n#define __DEFINED_{kind}{name}\n#endif\n"
            )
        };
        if let Some(rest) = line.strip_prefix("TYPEDEF ") {
            if let Some(body) = rest.strip_suffix(';')
                && let Some((ty, name)) = body.rsplit_once(' ')
            {
                out.push_str(&guarded("", name, format!("typedef {ty} {name};")));
                continue;
            }
        } else if let Some(rest) = line.strip_prefix("STRUCT ") {
            let rest = rest.trim_start();
            if let Some(body) = rest.strip_suffix(';')
                && let Some((name, def)) = body.split_once(' ')
            {
                out.push_str(&guarded("struct_", name, format!("struct {name} {def};")));
                continue;
            }
        } else if let Some(rest) = line.strip_prefix("UNION ") {
            let rest = rest.trim_start();
            if let Some(body) = rest.strip_suffix(';')
                && let Some((name, def)) = body.split_once(' ')
            {
                out.push_str(&guarded("union_", name, format!("union {name} {def};")));
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The sources of `libc.so`: `src/*`, `src/malloc/mallocng` and `ldso`,
/// each directory's x86_64 files replacing generic files of the same name.
fn sources(src: &Path) -> Result<Vec<(PathBuf, Vec<PathBuf>)>, String> {
    let read = |d: &Path| -> Result<Vec<PathBuf>, String> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(d)
            .map_err(|e| format!("cannot list {}: {e}", d.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        v.sort();
        Ok(v)
    };
    let mut dirs: Vec<PathBuf> = read(&src.join("src"))?
        .into_iter()
        .filter(|p| p.is_dir())
        .collect();
    dirs.push(src.join("src").join("malloc").join("mallocng"));
    dirs.push(src.join("ldso"));
    let mut out = Vec::new();
    for dir in dirs {
        let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned());
        let arch: Vec<PathBuf> = if dir.join("x86_64").is_dir() {
            read(&dir.join("x86_64"))?
                .into_iter()
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("c" | "s" | "S")
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let replaced: Vec<String> = arch.iter().filter_map(|p| stem(p)).collect();
        let mut files: Vec<PathBuf> = read(&dir)?
            .into_iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("c"))
            .filter(|p| stem(p).is_none_or(|s| !replaced.contains(&s)))
            .collect();
        files.extend(arch);
        if !files.is_empty() {
            out.push((dir, files));
        }
    }
    Ok(out)
}

/// Builds (or reuses) `libc.so`; returns its bytes.
pub fn build_libc_so(
    zig: &Path,
    src: &Path,
    complex_mul: &Path,
    out_root: &Path,
) -> Result<Vec<u8>, String> {
    let out = out_root.join(format!("musl-{MUSL_VERSION}-b{BUILD_REVISION}"));
    let lib = out.join("libc.so");
    if let Ok(data) = std::fs::read(&lib) {
        return Ok(data);
    }
    println!(
        "[build] musl {MUSL_VERSION} libc.so (zig, once; cached in {})",
        out.display()
    );
    let mkdir = |d: &Path| {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))
    };
    let obj_inc = out.join("obj").join("include");
    let obj_int = out.join("obj").join("src").join("internal");
    mkdir(&obj_inc.join("bits"))?;
    mkdir(&obj_int)?;
    let read_text = |p: PathBuf| {
        std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))
    };
    // Generated headers, as musl's Makefile makes them.
    let alltypes = mkalltypes(&read_text(src.join("arch/x86_64/bits/alltypes.h.in"))?)
        + &mkalltypes(&read_text(src.join("include/alltypes.h.in"))?);
    let syscall_in = read_text(src.join("arch/x86_64/bits/syscall.h.in"))?;
    let mut syscall = syscall_in.clone();
    for line in syscall_in.lines().filter(|l| l.contains("__NR_")) {
        syscall.push_str(&line.replace("__NR_", "SYS_"));
        syscall.push('\n');
    }
    let write = |p: PathBuf, s: String| {
        std::fs::write(&p, s).map_err(|e| format!("cannot write {}: {e}", p.display()))
    };
    write(obj_inc.join("bits").join("alltypes.h"), alltypes)?;
    write(obj_inc.join("bits").join("syscall.h"), syscall)?;
    write(
        obj_int.join("version.h"),
        format!("#define VERSION \"{MUSL_VERSION}\"\n"),
    )?;

    // configure's choices for clang on x86_64 (docs/linux8-design.md).
    let inc = |p: PathBuf| format!("-I{}", p.display());
    let flags: Vec<String> = [
        "-target",
        "x86_64-linux-musl",
        "-std=c99",
        "-nostdinc",
        "-ffreestanding",
        "-fexcess-precision=standard",
        "-frounding-math",
        "-fno-strict-aliasing",
        "-Wa,--noexecstack",
        "-D_XOPEN_SOURCE=700",
        "-O2",
        "-fomit-frame-pointer",
        "-fno-unwind-tables",
        "-fno-asynchronous-unwind-tables",
        "-ffunction-sections",
        "-fdata-sections",
        "-fno-stack-protector",
        "-fPIC",
        "-w",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([
        inc(src.join("arch/x86_64")),
        inc(src.join("arch/generic")),
        inc(obj_int.clone()),
        inc(src.join("src/include")),
        inc(src.join("src/internal")),
        inc(obj_inc.clone()),
        inc(src.join("include")),
    ])
    .collect();

    // One `zig cc -c -o <unique>.o` per source file, several at once.
    // (Given several sources at once, Zig's driver keeps only one object
    // per invocation, silently losing the rest: docs/linux10-design.md.)
    let groups = sources(src)?;
    let objs_root = out.join("lo");
    mkdir(&objs_root)?;
    let mut jobs = Vec::new();
    for (k, (_, files)) in groups.iter().enumerate() {
        for file in files {
            let stem = file
                .file_name()
                .map(|n| n.to_string_lossy().replace('.', "_"))
                .unwrap_or_default();
            jobs.push((file.clone(), objs_root.join(format!("{k:03}-{stem}.o"))));
        }
    }
    // The complex-multiplication helpers a compiler runtime would provide.
    jobs.push((
        complex_mul.to_path_buf(),
        objs_root.join("zoo-complex-mul.o"),
    ));
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let next = std::sync::Mutex::new(0usize);
    let errors = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let k = {
                        let mut n = next.lock().unwrap_or_else(|e| e.into_inner());
                        let k = *n;
                        *n += 1;
                        k
                    };
                    let Some((file, obj)) = jobs.get(k) else {
                        break;
                    };
                    let mut cmd = Command::new(zig);
                    cmd.arg("cc")
                        .args(&flags)
                        .arg("-c")
                        .arg(file)
                        .arg("-o")
                        .arg(obj);
                    match crate::process::output_bounded(
                        cmd,
                        std::time::Duration::from_secs(600),
                        "zig cc (musl)",
                    ) {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => errors
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(format!(
                                "{}: {}",
                                file.display(),
                                String::from_utf8_lossy(&o.stderr)
                            )),
                        Err(e) => errors.lock().unwrap_or_else(|e| e.into_inner()).push(e),
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap_or_else(|e| e.into_inner());
    if let Some(first) = errors.first() {
        return Err(format!(
            "musl build failed ({} files):\n{first}",
            errors.len()
        ));
    }
    let mut objs = Vec::new();
    for (_, obj) in &jobs {
        if !obj.is_file() {
            return Err(format!("musl build: {} was not produced", obj.display()));
        }
        objs.push(obj.clone());
    }
    println!(
        "[build] musl: {} objects from {} sources",
        objs.len(),
        jobs.len()
    );
    objs.sort();
    // musl's link line, given to LLD directly (`zig ld.lld`; Zig's `cc`
    // driver does not pass these options through): a shared object entered
    // at the loader's `_dlstart`, musl's exported-symbol list, and no
    // undefined symbols.
    let list = out.join("objects.txt");
    let quoted: Vec<String> = objs
        .iter()
        .map(|p| format!("\"{}\"", p.display().to_string().replace('\\', "/")))
        .collect();
    write(list.clone(), quoted.join("\n"))?;
    let tmp = out.join("libc.so.tmp");
    let mut cmd = Command::new(zig);
    cmd.current_dir(&out)
        .args([
            "ld.lld",
            "-shared",
            "-e",
            "_dlstart",
            "--no-undefined",
            "-z",
            "noexecstack",
        ])
        .arg("--hash-style=both")
        .arg("--strip-all")
        .arg(format!(
            "--dynamic-list={}",
            src.join("dynamic.list").display()
        ))
        .arg("-o")
        .arg(&tmp)
        .arg(format!("@{}", list.display()));
    let o = crate::process::output_bounded(
        cmd,
        std::time::Duration::from_secs(600),
        "zig ld.lld (musl link)",
    )?;
    if !o.status.success() {
        return Err(format!(
            "musl libc.so link failed:\n{}",
            String::from_utf8_lossy(&o.stderr)
        ));
    }
    std::fs::rename(&tmp, &lib).map_err(|e| format!("cannot write {}: {e}", lib.display()))?;
    std::fs::read(&lib).map_err(|e| format!("cannot read {}: {e}", lib.display()))
}

#[cfg(test)]
mod tests {
    use super::mkalltypes;

    #[test]
    fn alltypes_lines_become_guarded_definitions() {
        let out = mkalltypes(
            "TYPEDEF unsigned _Addr size_t;\nSTRUCT timespec { time_t tv_sec; long tv_nsec; };\n#define X 1\n",
        );
        assert!(out.contains("#if defined(__NEED_size_t) && !defined(__DEFINED_size_t)\ntypedef unsigned _Addr size_t;\n#define __DEFINED_size_t\n#endif\n"));
        assert!(out.contains("#if defined(__NEED_struct_timespec) && !defined(__DEFINED_struct_timespec)\nstruct timespec { time_t tv_sec; long tv_nsec; };\n"));
        assert!(out.contains("#define X 1\n"));
    }
}
