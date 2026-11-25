//! The journal plugin's skills, served over MCP with the Skills extension
//! (`io.modelcontextprotocol/skills`), so a client that supports it (Claude
//! Code) loads them from `ganymede mcp` without the plugin.
//!
//! The files are the plugin's own (`plugins/ganymede-journal/skills`),
//! embedded at build time: one source of truth, and no folder to read at
//! run time. [`CATALOG`] lists them; a test fails when the plugin has a
//! file the catalog doesn't.

use rmcp::model::{Skill, SkillFrontmatter, SkillResource, SkillResources, SKILL_MD};
use std::collections::HashMap;
use std::sync::LazyLock;

/// URI scheme of a skill's files: `skill://<name>/<file>`.
pub(super) const SCHEME: &str = "skill://";

/// Each skill and its files, `SKILL.md` first.
const CATALOG: &[(&str, &[(&str, &str)])] = &[
    (
        "journal",
        &[
            (
                SKILL_MD,
                include_str!("../../../../plugins/ganymede-journal/skills/journal/SKILL.md"),
            ),
            (
                "python.md",
                include_str!("../../../../plugins/ganymede-journal/skills/journal/python.md"),
            ),
            (
                "typescript.md",
                include_str!("../../../../plugins/ganymede-journal/skills/journal/typescript.md"),
            ),
        ],
    ),
    (
        "page-style",
        &[
            (
                SKILL_MD,
                include_str!("../../../../plugins/ganymede-journal/skills/page-style/SKILL.md"),
            ),
            (
                "page.css",
                include_str!("../../../../plugins/ganymede-journal/skills/page-style/page.css"),
            ),
        ],
    ),
];

/// The skills as `skills/list` returns them, and each file by URI.
struct Built {
    skills: Vec<Skill>,
    files: HashMap<String, (&'static str, &'static str)>,
}

static BUILT: LazyLock<Built> = LazyLock::new(|| build(CATALOG).expect("the embedded skills"));

fn build(catalog: &[(&str, &[(&'static str, &'static str)])]) -> Result<Built, String> {
    let mut skills = Vec::new();
    let mut files = HashMap::new();
    for (name, entries) in catalog {
        let uri = |file: &str| format!("{SCHEME}{name}/{file}");
        let mut manifest = Vec::new();
        let mut frontmatter = None;
        for (file, text) in *entries {
            manifest.push(SkillResource::from_bytes(uri(file), text.as_bytes()));
            files.insert(uri(file), (mime(file), *text));
            if *file == SKILL_MD {
                frontmatter =
                    Some(parse_frontmatter(text).map_err(|e| format!("{name}/{file}: {e}"))?);
            }
        }
        let frontmatter = frontmatter.ok_or_else(|| format!("{name} has no {SKILL_MD}"))?;
        skills.push(Skill::new(
            uri(SKILL_MD),
            frontmatter,
            SkillResources::Files(manifest),
        ));
    }
    Ok(Built { skills, files })
}

/// Every skill, for `skills/list`.
pub(super) fn list() -> Vec<Skill> {
    BUILT.skills.clone()
}

/// The skill whose `SKILL.md` is at `uri`.
pub(super) fn get(uri: &str) -> Option<Skill> {
    BUILT.skills.iter().find(|s| s.uri == uri).cloned()
}

/// A file's media type and text, for `resources/read`.
pub(super) fn file(uri: &str) -> Option<(&'static str, &'static str)> {
    BUILT.files.get(uri).copied()
}

fn mime(file: &str) -> &'static str {
    match file.rsplit('.').next() {
        Some("css") => "text/css",
        _ => "text/markdown",
    }
}

