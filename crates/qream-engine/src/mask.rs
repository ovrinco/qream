//! Secrets masked before anything leaves the daemon (RW-17-R03): the house patterns are
//! data in `rules/secrets.toml`; pane text and mail bodies pass through `Mask::apply`, and
//! `submit` refuses a report in which `Mask::findings` names a line.
use crate::{Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::LazyLock};

const RULES: &str = include_str!("../rules/secrets.toml");

#[derive(Deserialize)]
struct Table {
    replacement: String,
    pattern: Vec<PatternRow>,
}
#[derive(Deserialize)]
struct PatternRow {
    name: String,
    regex: String,
}

pub struct Mask {
    replacement: String,
    patterns: Vec<(String, Regex)>,
}

/// One unmasked secret: the 1-based line it starts on and the pattern's name, never the secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub line: usize,
    pub pattern: String,
}

static HOUSE: LazyLock<Mask> = LazyLock::new(|| {
    Mask::parse(RULES).expect("rules/secrets.toml is fixed at build time and tested")
});

impl Mask {
    /// The house patterns.
    pub fn house() -> &'static Mask {
        &HOUSE
    }
    pub fn parse(text: &str) -> Result<Mask> {
        let table: Table = toml::from_str(text)?;
        let patterns = table
            .pattern
            .into_iter()
            .map(|p| Ok((p.name, Regex::new(&p.regex)?)))
            .collect::<Result<Vec<_>>>()?;
        Ok(Mask {
            replacement: table.replacement,
            patterns,
        })
    }
    /// The text with every secret replaced by the replacement word.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (_, regex) in &self.patterns {
            if regex.is_match(&out) {
                out = regex
                    .replace_all(&out, self.replacement.as_str())
                    .into_owned();
            }
        }
        out
    }
    /// The lines holding an unmasked secret, in line order.
    pub fn findings(&self, text: &str) -> Vec<Finding> {
        let mut found: Vec<Finding> = self
            .patterns
            .iter()
            .flat_map(|(name, regex)| {
                regex.find_iter(text).map(move |m| Finding {
                    line: text[..m.start()].matches('\n').count() + 1,
                    pattern: name.clone(),
                })
            })
            .collect();
        // One finding per line, named by the first pattern of the table that matched it.
        found.sort_by_key(|f| f.line);
        found.dedup_by_key(|f| f.line);
        found
    }
}

/// The seal gate of `submit`: a report holding an unmasked secret is refused with its line.
pub fn gate_report(report: &Path) -> Result<usize> {
    let text = crate::error::read(report)?;
    match Mask::house().findings(&text).first() {
        Some(f) => Err(Error::Denied(format!(
            "The report holds an unmasked {} on line {}; write it as [masked] and submit again",
            f.pattern, f.line
        ))),
        None => Ok(text.lines().count()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_house_patterns_mask_each_kind_and_leave_plain_text() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----";
        let text = format!(
            "a sk-ant-api03-{a}\nb sk-proj-{a}\nc Authorization: Bearer {a}\n{pem}\nplain sk-short words",
            a = "Abcdefghijklmnopqrstuvwxyz012345"
        );
        let masked = Mask::house().apply(&text);
        assert_eq!(
            masked,
            "a [masked]\nb [masked]\nc Authorization: [masked]\n[masked]\nplain sk-short words"
        );
        let lines: Vec<usize> = Mask::house()
            .findings(&text)
            .iter()
            .map(|f| f.line)
            .collect();
        assert_eq!(lines, vec![1, 2, 3, 4]);
        assert!(Mask::house().findings(&masked).is_empty());
    }
}
