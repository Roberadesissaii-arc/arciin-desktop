//! Where a credential is allowed to go, and where it is not.
//!
//! The rule is narrow enough to state in a sentence: the trusted-device
//! credential goes from the server into Windows Credential Manager, and from
//! there into one `Authorization` header. It reaches no log, no URL, and no
//! part of the UI.
//!
//! Stating it is easy; keeping it true across changes is not, because each of
//! those leaks is one careless line away — a `tracing::info!` that interpolates
//! the wrong variable, a debug field added to a struct that is serialised to
//! the renderer. These tests fail on the line, not on the incident.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src/`, with its path.
fn rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).expect("src must be readable") {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("source must be UTF-8");
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&src_dir(), &mut out);
    assert!(!out.is_empty(), "found no sources to audit");
    out
}

/// A line with its string literals blanked out.
///
/// The distinction that matters: `tracing::info!("credential stored")` says the
/// word in a message, which is fine and useful. `tracing::info!(?credential)`
/// prints the secret. Only the second survives this.
fn without_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in line.chars() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            _ if !in_string => out.push(ch),
            _ => {}
        }
    }
    out
}

#[test]
fn no_logging_call_ever_interpolates_a_secret() {
    // Words that name something that must never be printed. Checked against
    // the line with its message text removed, so saying "credential" in a
    // message is fine and passing one as a field is not.
    const FORBIDDEN: [&str; 6] = [
        "credential",
        "secret",
        "password",
        "cookie",
        "authorization",
        "arcsync",
    ];
    // Names that merely *contain* a forbidden word but carry no secret.
    const ALLOWED: [&str; 4] = [
        "credential_issued",
        "credentials",
        "credential_store",
        "session_cookie_header",
    ];

    let mut offences = Vec::new();
    for (path, text) in rust_sources() {
        for (number, line) in text.lines().enumerate() {
            if !line.contains("tracing::") {
                continue;
            }
            let mut code = without_string_literals(line).to_lowercase();
            for allowed in ALLOWED {
                code = code.replace(allowed, "");
            }
            for word in FORBIDDEN {
                if code.contains(word) {
                    offences.push(format!(
                        "{}:{} passes `{word}` to a logging macro",
                        path.display(),
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "secrets must never reach a log:\n{}",
        offences.join("\n")
    );
}

/// The body of one `impl` block, by brace depth.
fn impl_block<'a>(source: &'a str, header: &str) -> &'a str {
    let start = source
        .find(header)
        .unwrap_or_else(|| panic!("{header} not found"));
    let body = &source[start + header.len()..];
    let mut depth = 1usize;
    for (index, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..index];
                }
            }
            _ => {}
        }
    }
    panic!("{header} is unbalanced");
}

#[test]
fn the_issued_credential_leaves_its_carrier_exactly_once() {
    // `PairOutcome` is the only thing that ever holds a freshly issued
    // credential. It exists to be consumed once, at the point the secret is
    // handed to Windows Credential Manager — so the field must have no
    // accessor that could hand it anywhere else.
    let pairing = std::fs::read_to_string(src_dir().join("pairing").join("mod.rs"))
        .expect("pairing/mod.rs must exist");

    let body = impl_block(&pairing, "impl PairOutcome {");
    let uses: Vec<&str> = body
        .lines()
        .filter(|line| without_string_literals(line).contains("self.credential"))
        .map(str::trim)
        .collect();

    assert_eq!(
        uses.len(),
        1,
        "the issued credential must be readable in exactly one place, found: {uses:?}"
    );
    assert!(
        body.contains("pub fn into_credential(self)"),
        "and that place must consume the outcome, so it cannot be read twice"
    );
}

#[test]
fn no_url_is_ever_built_from_a_credential() {
    // The failure this guards: a credential in a path or query string. URLs are
    // written down everywhere — access logs, proxies, error reports — so one
    // there leaks into places nobody is guarding.
    //
    // Every source, not one file: a URL can be built anywhere, and the module
    // that used to be the only place this mattered has since been deleted.
    let mut offences = Vec::new();
    for (path, text) in rust_sources() {
        for (number, line) in text.lines().enumerate() {
            let joins_a_url = line.contains(".join(") || line.contains("format!(\"/");
            if joins_a_url && line.to_lowercase().contains("credential") {
                offences.push(format!(
                    "{}:{} builds a URL from a credential: {}",
                    path.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offences.is_empty(),
        "a credential must never reach a URL:
{}",
        offences.join(
            "
"
        )
    );
}
