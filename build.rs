use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TOKEN_FILES: [&str; 2] = ["tokens.slint", "theme.slint"];
const ALLOWED_PX: [&str; 2] = ["0px", "1px"];
const ALLOWED_DUR: [&str; 3] = ["0ms", "1ms", "0s"];

fn main() {
    check_tokens();
    slint_build::compile("ui/app.slint").expect("slint compile");
}

fn slint_files(root: &Path) -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(root.join("ui"))
        .expect("read ui/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "slint"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            println!("cargo:rerun-if-changed={}", p.display());
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(p).expect("read .slint file"),
            )
        })
        .collect()
}

fn literals(line: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let (mut hex, mut px, mut dur) = (Vec::new(), Vec::new(), Vec::new());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' {
            let (mut j, mut digits) = (i + 1, String::new());
            while j < chars.len() && chars[j].is_ascii_hexdigit() {
                digits.push(chars[j]);
                j += 1;
            }
            if (6..=8).contains(&digits.len()) {
                hex.push(format!("#{digits}"));
            }
            i = j.max(i + 1);
        } else if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let unit_start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let unit: String = chars[unit_start..i].iter().collect();
            let literal: String = chars[start..i].iter().collect();
            match unit.as_str() {
                "px" => px.push(literal),
                "ms" | "s" => dur.push(literal),
                _ => {}
            }
        } else {
            i += 1;
        }
    }
    (hex, px, dur)
}

fn defined_tokens(tokens: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut global: Option<String> = None;
    for line in tokens.lines() {
        if let Some(rest) = line.strip_prefix("export global ") {
            global = rest.split_whitespace().next().map(str::to_owned);
        } else if line == "}" {
            global = None;
        } else if let (Some(g), Some(after)) = (&global, line.split("property <").nth(1)) {
            if let Some(name) = after
                .split('>')
                .nth(1)
                .and_then(|s| s.split(':').next())
                .map(str::trim)
            {
                out.push((g.clone(), name.to_owned()));
            }
        }
    }
    out
}

fn palette(text: &str, prefix: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (at, _) in text.match_indices(prefix) {
        let hex_start = at + prefix.len();
        let hex: String = text[hex_start..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let name: String = text[..at]
            .trim_end_matches(':')
            .trim_end()
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if hex.len() == 6 && !name.is_empty() {
            out.insert(name.replace('-', "_"), hex.to_uppercase());
        }
    }
    out
}

fn check_tokens() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed={}", root.join("src/settings.rs").display());
    let files = slint_files(&root);
    let mut problems: Vec<String> = Vec::new();

    for (name, text) in &files {
        if TOKEN_FILES.contains(&name.as_str()) {
            continue;
        }
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split("//").next().unwrap_or("");
            let (hex, px, dur) = literals(line);
            for h in hex {
                problems.push(format!("{name}:{}: raw color {h} — use Theme.c.* or Fixed.*", n + 1));
            }
            for p in px {
                if !ALLOWED_PX.contains(&p.as_str()) {
                    problems.push(format!("{name}:{}: raw length {p} — add/use a token in ui/tokens.slint", n + 1));
                }
            }
            for d in dur {
                if !ALLOWED_DUR.contains(&d.as_str()) {
                    problems.push(format!("{name}:{}: raw duration {d} — add/use a Motion.* token", n + 1));
                }
            }
        }
    }

    let get = |name: &str| files.iter().find(|(n, _)| n == name).map_or("", |(_, t)| t.as_str());
    let others: String = files
        .iter()
        .filter(|(n, _)| n != "tokens.slint")
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for (g, tok) in defined_tokens(get("tokens.slint")) {
        if !others.contains(&format!("{g}.{tok}")) {
            problems.push(format!("tokens.slint: unused token {g}.{tok} — use it or delete it"));
        }
    }
    for (name, text) in &files {
        for line in text.lines().filter(|l| l.contains("from \"tokens.slint\"")) {
            if let (Some(a), Some(b)) = (line.find('{'), line.find('}')) {
                for imp in line[a + 1..b].split(',').map(str::trim) {
                    if !text.contains(&format!("{imp}.")) {
                        problems.push(format!("{name}: unused tokens.slint import {imp}"));
                    }
                }
            }
        }
    }

    let slint_linen = palette(get("theme.slint"), ": #");
    let settings = std::fs::read_to_string(root.join("src/settings.rs")).expect("read settings.rs");
    let linen = &settings[settings.find("(\"Linen\"").expect("Linen palette")..];
    let linen = &linen[..linen.find("}),").expect("end of Linen palette")];
    let rust_linen = palette(linen, ": c(0x");
    let keys: std::collections::BTreeSet<_> = slint_linen.keys().chain(rust_linen.keys()).collect();
    for k in keys {
        if slint_linen.get(k) != rust_linen.get(k) {
            problems.push(format!(
                "Linen drift: {k} is #{} in ui/theme.slint but 0x{} in src/settings.rs",
                slint_linen.get(k).map_or("?", String::as_str),
                rust_linen.get(k).map_or("?", String::as_str)
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "design-token guard (build.rs): {} violation(s)\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}
