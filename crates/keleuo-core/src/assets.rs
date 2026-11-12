//! Game assets made step by step, the person approving each step (see
//! `docs/proposals/asset-pipeline.md`): a concept image iterated at low resolution, three views
//! from it, then a model and its rig. An asset is a folder in the project,
//! `assets/<kind>/<name>/`, with its record, `asset.json`: its brief, every attempt (what was
//! asked, the file it made, what it cost) and which one was approved at each step. Making the
//! images goes through the game-assets pack, with the person's own keys.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The record's file name, in the asset's folder.
pub const RECORD: &str = "asset.json";

/// A step of making an asset, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Concept,
    Views,
    Model,
    Rig,
    Done,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Concept => "Concept",
            Stage::Views => "Views",
            Stage::Model => "Model",
            Stage::Rig => "Rig",
            Stage::Done => "Done",
        }
    }
}

/// One of the three views made from the approved concept, for the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Front,
    Side,
    Back,
}

impl View {
    pub const ALL: [View; 3] = [View::Front, View::Side, View::Back];

    pub fn label(self) -> &'static str {
        match self {
            View::Front => "Front",
            View::Side => "Side",
            View::Back => "Back",
        }
    }

    /// What the image model is asked, with the approved concept sent as the image.
    pub fn prompt(self, character: bool) -> String {
        let pose = if character { " standing in an A-pose, arms held a little away from the body, legs slightly apart" } else { "" };
        let view = match self {
            View::Front => "seen straight from the front",
            View::Side => "seen exactly from its right side, in profile",
            View::Back => "seen straight from behind",
        };
        format!(
            "The same subject as in the image, identical in design, colours, proportions and details, {view}{pose}. \
             The whole subject in frame and centred, nothing cropped; even soft lighting; a plain light grey background; \
             no text, no shadows on the background. An orthographic-looking view, for turning into a 3D model."
        )
    }
}

/// One try at a step: what it made and what it cost.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attempt {
    /// The file it made, relative to the asset's folder.
    pub file: String,
    /// What was asked: the prompt, or the change asked for.
    pub asked: String,
    /// The attempt it was made from (a change to an earlier one), by file.
    pub from: Option<String>,
    /// Which view, for a view.
    pub view: Option<View>,
    /// Its quality as asked of the service (`low`, `medium`).
    pub quality: String,
    /// What it cost, in US cents, as estimated from the service's price (not billed back).
    pub cents: f32,
    /// Seconds since 1970.
    pub at: u64,
}

/// An asset's record.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub name: String,
    /// `character`, `creature`, `prop`, `environment`.
    pub kind: String,
    /// What it is, as the person described it.
    pub brief: String,
    pub stage: Stage,
    pub concepts: Vec<Attempt>,
    /// The approved concept, by file.
    pub concept: Option<String>,
    pub views: Vec<Attempt>,
    /// The approved view of each kind, by file.
    pub approved_views: Vec<(View, String)>,
    pub models: Vec<Attempt>,
    pub model: Option<String>,
}

impl Asset {
    /// Everything it has cost so far, in cents.
    pub fn cents(&self) -> f32 {
        self.concepts.iter().chain(&self.views).chain(&self.models).map(|a| a.cents).sum()
    }

    /// Whether it is a character (views in an A-pose).
    pub fn is_character(&self) -> bool {
        matches!(self.kind.as_str(), "character" | "")
    }

    /// The approved view of `view`, by file.
    pub fn approved_view(&self, view: View) -> Option<&str> {
        self.approved_views.iter().find(|(v, _)| *v == view).map(|(_, f)| f.as_str())
    }

    /// Approve `file` as `view` (replacing an earlier one).
    pub fn approve_view(&mut self, view: View, file: &str) {
        self.approved_views.retain(|(v, _)| *v != view);
        self.approved_views.push((view, file.to_string()));
        if View::ALL.iter().all(|v| self.approved_view(*v).is_some()) && self.stage < Stage::Model {
            self.stage = Stage::Model;
        }
    }

    /// Approve the concept `file`: on to the views.
    pub fn approve_concept(&mut self, file: &str) {
        self.concept = Some(file.to_string());
        if self.stage < Stage::Views {
            self.stage = Stage::Views;
        }
    }
}

/// The folder for a new asset called `name` of `kind`, in `project`: `assets/<kind>s/<slug>`.
pub fn folder_for(project: &Path, kind: &str, name: &str) -> PathBuf {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let kind = if kind.is_empty() { "character" } else { kind };
    project.join("assets").join(format!("{kind}s")).join(if slug.is_empty() { "asset".into() } else { slug })
}

/// The record in `dir`, if there is one.
pub fn load(dir: &Path) -> Option<Asset> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(RECORD)).ok()?).ok()
}

