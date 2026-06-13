use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

const SKILL_FILE: &str = "SKILL.md";
const MAX_SKILL_TEXT_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing)]
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SkillDocument {
    pub summary: SkillSummary,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct SkillResource {
    pub skill_id: String,
    pub path: String,
    pub content: String,
}

pub fn list_skills(workspace_root: &Path) -> Result<Vec<SkillSummary>, String> {
    let mut seen = HashSet::new();
    let mut skills = Vec::new();
    for root in skill_roots(workspace_root) {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join(SKILL_FILE).is_file() {
                continue;
            }
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            skills.push(load_skill_summary(id, path)?);
        }
    }
    skills.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(skills)
}

pub fn search_skills(workspace_root: &Path, query: &str) -> Result<Vec<SkillSummary>, String> {
    let query = query.trim();
    if query.is_empty() {
        return list_skills(workspace_root);
    }
    let query_lower = query.to_ascii_lowercase();
    let mut scored = Vec::new();
    for summary in list_skills(workspace_root)? {
        let content = read_limited_text(&summary.root.join(SKILL_FILE))?.to_ascii_lowercase();
        let mut haystack = format!("{}\n{}\n{}", summary.id, summary.name, summary.description)
            .to_ascii_lowercase();
        haystack.push('\n');
        haystack.push_str(&content);
        if !haystack.contains(&query_lower) {
            continue;
        }
        let score = skill_match_score(&summary, &content, &query_lower);
        scored.push((score, summary));
    }
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(scored.into_iter().map(|(_, summary)| summary).collect())
}

pub fn read_skill(workspace_root: &Path, skill_id: &str) -> Result<SkillDocument, String> {
    let summary = find_skill(workspace_root, skill_id)?;
    let content = read_limited_text(&summary.root.join(SKILL_FILE))?;
    Ok(SkillDocument { summary, content })
}

pub fn read_skill_resource(
    workspace_root: &Path,
    skill_id: &str,
    resource_path: &str,
) -> Result<SkillResource, String> {
    let summary = find_skill(workspace_root, skill_id)?;
    let safe_path = safe_relative_path(resource_path)?;
    if safe_path == PathBuf::from(SKILL_FILE) {
        return Err(
            "read_skill_resource is for resource files; use read_skill for SKILL.md".to_string(),
        );
    }
    let path = summary.root.join(&safe_path);
    let canonical_root = summary
        .root
        .canonicalize()
        .map_err(|error| format!("failed to resolve skill root: {error}"))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("skill resource not found: {error}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("skill resource path escapes the skill directory".to_string());
    }
    let content = read_limited_text(&canonical_path)?;
    Ok(SkillResource {
        skill_id: summary.id,
        path: safe_path.to_string_lossy().replace('\\', "/"),
        content,
    })
}

pub fn skill_summaries_payload(skills: &[SkillSummary]) -> Value {
    json!({
        "skillCount": skills.len(),
        "skills": skills.iter().map(skill_summary_payload).collect::<Vec<_>>()
    })
}

pub fn skill_summary_payload(summary: &SkillSummary) -> Value {
    json!({
        "id": summary.id,
        "name": summary.name,
        "description": summary.description,
    })
}

pub fn skill_roots(workspace_root: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(value) = std::env::var("CATDESK_SKILLS_DIR") {
        for part in std::env::split_paths(&value) {
            push_existing_dir(&mut roots, part);
        }
    }
    push_existing_dir(&mut roots, workspace_root.join(".catdesk").join("skills"));
    push_existing_dir(&mut roots, workspace_root.join("skills"));
    if let Some(home) = std::env::var_os("HOME") {
        push_existing_dir(
            &mut roots,
            PathBuf::from(home).join(".catdesk").join("skills"),
        );
    }
    push_existing_dir(&mut roots, PathBuf::from("/home/oai/skills"));
    roots
}

