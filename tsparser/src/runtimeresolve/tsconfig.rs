use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Deserialize;
use swc_common::FileName;
use thiserror::Error;

#[derive(Debug)]
pub struct TsConfigPathResolver {
    /// The base path from which relative paths are resolved.
    base: PathBuf,
    base_filename: FileName,

    /// The parsed paths, sorted by descending prefix length (before any '*' wildcard).
    paths: Vec<PathEntry>,
}

#[derive(Error, Debug)]
pub enum TsConfigError {
    #[error("reading {0} failed")]
    Read(PathBuf, #[source] io::Error),
    #[error("parsing {0} failed")]
    Parse(PathBuf, #[source] serde_json::Error),
    #[error("{0} has no parent directory")]
    NoParent(PathBuf),
}

impl TsConfigPathResolver {
    pub fn from_file(tsconfig_path: &Path) -> Result<Self, TsConfigError> {
        let tsconfig_dir = tsconfig_path
            .parent()
            .ok_or_else(|| TsConfigError::NoParent(tsconfig_path.to_path_buf()))?;
        let tsconfig = std::fs::read_to_string(tsconfig_path)
            .map_err(|e| TsConfigError::Read(tsconfig_path.to_path_buf(), e))?;
        let tsconfig = strip_jsonc_comments(&tsconfig, false);
        let tsconfig: TSConfig = serde_json::from_str(&tsconfig)
            .map_err(|e| TsConfigError::Parse(tsconfig_path.to_path_buf(), e))?;
        Ok(Self::from_config(tsconfig_dir, tsconfig))
    }

    pub fn from_config(tsconfig_dir: &Path, tsconfig: TSConfig) -> Self {
        let base = tsconfig
            .compiler_options
            .base_url
            .map(|p| tsconfig_dir.join(p))
            .unwrap_or(tsconfig_dir.to_path_buf());

        let mut paths: Vec<PathEntry> = tsconfig
            .compiler_options
            .paths
            .into_iter()
            .map(|(key, val)| {
                let key = PathVal::from(key);
                let values = val.into_iter().map(PathVal::from).collect();
                PathEntry { key, values }
            })
            .collect();

        // Sort the paths by descending prefix length.
        paths.sort_by(|a, b| {
            let a = a.key.prefix_len();
            let b = b.key.prefix_len();
            b.cmp(&a)
        });

        let base_filename = FileName::Real(base.clone());

        Self {
            base,
            base_filename,
            paths,
        }
    }

    pub fn base(&self) -> &FileName {
        &self.base_filename
    }

    pub fn resolve(&self, import: &str) -> Option<Cow<'_, str>> {
        for entry in &self.paths {
            match &entry.key {
                PathVal::Exact(key) if import == key => {
                    for val in &entry.values {
                        if let PathVal::Exact(val) = val {
                            for candidate in file_candidates(self.base.join(val)) {
                                if candidate.exists() {
                                    return Some(Cow::Borrowed(val));
                                }
                            }
                        }
                    }
                }

                PathVal::Wildcard { prefix, suffix }
                    if import.starts_with(prefix) && import.ends_with(suffix) =>
                {
                    let wildcard = &import[prefix.len()..import.len() - suffix.len()];
                    for val in &entry.values {
                        match val {
                            PathVal::Wildcard { prefix, suffix } => {
                                let mut rel_path = String::with_capacity(
                                    prefix.len() + suffix.len() + wildcard.len() + 2,
                                );
                                rel_path.push_str(prefix);
                                rel_path.push_str(wildcard);
                                rel_path.push_str(suffix);

                                for candidate in file_candidates(self.base.join(&rel_path)) {
                                    if candidate.exists() {
                                        return Some(Cow::Owned(rel_path));
                                    }
                                }
                            }
                            PathVal::Exact(val) => {
                                for candidate in file_candidates(self.base.join(val)) {
                                    if candidate.exists() {
                                        return Some(Cow::Borrowed(val));
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        None
    }
}

#[derive(Debug)]
struct PathEntry {
    key: PathVal,
    values: Vec<PathVal>,
}

/// Represents a path, possibly with a wildcard.
#[derive(Debug)]
enum PathVal {
    /// The path is an exact path.
    Exact(String),

    /// The path is a wildcard path, with the given prefix and suffix surrounding the '*'.
    Wildcard { prefix: String, suffix: String },
}

impl PathVal {
    /// Returns the length of the prefix, or of the full path if it is an exact path.
    fn prefix_len(&self) -> usize {
        match self {
            PathVal::Exact(s) => s.len(),
            PathVal::Wildcard { prefix, .. } => prefix.len(),
        }
    }
}

impl From<String> for PathVal {
    fn from(s: String) -> Self {
        match s.split_once('*') {
            Some((prefix, suffix)) => PathVal::Wildcard {
                prefix: prefix.to_string(),
                suffix: suffix.to_string(),
            },
            None => PathVal::Exact(s),
        }
    }
}

#[derive(Deserialize)]
pub struct TSConfig {
    #[serde(default, rename = "compilerOptions")]
    pub compiler_options: CompilerOptions,
}

#[derive(Deserialize, Default)]
pub struct CompilerOptions {
    #[serde(rename = "baseUrl")]
    pub base_url: Option<PathBuf>,
    pub paths: IndexMap<String, Vec<String>>,
}

/// Takes a string of jsonc content and returns a comment free version
/// which should parse fine as regular json.
/// Nested block comments are supported.
/// preserve_locations will replace most comments with spaces, so that JSON parsing
/// errors should point to the right location.
///
/// From https://github.com/serde-rs/json/issues/168#issuecomment-761831843.
fn strip_jsonc_comments(jsonc_input: &str, preserve_locations: bool) -> String {
    let mut json_output = String::new();

    let mut block_comment_depth: u8 = 0;
    let mut is_in_string: bool = false; // Comments cannot be in strings

    for line in jsonc_input.split('\n') {
        let mut last_char: Option<char> = None;
        for cur_char in line.chars() {
            // Check whether we're in a string
            if block_comment_depth == 0 && last_char != Some('\\') && cur_char == '"' {
                is_in_string = !is_in_string;
            }

            // Check for line comment start
            if !is_in_string && last_char == Some('/') && cur_char == '/' {
                last_char = None;
                if preserve_locations {
                    json_output.push_str("  ");
                }
                break; // Stop outputting or parsing this line
            }
            // Check for block comment start
            if !is_in_string && last_char == Some('/') && cur_char == '*' {
                block_comment_depth += 1;
                last_char = None;
                if preserve_locations {
                    json_output.push_str("  ");
                }
                // Check for block comment end
            } else if !is_in_string && last_char == Some('*') && cur_char == '/' {
                block_comment_depth = block_comment_depth.saturating_sub(1);
                last_char = None;
                if preserve_locations {
                    json_output.push_str("  ");
                }
                // Output last char if not in any block comment
            } else {
                if block_comment_depth == 0 {
                    if let Some(last_char) = last_char {
                        json_output.push(last_char);
                    }
                } else if preserve_locations {
                    json_output.push(' ');
                }
                last_char = Some(cur_char);
            }
        }

        // Add last char and newline if not in any block comment
        if let Some(last_char) = last_char {
            if block_comment_depth == 0 {
                json_output.push(last_char);
            } else if preserve_locations {
                json_output.push(' ');
            }
        }

        // Remove trailing whitespace from line
        while json_output.ends_with(' ') {
            json_output.pop();
        }
        json_output.push('\n');
    }

    json_output
}

struct PathResolveIterator {
    base: PathBuf,
    base_ext: Option<String>,
    candidates: &'static [&'static str],
    idx: usize,
}

impl Iterator for PathResolveIterator {
    type Item = PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.candidates.len() {
            let candidate = self.candidates[self.idx];
            self.idx += 1;

            if candidate.is_empty() {
                Some(self.base.clone())
            } else if let Some(base_ext) = &self.base_ext {
                // Combine the candidate extension with the base extension.
                let ext = format!("{base_ext}.{candidate}");
                Some(self.base.with_extension(ext))
            } else {
                Some(self.base.with_extension(candidate))
            }
        } else {
            None
        }
    }
}

fn file_candidates(base: PathBuf) -> impl Iterator<Item = PathBuf> {
    let base_ext = base
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());

    let candidates: &'static [&'static str] = match base_ext.as_deref() {
        Some("js") => &["ts", "tsx", "d.ts", "js", "jsx"],
        Some("mjs") => &["mts", "d.mts", "mjs"],
        Some("cjs") => &["cts", "d.cts", "cjs"],

        // If we don't have an extension, or it's not recognized, try all extensions.
        _ => &[
            "ts", "tsx", "d.ts", "js", "jsx", "mts", "d.mts", "mjs", "cts", "d.cts", "cjs", "",
        ],
    };

    PathResolveIterator {
        base,
        candidates,
        base_ext,
        idx: 0,
    }
}