/// The `SKILL.md` frontmatter: the lines between the `---` fences, each a
/// plain `key: value`, every value kept as a string. Anything else YAML
/// allows (quotes, block scalars, lists, continuation lines, comments)
/// is an error, not a guess, so a richer value can't be silently mangled.
fn parse_frontmatter(text: &str) -> Result<SkillFrontmatter, String> {
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Err("no frontmatter".into());
    }
    let (mut name, mut description) = (None, None);
    let mut extra = Vec::new();
    loop {
        let line = lines.next().ok_or("frontmatter is not closed by ---")?;
        if line == "---" {
            break;
        }
        let (key, value) = line
            .split_once(": ")
            .ok_or_else(|| format!("not a `key: value` line: {line:?}"))?;
        let key_ok = key.starts_with(|c: char| c.is_ascii_alphabetic())
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        // YAML reads these as a boolean, null or number, not the string
        // kept here.
        let not_a_string = matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "null" | "~"
        ) || value.parse::<f64>().is_ok()
            || value.starts_with(['+', '-', '.']) && value[1..].parse::<f64>().is_ok();
        let value_ok = !value.trim().is_empty()
            && !not_a_string
            && value == value.trim()
            && !value.starts_with([
                '"', '\'', '|', '>', '[', '{', '&', '*', '!', '#', '%', '@', '`',
            ])
            && !value.contains(": ")
            && !value.contains(" #");
        if !key_ok || !value_ok {
            return Err(format!("not a plain `key: value` line: {line:?}"));
        }
        match key {
            "name" => name = Some(value.to_string()),
            "description" => description = Some(value.to_string()),
            _ => extra.push((key, value)),
        }
    }
    let mut fm = SkillFrontmatter::new(
        name.ok_or("frontmatter has no name")?,
        description.ok_or("frontmatter has no description")?,
    );
    for (k, v) in extra {
        fm = fm.with_field(k, v);
    }
    Ok(fm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn both_skills_are_listed_with_their_frontmatter() {
        let skills = list();
        let names: Vec<_> = skills.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["journal", "page-style"]);
        for s in &skills {
            s.validate().unwrap();
            let (_, text) = file(&s.uri).unwrap();
            let fm = parse_frontmatter(text).unwrap();
            assert_eq!(s.frontmatter, fm);
            assert!(!fm.description.is_empty());
            // The skill's own name leads its URI.
            assert_eq!(s.uri, format!("skill://{}/SKILL.md", fm.name));
        }
        assert_eq!(skills[0].uri, "skill://journal/SKILL.md");
    }

    /// The manifest's digests and sizes are those of the bytes served, for
    /// every file, so a client's integrity check passes.
    #[test]
    fn the_manifest_matches_the_bytes_served() {
        for s in list() {
            let SkillResources::Files(files) = &s.resources else {
                panic!("a manifest");
            };
            assert!(files.len() >= 2 || s.name() != "journal");
            for f in files {
                let (_, text) = file(&f.uri).unwrap_or_else(|| panic!("{} is served", f.uri));
                f.verify(text.as_bytes()).unwrap();
            }
        }
        assert_eq!(
            file("skill://journal/python.md").unwrap().0,
            "text/markdown"
        );
        assert_eq!(file("skill://page-style/page.css").unwrap().0, "text/css");
    }

    #[test]
    fn an_unknown_skill_is_not_found() {
        assert!(get("skill://nope/SKILL.md").is_none());
        // A file of a skill is not a skill.
        assert!(get("skill://journal/python.md").is_none());
        assert!(file("skill://journal/nope.md").is_none());
        assert!(file("notebook://a.ipynb").is_none());
    }

    /// The plugin's skills folders and the catalog hold the same files:
    /// a file added to the plugin without the catalog would be missing
    /// from the skill served over MCP.
    #[test]
    fn every_plugin_skill_file_is_in_the_catalog() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/ganymede-journal/skills");
        let mut on_disk = Vec::new();
        for skill in std::fs::read_dir(&dir).unwrap() {
            let skill = skill.unwrap();
            assert!(skill.file_type().unwrap().is_dir(), "{:?}", skill.path());
            for f in std::fs::read_dir(skill.path()).unwrap() {
                let f = f.unwrap();
                assert!(f.file_type().unwrap().is_file(), "{:?}", f.path());
                let uri = format!(
                    "{SCHEME}{}/{}",
                    skill.file_name().to_string_lossy(),
                    f.file_name().to_string_lossy()
                );
                assert_eq!(
                    file(&uri).map(|(_, t)| t.as_bytes().to_vec()),
                    Some(std::fs::read(f.path()).unwrap()),
                    "{uri} is not in the catalog, or differs from the plugin's"
                );
                on_disk.push(uri);
            }
        }
        let mut served: Vec<_> = BUILT.files.keys().cloned().collect();
        served.sort();
        on_disk.sort();
        assert_eq!(served, on_disk);
    }

    /// Frontmatter is read as plain `key: value` lines; anything richer is
    /// refused, not mangled.
    #[test]
    fn frontmatter_is_plain_key_value_lines() {
        let fm =
            parse_frontmatter("---\nname: a\ndescription: Does \"x\" (y).\nowner: me\n---\nbody")
                .unwrap();
        assert_eq!(
            (fm.name.as_str(), fm.description.as_str()),
            ("a", "Does \"x\" (y).")
        );
        assert_eq!(fm.extra["owner"], "me");
        for bad in [
            "name: a\ndescription: d\n---\n",
            "---\nname: a\ndescription: >\n  folded\n---\n",
            "---\nname: a\ndescription: d\n  continued\n---\n",
            "---\nname: a\ndescription: \"quoted\"\n---\n",
            "---\nname: a\ndescription: d\ntags: [x, y]\n---\n",
            "---\nname: a\ndescription: has: colon\n---\n",
            "---\nname: a\ndescription: d # comment\n---\n",
            "---\nname: a\ndescription: d\nempty:\n---\n",
            "---\nname: a\ndescription: d\nempty: \n---\n",
            "---\nname: a\ndescription: d\nbad key: v\n---\n",
            "---\nname: a\ndescription: d \n---\n",
            "---\nname: a\ndescription: d\nflag: true\n---\n",
            "---\nname: a\ndescription: d\nflag: Off\n---\n",
            "---\nname: a\ndescription: d\nversion: 1.0\n---\n",
            "---\nname: a\ndescription: d\nn: null\n---\n",
            "---\nname: a\ndescription: d\n",
            "---\nname: a\n---\n",
        ] {
            assert!(parse_frontmatter(bad).is_err(), "{bad:?}");
        }
    }
}
