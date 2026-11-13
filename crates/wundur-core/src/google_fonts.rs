//! Google Fonts: the catalogue (every family, its kind and weights), and fetching a family's
//! files when the person picks it for the app, the chat or the terminals. All of them are open
//! (SIL Open Font License, Apache or Ubuntu Font License): free to download and use.
//!
//! The catalogue is the one fonts.google.com reads (no key needed), kept for a week. A family's
//! files come from the CSS API asked as an old browser, which answers with one TrueType file per
//! weight. They are kept in `<config>/fonts/<family>/<weight>.ttf`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

const CATALOGUE: &str = "https://fonts.google.com/metadata/fonts";
/// The weights fetched, when the family has them.
const WEIGHTS: [u16; 4] = [400, 500, 600, 700];
/// How long a kept catalogue is used before asking again.
const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    /// "Sans Serif", "Serif", "Display", "Handwriting" or "Monospace".
    pub category: String,
    /// Its upright weights.
    pub weights: Vec<u16>,
    /// Rank by use (1: the most used).
    pub popularity: u32,
}

impl Family {
    pub fn is_mono(&self) -> bool {
        self.category == "Monospace"
    }
}

/// Where fetched fonts and the catalogue are kept.
pub fn dir() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("fonts"))
}

/// The catalogue's families, most used first.
pub fn parse_catalogue(text: &str) -> Result<Vec<Family>> {
    // The site's answers may start with a guard line.
    let text = text.trim_start_matches(")]}'").trim_start();
    let v: Value = serde_json::from_str(text).context("the catalogue is not JSON")?;
    let list = v.get("familyMetadataList").and_then(Value::as_array).ok_or_else(|| anyhow!("the catalogue has no families"))?;
    let mut out: Vec<Family> = list
        .iter()
        .filter_map(|f| {
            let mut weights: Vec<u16> = f.get("fonts")?.as_object()?.keys().filter_map(|k| k.parse().ok()).collect();
            weights.sort_unstable();
            Some(Family {
                name: f.get("family")?.as_str()?.to_string(),
                category: f.get("category").and_then(Value::as_str).unwrap_or_default().to_string(),
                weights,
                popularity: f.get("popularity").and_then(Value::as_u64).unwrap_or(u64::MAX) as u32,
            })
        })
        .filter(|f| !f.weights.is_empty())
        .collect();
    out.sort_by_key(|f| f.popularity);
    Ok(out)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into()
}

/// The catalogue: the kept one while it is fresh, else asked for (and kept); the kept one, even
/// old, when asking fails.
pub fn catalogue() -> Result<Vec<Family>> {
    let kept = dir().map(|d| d.join("catalogue.json"));
    let fresh = kept.as_ref().and_then(|p| p.metadata().ok()).and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < KEEP);
    if fresh && let Some(list) = kept.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| parse_catalogue(&t).ok()) {
        return Ok(list);
    }
    let asked = agent().get(CATALOGUE).call().map_err(anyhow::Error::from).and_then(|mut r| r.body_mut().read_to_string().map_err(anyhow::Error::from));
    match asked {
        Ok(text) => {
            let list = parse_catalogue(&text)?;
            if let Some(p) = &kept {
                let _ = std::fs::create_dir_all(p.parent().unwrap_or(p));
                let _ = std::fs::write(p, &text);
            }
            Ok(list)
        }
        Err(e) => kept
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| parse_catalogue(&t).ok())
            .ok_or_else(|| anyhow!("could not reach Google Fonts: {e}")),
    }
}

/// The weights fetched for `family`: the usual four it has, else the one nearest regular.
pub fn weights_for(family: &Family) -> Vec<u16> {
    let usual: Vec<u16> = WEIGHTS.iter().copied().filter(|w| family.weights.contains(w)).collect();
    if !usual.is_empty() {
        return usual;
    }
    family.weights.iter().copied().min_by_key(|w| (*w as i32 - 400).abs()).into_iter().collect()
}

