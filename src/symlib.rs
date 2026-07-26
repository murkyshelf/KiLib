//! Minimal s-expression handling for KiCad `.kicad_sym` libraries.
//!
//! Enough to split a library into its top-level `(symbol "NAME" ...)` blocks
//! and splice them into a combined library, without reformatting the parts of
//! the file we do not touch.

use std::collections::HashSet;
use std::path::Path;

use crate::logging;

const DEFAULT_HEADER: &str = "(kicad_symbol_lib (version 20211014) (generator cse-importer)";

#[derive(Clone, Debug)]
pub struct SymbolBlock {
    pub name: String,
    /// Verbatim source text, from the opening `(` to its matching `)`.
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct SymbolLib {
    /// Everything before the first symbol, i.e. `(kicad_symbol_lib (version …)`
    /// together with any non-symbol children such as `(generator …)`.
    pub header: String,
    pub symbols: Vec<SymbolBlock>,
}

/// Splits a `.kicad_sym` file into its header and top-level symbol blocks.
pub fn parse(text: &str) -> Result<SymbolLib, String> {
    let bytes = text.as_bytes();
    let mut i = 0;

    // Find the opening paren of the library form.
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'(' {
        return Err("not a kicad_symbol_lib: no opening '('".to_string());
    }

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut child_start: Option<usize> = None;
    let mut symbols: Vec<SymbolBlock> = Vec::new();
    let mut first_symbol_start: Option<usize> = None;
    let mut lib_close: Option<usize> = None;

    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'(' => {
                depth += 1;
                if depth == 2 {
                    child_start = Some(i);
                }
            }
            b')' => {
                if depth == 0 {
                    return Err(format!("unbalanced ')' at byte {i}"));
                }
                depth -= 1;
                if depth == 1 {
                    if let Some(start) = child_start.take() {
                        let end = i + 1;
                        let block = &text[start..end];
                        if head_atom(block) == Some("symbol") {
                            let name = first_quoted(block)
                                .ok_or_else(|| format!("symbol at byte {start} has no name"))?;
                            first_symbol_start.get_or_insert(start);
                            symbols.push(SymbolBlock {
                                name,
                                text: block.to_string(),
                            });
                        }
                    }
                } else if depth == 0 {
                    lib_close = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if depth != 0 && lib_close.is_none() {
        return Err("unbalanced parentheses: library never closes".to_string());
    }

    let header_end = first_symbol_start
        .or(lib_close)
        .ok_or_else(|| "could not locate library body".to_string())?;

    Ok(SymbolLib {
        header: text[..header_end].trim_end().to_string(),
        symbols,
    })
}

/// Serialises a library back to `.kicad_sym` text.
pub fn render(lib: &SymbolLib) -> String {
    let mut out = String::with_capacity(lib.header.len() + 64);
    out.push_str(&lib.header);
    out.push('\n');
    for symbol in &lib.symbols {
        // Top-level symbols sit at one level of indentation.
        out.push_str("  ");
        out.push_str(symbol.text.trim_end());
        out.push('\n');
    }
    out.push_str(")\n");
    out
}

/// Adds every symbol in `source_text` to the library at `lib_path`, replacing
/// any symbol already present under the same name. Returns the names merged.
pub fn merge_into(lib_path: &Path, source_text: &str, backup: bool) -> Result<Vec<String>, String> {
    let incoming = parse(source_text)?;
    if incoming.symbols.is_empty() {
        return Ok(Vec::new());
    }

    let mut target = if lib_path.is_file() {
        let existing = std::fs::read_to_string(lib_path)
            .map_err(|e| format!("reading {}: {e}", lib_path.display()))?;
        parse(&existing).map_err(|e| format!("parsing {}: {e}", lib_path.display()))?
    } else {
        SymbolLib {
            header: DEFAULT_HEADER.to_string(),
            symbols: Vec::new(),
        }
    };

    let names: Vec<String> = incoming.symbols.iter().map(|s| s.name.clone()).collect();
    let incoming_names: HashSet<&str> = names.iter().map(String::as_str).collect();

    let replaced = target
        .symbols
        .iter()
        .filter(|s| incoming_names.contains(s.name.as_str()))
        .count();
    target
        .symbols
        .retain(|s| !incoming_names.contains(s.name.as_str()));
    target.symbols.extend(incoming.symbols);
    // Stable ordering keeps diffs of the merged library readable.
    target.symbols.sort_by_key(|s| s.name.to_lowercase());

    if backup {
        backup_file(lib_path)?;
    }
    if let Some(parent) = lib_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(lib_path, render(&target))
        .map_err(|e| format!("writing {}: {e}", lib_path.display()))?;

    logging::info(format!(
        "merged {} symbol(s) into {} ({replaced} replaced, {} total)",
        names.len(),
        lib_path.display(),
        target.symbols.len()
    ));
    Ok(names)
}

/// Copies `path` to `<stem>.bak` when it already exists, matching how KiCad
/// itself keeps a previous revision alongside a library.
pub fn backup_file(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let backup = path.with_extension("bak");
    std::fs::copy(path, &backup)
        .map_err(|e| format!("backing up {} to {}: {e}", path.display(), backup.display()))?;
    logging::info(format!(
        "backed up {} -> {}",
        path.display(),
        backup.display()
    ));
    Ok(())
}

/// The names of the top-level symbols in a library, for the manifest.
pub fn symbol_names(text: &str) -> Vec<String> {
    parse(text)
        .map(|lib| lib.symbols.into_iter().map(|s| s.name).collect())
        .unwrap_or_default()
}

/// The first atom inside a form: `(symbol "X" …)` -> `symbol`.
fn head_atom(block: &str) -> Option<&str> {
    let rest = block.strip_prefix('(')?;
    let trimmed = rest.trim_start();
    let end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .unwrap_or(trimmed.len());
    (end > 0).then(|| &trimmed[..end])
}

/// The first double-quoted string in a form, with escapes unwrapped.
fn first_quoted(block: &str) -> Option<String> {
    let bytes = block.as_bytes();
    let start = bytes.iter().position(|&b| b == b'"')? + 1;
    let mut out = String::new();
    let mut i = start;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if escaped {
            out.push(c as char);
            escaped = false;
        } else if c == b'\\' {
            escaped = true;
        } else if c == b'"' {
            return Some(out);
        } else {
            // Push the whole UTF-8 sequence, not the raw byte.
            let ch = block[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        i += 1;
    }
    None
}