/// Write the record in `dir`.
pub fn save(dir: &Path, asset: &Asset) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(RECORD), serde_json::to_string_pretty(asset).unwrap_or_default())
}

/// The estimated price of an image, in cents, by its quality (gpt-image at 1024 px; the owner's
/// measured figures: low about a cent, medium about ten).
pub fn image_cents(quality: &str) -> f32 {
    match quality {
        "low" => 1.0,
        "medium" => 10.0,
        "high" => 25.0,
        _ => 10.0,
    }
}

/// What to make next.
#[derive(Clone, Debug, PartialEq)]
pub enum Make {
    /// A concept from the brief (and the prompt as written now).
    Concept { prompt: String },
    /// A change to an earlier concept: `from` (a file in the asset's folder) and what to change.
    Change { from: String, note: String },
    /// One of the views, from the approved concept.
    View(View),
}

/// The game-assets pack's id, whose image tools the asset steps use.
pub const PACK: &str = "wundur/game-assets";

/// Make `what` for the asset in `dir`, through the game-assets `pack` run under `grant` (its
/// `cwd` the project; the person's keys and the pack's settings): the new attempt, recorded in
/// the asset (not saved; the caller saves). Blocking: off the UI thread.
pub fn make(dir: &Path, asset: &mut Asset, what: &Make, pack: &crate::packs::Pack, grant: &crate::packs::Grant) -> Result<Attempt, String> {
    let project = grant.cwd;
    let rel = |p: &Path| p.strip_prefix(project).unwrap_or(p).to_string_lossy().to_string();
    let n = asset.concepts.len() + asset.views.len() + 1;
    let (tool, args, quality, view, asked, from) = match what {
        Make::Concept { prompt } => {
            let prompt = if prompt.trim().is_empty() { asset.brief.clone() } else { prompt.clone() };
            let args = serde_json::json!({ "prompt": prompt, "quality": "low", "size": "1024x1024", "for_3d": true, "name": format!("concept-{n}") });
            ("make_image", args, "low", None, prompt, None)
        }
        Make::Change { from, note } => {
            let args = serde_json::json!({ "image": rel(&dir.join(from)), "prompt": note, "quality": "low", "size": "1024x1024", "name": format!("concept-{n}") });
            ("edit_image", args, "low", None, note.clone(), Some(from.clone()))
        }
        Make::View(v) => {
            let Some(concept) = asset.concept.clone() else { return Err("Approve a concept first.".into()) };
            let args = serde_json::json!({
                "image": rel(&dir.join(&concept)),
                "prompt": v.prompt(asset.is_character()),
                "quality": "medium",
                "size": "1024x1536",
                "name": format!("{}-{n}", v.label().to_lowercase()),
            });
            ("edit_image", args, "medium", Some(*v), v.prompt(asset.is_character()), Some(concept))
        }
    };
    let tool = pack.manifest.tools.iter().find(|t| t.name == tool).ok_or_else(|| format!("The game-assets pack has no {tool} tool."))?.clone();
    // Saved in the asset's own folder.
    let mut settings = grant.settings.clone();
    settings.insert("output_folder".into(), rel(dir));
    let grant = crate::packs::Grant { cwd: project, secret: grant.secret, settings: &settings, extra_hosts: grant.extra_hosts.clone(), allow_http: grant.allow_http };
    let out = crate::packs::run(pack, &tool, &args, &grant);
    if out.failed {
        return Err(out.text);
    }
    let made = out.images.first().ok_or_else(|| format!("No image came back ({}).", out.text))?;
    let attempt = Attempt {
        file: made.strip_prefix(dir).unwrap_or(made).to_string_lossy().to_string(),
        asked,
        from,
        view,
        quality: quality.into(),
        cents: image_cents(quality),
        at: now(),
    };
    match view {
        Some(_) => asset.views.push(attempt.clone()),
        None => asset.concepts.push(attempt.clone()),
    }
    Ok(attempt)
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_moves_on_as_its_steps_are_approved() {
        let mut a = Asset { name: "Knight".into(), kind: "character".into(), ..Default::default() };
        assert_eq!(a.stage, Stage::Concept);
        a.concepts.push(Attempt { file: "concept-1.png".into(), cents: 1.0, ..Default::default() });
        a.concepts.push(Attempt { file: "concept-2.png".into(), cents: 1.0, ..Default::default() });
        a.approve_concept("concept-2.png");
        assert_eq!((a.stage, a.concept.as_deref()), (Stage::Views, Some("concept-2.png")));
        a.approve_view(View::Front, "front-1.png");
        a.approve_view(View::Side, "side-1.png");
        assert_eq!(a.stage, Stage::Views, "still a view to go");
        a.approve_view(View::Front, "front-2.png");
        assert_eq!(a.approved_view(View::Front), Some("front-2.png"), "the later one replaces it");
        a.approve_view(View::Back, "back-1.png");
        assert_eq!(a.stage, Stage::Model);
        a.views.push(Attempt { file: "front-1.png".into(), cents: 10.0, ..Default::default() });
        assert_eq!(a.cents(), 12.0);
    }

    #[test]
    fn a_record_saves_and_loads_and_its_folder_is_named_for_it() {
        let dir = std::env::temp_dir().join(format!("keleuo-asset-{}", std::process::id()));
        let folder = folder_for(&dir, "character", "Sir Knight (red)");
        assert_eq!(folder, dir.join("assets/characters/sir-knight-red"));
        let a = Asset { name: "Sir Knight".into(), brief: "a red knight".into(), concept: Some("c.png".into()), ..Default::default() };
        save(&folder, &a).unwrap();
        assert_eq!(load(&folder), Some(a));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stand-in image service answering `count` requests with a PNG; what it was sent.
    fn stand_in(count: usize) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let png = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"\x89PNG art");
        (port, std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..count {
                let (mut s, _) = server.accept().unwrap();
                let mut got = Vec::new();
                let mut buf = vec![0u8; 65536];
                loop {
                    let n = s.read(&mut buf).unwrap();
                    got.extend(&buf[..n]);
                    let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") else { if n == 0 { break } else { continue } };
                    let head = String::from_utf8_lossy(&got[..end]).to_lowercase();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
                    if got.len() >= end + 4 + len || n == 0 { break }
                }
                let body = format!("{{\"data\":[{{\"b64_json\":\"{png}\"}}]}}");
                let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                seen.push(String::from_utf8_lossy(&got).to_string());
            }
            seen
        }))
    }

    #[test]
    fn concepts_changes_and_views_are_made_into_the_assets_folder() {
        let project = std::env::temp_dir().join(format!("keleuo-asset-make-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        let dir = folder_for(&project, "character", "Knight");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/game-assets");
        let manifest = serde_json::from_str(&std::fs::read_to_string(root.join("pack.json")).unwrap()).unwrap();
        let pack = crate::packs::Pack { manifest, dir: root };
        let (port, seen) = stand_in(3);
        let settings: std::collections::BTreeMap<String, String> = [("openai_url".to_string(), format!("http://127.0.0.1:{port}/v1"))].into();
        let secret = |n: &str| (n == "OPENAI_API_KEY").then(|| "sk-test-not-a-real-key".to_string());
        let grant = crate::packs::Grant { cwd: &project, secret: &secret, settings: &settings, extra_hosts: vec!["127.0.0.1".into()], allow_http: true };
        let mut a = Asset { name: "Knight".into(), kind: "character".into(), brief: "a red knight".into(), ..Default::default() };
        let first = make(&dir, &mut a, &Make::Concept { prompt: String::new() }, &pack, &grant).unwrap();
        assert_eq!((first.file.as_str(), first.asked.as_str(), first.cents), ("concept-1.png", "a red knight", 1.0));
        assert!(dir.join("concept-1.png").is_file());
        let second = make(&dir, &mut a, &Make::Change { from: "concept-1.png".into(), note: "a blue cape".into() }, &pack, &grant).unwrap();
        assert_eq!((second.file.as_str(), second.from.as_deref()), ("concept-2.png", Some("concept-1.png")));
        assert!(make(&dir, &mut a, &Make::View(View::Front), &pack, &grant).is_err(), "no concept approved yet");
        a.approve_concept("concept-2.png");
        let front = make(&dir, &mut a, &Make::View(View::Front), &pack, &grant).unwrap();
        assert_eq!((front.file.as_str(), front.view, front.cents), ("front-3.png", Some(View::Front), 10.0));
        let sent = seen.join().unwrap();
        assert!(sent[0].starts_with("POST /v1/images/generations") && sent[0].contains("\"quality\":\"low\""), "{}", sent[0]);
        assert!(sent[1].starts_with("POST /v1/images/edits") && sent[1].contains("filename=\"concept-1.png\""), "{}", &sent[1][..300]);
        assert!(sent[2].contains("filename=\"concept-2.png\"") && sent[2].contains("A-pose"), "the view is made from the approved concept");
        assert_eq!((a.concepts.len(), a.views.len(), a.cents()), (2, 1, 12.0));
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn views_ask_for_the_same_subject_and_a_character_in_an_a_pose() {
        let front = View::Front.prompt(true);
        assert!(front.contains("same subject") && front.contains("A-pose") && front.contains("from the front"));
        assert!(!View::Side.prompt(false).contains("A-pose"));
    }
}