fn push_existing_dir(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_dir() && !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

fn find_skill(workspace_root: &Path, skill_id: &str) -> Result<SkillSummary, String> {
    list_skills(workspace_root)?
        .into_iter()
        .find(|skill| skill.id == skill_id)
        .ok_or_else(|| format!("skill not found: {skill_id}"))
}

fn load_skill_summary(id: String, root: PathBuf) -> Result<SkillSummary, String> {
    let content = read_limited_text(&root.join(SKILL_FILE))?;
    let frontmatter = parse_skill_frontmatter(&content)
        .ok_or_else(|| format!("skill {id} is missing required YAML frontmatter"))?;
    Ok(SkillSummary {
        id,
        name: frontmatter.name,
        description: frontmatter.description,
        root,
    })
}

fn read_limited_text(path: &Path) -> Result<String, String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("failed to read metadata for {}: {error}", path.display()))?;
    if metadata.len() > MAX_SKILL_TEXT_BYTES {
        return Err(format!(
            "skill file exceeds {} bytes: {}",
            MAX_SKILL_TEXT_BYTES,
            path.display()
        ));
    }
    std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

#[derive(Debug, PartialEq, Eq)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

fn parse_skill_frontmatter(content: &str) -> Option<SkillFrontmatter> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut fields = HashMap::<String, String>::new();
    let mut multiline_key: Option<String> = None;
    let mut multiline_value = String::new();
    for line in lines {
        let trimmed_end = line.trim_end();
        if trimmed_end == "---" {
            flush_multiline_field(&mut fields, &mut multiline_key, &mut multiline_value);
            let name = fields.remove("name")?.trim().to_string();
            let description = fields.remove("description")?.trim().to_string();
            if name.is_empty() || description.is_empty() {
                return None;
            }
            return Some(SkillFrontmatter { name, description });
        }
        if let Some(key) = multiline_key.as_ref() {
            if starts_unclosed_quote(&multiline_value)
                || line.starts_with(' ')
                || line.starts_with('\t')
                || trimmed_end.is_empty()
            {
                if !multiline_value.is_empty() {
                    multiline_value.push('\n');
                }
                multiline_value.push_str(trimmed_end.trim());
                continue;
            }
            let key = key.clone();
            fields.insert(key, unquote_yaml_scalar(multiline_value.trim()));
            multiline_key = None;
            multiline_value.clear();
        }
        let Some((key, value)) = trimmed_end.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        if value == "|" || value == ">" {
            multiline_key = Some(key);
            multiline_value.clear();
        } else if starts_unclosed_quote(value) {
            multiline_key = Some(key);
            multiline_value.push_str(value);
        } else {
            fields.insert(key, unquote_yaml_scalar(value));
        }
    }
    None
}

fn flush_multiline_field(
    fields: &mut HashMap<String, String>,
    multiline_key: &mut Option<String>,
    multiline_value: &mut String,
) {
    if let Some(key) = multiline_key.take() {
        fields.insert(key, unquote_yaml_scalar(multiline_value.trim()));
        multiline_value.clear();
    }
}

fn starts_unclosed_quote(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() < 2 {
        return false;
    }
    let first = trimmed.as_bytes()[0];
    if first != b'\'' && first != b'"' {
        return false;
    }
    trimmed.as_bytes()[trimmed.len() - 1] != first
}