/// The CSS API's answer: each weight's file address.
pub fn parse_css(css: &str) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    let mut weight = None;
    for line in css.lines() {
        let line = line.trim();
        if let Some(w) = line.strip_prefix("font-weight:") {
            weight = w.trim().trim_end_matches(';').trim().parse().ok();
        } else if let Some(start) = line.find("url(") {
            let rest = &line[start + 4..];
            if let (Some(w), Some(end)) = (weight.take(), rest.find(')')) {
                out.push((w, rest[..end].to_string()));
            }
        }
    }
    out
}

/// The folder a family's files are kept in.
pub fn family_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join(name.replace(['/', '\\'], "_"))
}

/// Fetch `family`'s files into `dir` (those not kept yet); the files.
pub fn fetch(family: &Family, dir: &Path) -> Result<Vec<PathBuf>> {
    let weights = weights_for(family);
    let list = weights.iter().map(u16::to_string).collect::<Vec<_>>().join(";");
    let url = format!("https://fonts.googleapis.com/css2?family={}:wght@{list}", family.name.replace(' ', "+"));
    let agent = agent();
    let css = agent.get(&url).header("User-Agent", "Mozilla/4.0").call()?.body_mut().read_to_string()?;
    let files = parse_css(&css);
    if files.is_empty() {
        return Err(anyhow!("Google Fonts sent no files for {}", family.name));
    }
    let to = family_dir(dir, &family.name);
    std::fs::create_dir_all(&to)?;
    let mut out = Vec::new();
    for (w, url) in files {
        let path = to.join(format!("{w}.ttf"));
        if !path.exists() {
            let bytes = agent.get(&url).call()?.body_mut().with_config().limit(20 * 1024 * 1024).read_to_vec()?;
            let part = path.with_extension("part");
            std::fs::write(&part, bytes)?;
            std::fs::rename(&part, &path)?;
        }
        out.push(path);
    }
    Ok(out)
}

/// The families fetched before, with their files.
pub fn fetched(dir: &Path) -> Vec<(String, Vec<PathBuf>)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let files: Vec<PathBuf> = std::fs::read_dir(e.path()).ok()?.flatten().map(|f| f.path()).filter(|p| p.extension().is_some_and(|x| x == "ttf")).collect();
            (!files.is_empty()).then(|| (e.file_name().to_string_lossy().to_string(), files))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_reads_families_by_use_with_their_upright_weights() {
        let text = r#")]}'
{"familyMetadataList": [
  {"family": "Zed Mono", "category": "Monospace", "popularity": 40, "fonts": {"400": {}, "400i": {}, "700": {}}},
  {"family": "Inter", "category": "Sans Serif", "popularity": 5, "fonts": {"300": {}, "400": {}, "500": {}}}
]}"#;
        let list = parse_catalogue(text).unwrap();
        assert_eq!(list[0].name, "Inter");
        assert_eq!(list[1].weights, [400, 700], "italics are not weights");
        assert!(list[1].is_mono());
    }

    #[test]
    fn the_usual_weights_are_fetched_or_the_one_nearest_regular() {
        let f = |w: &[u16]| Family { name: "X".into(), category: String::new(), weights: w.to_vec(), popularity: 1 };
        assert_eq!(weights_for(&f(&[100, 400, 700, 900])), [400, 700]);
        assert_eq!(weights_for(&f(&[300])), [300]);
        assert_eq!(weights_for(&f(&[100, 800])), [100]);
    }

    #[test]
    fn the_css_gives_each_weight_s_file() {
        let css = "@font-face {\n  font-family: 'Inter';\n  font-weight: 400;\n  src: url(https://fonts.gstatic.com/a.ttf) format('truetype');\n}\n@font-face {\n  font-weight: 700;\n  src: url(https://fonts.gstatic.com/b.ttf) format('truetype');\n}\n";
        assert_eq!(parse_css(css), [(400, "https://fonts.gstatic.com/a.ttf".to_string()), (700, "https://fonts.gstatic.com/b.ttf".to_string())]);
    }
}
