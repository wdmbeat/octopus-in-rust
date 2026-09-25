use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn discover(&mut self, dirs: &[PathBuf]) {
        for dir in dirs {
            if dir.is_dir() {
                self.discover_dir(dir);
            }
        }
    }

    fn discover_dir(&mut self, dir: &Path) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Subdirectory form: <name>/SKILL.md
                    let skill_md = path.join("SKILL.md");
                    if skill_md.is_file()
                        && let Some(skill) = Self::parse_skill_md(&skill_md)
                    {
                        self.skills.insert(skill.name.clone(), skill);
                    }
                } else if path.extension() == Some("md".as_ref()) {
                    // Flat form: <name>.md
                    if let Some(skill) = Self::parse_skill_md(&path) {
                        self.skills.insert(skill.name.clone(), skill);
                    }
                }
            }
        }
    }

    fn parse_skill_md(path: &Path) -> Option<Skill> {
        let content = std::fs::read_to_string(path).ok()?;
        let mut name = None;
        let mut description = None;

        // Parse YAML frontmatter
        if content.starts_with("---")
            && let Some(end) = content.find("\n---\n")
        {
            let frontmatter = &content[3..end];
            for line in frontmatter.lines() {
                if let Some((key, value)) = line.split_once(':') {
                    let key = key.trim();
                    let value = value.trim();
                    match key {
                        "name" => name = Some(value.to_string()),
                        "description" => description = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
        }

        let name = name.unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed".to_string())
        });
        let description = description.unwrap_or_default();

        Some(Skill {
            name,
            description,
            path: path.to_path_buf(),
        })
    }

    pub fn format_for_system_prompt(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let mut lines = vec!["Available skills:".to_string()];
        for skill in self.skills.values() {
            lines.push(format!("- {}: {}", skill.name, skill.description));
        }
        lines.join("\n")
    }
}