fn unquote_yaml_scalar(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[trimmed.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[trimmed.len() - 1] == b'\'')
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn skill_match_score(summary: &SkillSummary, content_lower: &str, query_lower: &str) -> i32 {
    let mut score = 0;
    if summary.id.eq_ignore_ascii_case(query_lower) {
        score += 100;
    }
    if summary.name.to_ascii_lowercase().contains(query_lower) {
        score += 40;
    }
    if summary
        .description
        .to_ascii_lowercase()
        .contains(query_lower)
    {
        score += 25;
    }
    if content_lower.contains(query_lower) {
        score += 5;
    }
    score
}

fn safe_relative_path(resource_path: &str) -> Result<PathBuf, String> {
    let trimmed = resource_path.trim();
    if trimmed.is_empty() {
        return Err("resource path is required".to_string());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err("resource path must be relative to the skill directory".to_string());
    }
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => output.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("resource path cannot contain parent directory traversal".to_string());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("resource path must be relative to the skill directory".to_string());
            }
        }
    }
    if output.as_os_str().is_empty() {
        return Err("resource path is required".to_string());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_workspace() -> PathBuf {
        let path = std::env::temp_dir().join(format!("catdesk-skills-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp workspace");
        path
    }

    fn write_skill(workspace: &Path, id: &str, skill_md: &str) {
        let root = workspace.join(".catdesk").join("skills").join(id);
        std::fs::create_dir_all(&root).expect("create skill root");
        std::fs::write(root.join(SKILL_FILE), skill_md).expect("write skill file");
    }

    fn skill_doc(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n")
    }

    #[test]
    fn parse_skill_frontmatter_reads_required_name_and_description() {
        let parsed = parse_skill_frontmatter(
            "---\nname: Docs\ndescription: Use when drafting documents.\n---\n# Body\n",
        )
        .expect("parse frontmatter");
        assert_eq!(parsed.name, "Docs");
        assert_eq!(parsed.description, "Use when drafting documents.");
    }

    #[test]
    fn parse_skill_frontmatter_rejects_missing_description() {
        assert!(parse_skill_frontmatter("---\nname: Docs\n---\n").is_none());
    }

    #[test]
    fn parse_skill_frontmatter_reads_multiline_description() {
        let parsed = parse_skill_frontmatter(
            "---\nname: Docs\ndescription: |\n  Use when drafting documents.\n  Handles reports.\n---\n",
        )
        .expect("parse frontmatter");
        assert_eq!(
            parsed.description,
            "Use when drafting documents.\nHandles reports."
        );
    }

    #[test]
    fn parse_skill_frontmatter_reads_quoted_multiline_description() {
        let parsed = parse_skill_frontmatter(
            "---\nname: spreadsheets\ndescription: \"Create and edit spreadsheets.\nThis skill applies when workbook work is requested.\"\n---\n",
        )
        .expect("parse frontmatter");
        assert_eq!(
            parsed.description,
            "Create and edit spreadsheets.\nThis skill applies when workbook work is requested."
        );
    }

    #[test]
    fn list_skills_reads_workspace_skill_dirs() {
        let workspace = temp_workspace();
        write_skill(
            &workspace,
            "slides",
            &skill_doc(
                "Slides",
                "Create slide decks.",
                "Use this skill for presentations.",
            ),
        );
        let skills = list_skills(&workspace).expect("list skills");
        assert!(skills.iter().any(|skill| skill.id == "slides"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn search_skills_matches_description_and_body() {
        let workspace = temp_workspace();
        write_skill(
            &workspace,
            "docs",
            &skill_doc(
                "Documents",
                "Create polished reports.",
                "Use for writing reports.",
            ),
        );
        let skills = search_skills(&workspace, "reports").expect("search skills");
        assert_eq!(skills.first().map(|skill| skill.id.as_str()), Some("docs"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn read_skill_returns_full_skill_markdown() {
        let workspace = temp_workspace();
        write_skill(
            &workspace,
            "pdf",
            &skill_doc("PDFs", "Render and inspect PDFs.", "Use this for PDFs."),
        );
        let skill = read_skill(&workspace, "pdf").expect("read skill");
        assert_eq!(skill.summary.name, "PDFs");
        assert_eq!(skill.summary.description, "Render and inspect PDFs.");
        assert!(skill.content.contains("Use this for PDFs."));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn read_skill_resource_rejects_parent_traversal() {
        let workspace = temp_workspace();
        write_skill(
            &workspace,
            "pdf",
            &skill_doc("PDFs", "Render PDFs.", "Use this for PDFs."),
        );
        let error = read_skill_resource(&workspace, "pdf", "../secret.txt")
            .expect_err("traversal should fail");
        assert!(error.contains("traversal"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn read_skill_resource_reads_text_resource() {
        let workspace = temp_workspace();
        write_skill(
            &workspace,
            "pdf",
            &skill_doc("PDFs", "Render PDFs.", "Use this for PDFs."),
        );
        let root = workspace.join(".catdesk").join("skills").join("pdf");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::write(root.join("templates/basic.txt"), "template body").expect("write template");
        let resource =
            read_skill_resource(&workspace, "pdf", "templates/basic.txt").expect("read resource");
        assert_eq!(resource.content, "template body");
        let _ = std::fs::remove_dir_all(workspace);
    }
}
