use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

const DEFAULT_PREAMBLE: &str = "You are an expert coding assistant. You help users by reading files, executing commands, editing code, and writing new files.

<tools>
- read: Read a file as anchored rows
- replace, replace_match, insert: Edit a file by the anchors read returned
- write: Create a new file
- bash: Run shell commands
- rg: Search file contents
- fd: Find files by name
</tools>

<rules>
- Read a file before editing it, and edit it by anchor instead of rewriting it
- Use rg and fd, not grep and find through bash
- Be concise in your responses
- Show file paths clearly when working with files
</rules>";
const CONTEXT_FILES: [&str; 5] = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// Preamble, addendum, project context, skills, cwd. ponytail and caveman come from `modes`.
/// The project's `.agents/` wins over the agent dir (`~/.agents`).
pub fn system_prompt(agent_dir: &Path, cwd: &Path, disabled_skills: &[String]) -> String {
    let mut sections = vec![override_file(agent_dir, cwd, "SYSTEM.md").unwrap_or_else(|| DEFAULT_PREAMBLE.into())];
    if let Some(addendum) = override_file(agent_dir, cwd, "APPEND_SYSTEM.md") {
        sections.push(tag("addendum", &addendum));
    }
    let context = project_context(agent_dir, cwd);
    if !context.is_empty() {
        sections.push(tag("project_context", &context));
    }
    // The host injects ponytail and caveman natively, so their skills would only duplicate the rules.
    let skills: Vec<Skill> = load_skills(agent_dir, cwd, disabled_skills).into_iter().filter(|s| s.model_invocable && s.name != "ponytail" && s.name != "caveman").collect();
    if !skills.is_empty() {
        sections.push(tag("skills", &format_skills(skills)));
    }
    sections.push(tag("cwd", &cwd.display().to_string()));
    sections.retain(|s| !s.is_empty());
    sections.join("\n\n")
}

fn tag(name: &str, content: &str) -> String {
    format!("<{name}>\n{content}\n</{name}>")
}

pub fn read_text(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.strip_prefix('\u{feff}').map(String::from).unwrap_or(text))
}

fn override_file(agent_dir: &Path, cwd: &Path, name: &str) -> Option<String> {
    [cwd.join(".agents"), agent_dir.to_path_buf()].iter().find_map(|dir| read_text(&dir.join(name)))
}

fn context_file(dir: &Path) -> Option<(PathBuf, String)> {
    CONTEXT_FILES.iter().map(|name| dir.join(name)).filter(|path| path.is_file()).find_map(|path| read_text(&path).map(|text| (path, text)))
}

fn instructions_block(path: &Path, content: &str) -> String {
    format!("<project_instructions path=\"{}\">\n{content}\n</project_instructions>", path.display())
}

/// AGENTS.md files in directories below `cwd` on the way to `file`, skipping those in `loaded`.
/// The agents.md convention: the file closest to the code being touched applies to it.
pub fn nested_context(cwd: &Path, file: &Path, loaded: &mut HashSet<PathBuf>) -> String {
    let Some(relative) = file.parent().and_then(|dir| dir.strip_prefix(cwd).ok()) else { return String::new() };
    let mut dir = cwd.to_path_buf();
    let mut blocks = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else { return String::new() };
        dir.push(name);
        if let Some((path, content)) = context_file(&dir)
            && loaded.insert(path.clone())
        {
            blocks.push(instructions_block(&path, &content));
        }
    }
    blocks.join("\n\n")
}

fn project_context(agent_dir: &Path, cwd: &Path) -> String {
    let mut files: Vec<(PathBuf, String)> = context_file(agent_dir).into_iter().collect();
    let mut ancestors: Vec<(PathBuf, String)> = cwd.ancestors().filter_map(context_file).collect();
    ancestors.reverse();
    for file in ancestors {
        if !files.iter().any(|(path, _)| *path == file.0) {
            files.push(file);
        }
    }
    if files.is_empty() {
        return String::new();
    }
    let mut parts = vec!["Project-specific instructions and guidelines:".to_string()];
    for (path, content) in files {
        parts.push(instructions_block(&path, &content));
    }
    parts.join("\n\n")
}

pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// False for `disable-model-invocation: true`: listed for `/skill:name`, left out of the system prompt.
    pub model_invocable: bool,
}

/// Skills from `~/.agents/skills` and `.agents/skills` in cwd and its parents up to the git root.
pub fn load_skills(agent_dir: &Path, cwd: &Path, disabled: &[String]) -> Vec<Skill> {
    let mut dirs = vec![agent_dir.join("skills")];
    for dir in cwd.ancestors() {
        dirs.push(dir.join(".agents/skills"));
        if dir.join(".git").exists() {
            break;
        }
    }
    let mut files = Vec::new();
    for dir in &dirs {
        collect_skill_files(dir, true, &mut files);
    }
    let mut skills: Vec<Skill> = Vec::new();
    let mut seen = HashSet::new();
    for path in files {
        if !seen.insert(canonical(&path)) {
            continue;
        }
        let Some(skill) = parse_skill(&path) else { continue };
        if !disabled.contains(&skill.name) && !skills.iter().any(|s| s.name == skill.name) {
            skills.push(skill);
        }
    }
    skills
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn collect_skill_files(dir: &Path, root: bool, out: &mut Vec<PathBuf>) {
    let skill = dir.join("SKILL.md");
    if skill.is_file() {
        out.push(skill);
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_skill_files(&path, false, out);
        } else if root && name.ends_with(".md") {
            out.push(path);
        }
    }
}

fn parse_skill(path: &Path) -> Option<Skill> {
    let text = read_text(path)?;
    let fields = frontmatter(&text)?;
    let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let model_invocable = field("disable-model-invocation").as_deref() != Some("true");
    let description = field("description").filter(|d| !d.trim().is_empty())?;
    let dir_name = path.parent()?.file_name()?.to_string_lossy().into_owned();
    let name = field("name").filter(|n| !n.is_empty()).unwrap_or(dir_name);
    Some(Skill { name, description, path: path.to_path_buf(), model_invocable })
}

/// pi's `/skill:name args` expansion: the skill body wrapped in a `<skill>` block, then the args.
pub fn expand_skill(text: &str, skills: &[Skill]) -> Result<String, String> {
    let Some(rest) = text.strip_prefix("/skill:") else { return Ok(text.to_string()) };
    let (name, args) = rest.split_once(' ').map_or((rest, ""), |(name, args)| (name, args.trim()));
    let Some(skill) = skills.iter().find(|s| s.name == name) else { return Ok(text.to_string()) };
    let content = read_text(&skill.path).ok_or(format!("Cannot read {}", skill.path.display()))?;
    let body = without_frontmatter(&content);
    let base = skill.path.parent().unwrap_or(&skill.path).display();
    let block = format!("<skill name=\"{}\" location=\"{}\">\nReferences are relative to {base}.\n\n{body}\n</skill>", skill.name, skill.path.display());
    Ok(if args.is_empty() { block } else { format!("{block}\n\n{args}") })
}

/// A prompt template, run as `/name args` like pi's.
pub struct Template {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// `*.md` in `~/.agents/prompts`, then in the project's `.agents/prompts`, which replace global ones by name.
pub fn load_templates(agent_dir: &Path, cwd: &Path) -> Vec<Template> {
    let mut templates: Vec<Template> = Vec::new();
    for dir in [agent_dir.join("prompts"), cwd.join(".agents/prompts")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|ext| ext == "md")).collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()).map(str::to_string) else { continue };
            let fields = read_text(&path).and_then(|text| frontmatter(&text)).unwrap_or_default();
            let description = fields.into_iter().find(|(key, _)| key == "description").map(|(_, value)| value).unwrap_or_default();
            templates.retain(|t| t.name != name);
            templates.push(Template { name, description, path });
        }
    }
    templates
}

/// The template body with its placeholders filled from `args`, split like a shell line.
pub fn expand_template(template: &Template, args: &str) -> Result<String, String> {
    let content = read_text(&template.path).ok_or(format!("Cannot read {}", template.path.display()))?;
    Ok(substitute_args(without_frontmatter(&content), &split_args(args)))
}

/// Splits on whitespace; `"..."` and `'...'` keep their spaces and lose the quotes, like pi.
fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for c in text.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            None => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// pi's placeholders: `$1`, `$@` and `$ARGUMENTS`, `${1:-default}`, `${@:-default}`, `${@:2}` and `${@:2:3}`.
/// Values are not substituted again, and a `$` that starts no placeholder stays as is.
fn substitute_args(content: &str, args: &[String]) -> String {
    let all = args.join(" ");
    let mut out = String::new();
    let mut rest = content;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        match placeholder(rest, args, &all) {
            Some((value, consumed)) => {
                out.push_str(&value);
                rest = &rest[consumed..];
            }
            None => out.push('$'),
        }
    }
    out.push_str(rest);
    out
}

/// The value of the placeholder at the start of `after` (the text past a `$`) and how many bytes it spans.
fn placeholder(after: &str, args: &[String], all: &str) -> Option<(String, usize)> {
    let is_number = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let number = |text: &str| text.parse::<usize>().unwrap_or(usize::MAX);
    let positional = |n: usize| n.checked_sub(1).and_then(|i| args.get(i)).cloned().unwrap_or_default();
    if let Some(braced) = after.strip_prefix('{') {
        let (inner, _) = braced.split_once('}')?;
        let consumed = inner.len() + 2;
        if let Some((target, default)) = inner.split_once(":-") {
            let value = match target {
                "@" | "ARGUMENTS" => all.to_string(),
                n if is_number(n) => positional(number(n)),
                _ => return None,
            };
            return Some((if value.is_empty() { default.to_string() } else { value }, consumed));
        }
        let slice = inner.strip_prefix("@:")?;
        let (start, length) = slice.split_once(':').map_or((slice, None), |(start, length)| (start, Some(length)));
        if !is_number(start) || length.is_some_and(|l| !is_number(l)) {
            return None;
        }
        let picked = args.iter().skip(number(start).saturating_sub(1)).take(length.map_or(usize::MAX, number));
        return Some((picked.map(String::as_str).collect::<Vec<_>>().join(" "), consumed));
    }
    if after.starts_with("ARGUMENTS") {
        return Some((all.to_string(), "ARGUMENTS".len()));
    }
    if after.starts_with('@') {
        return Some((all.to_string(), 1));
    }
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| (positional(number(&after[..digits])), digits))
}

fn without_frontmatter(content: &str) -> &str {
    content.strip_prefix("---").and_then(|rest| rest.split_once("\n---")).map_or(content, |(_, body)| body).trim()
}

/// Top-level `key: value` pairs of a YAML frontmatter; enough YAML for skill files.
fn frontmatter(text: &str) -> Option<Vec<(String, String)>> {
    let (first, body) = text.split_once('\n')?;
    if first.trim_end() != "---" {
        return None;
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim_end() == "---" {
            return Some(fields);
        }
        if line.starts_with([' ', '\t']) || line.is_empty() {
            if let Some((_, value)) = fields.last_mut() {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        let value = if value.starts_with(['>', '|']) { "" } else { value };
        let value = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            Some(quoted) => quoted.replace("\\\"", "\"").replace("\\\\", "\\"),
            None => value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).map_or(value.to_string(), |quoted| quoted.replace("''", "'")),
        };
        fields.push((key.trim().to_string(), value));
    }
    None
}

/// pi-cache-optimizer's compressed skill list: one header per skills root, one line per skill.
fn format_skills(skills: Vec<Skill>) -> String {
    let mut by_root: Vec<(PathBuf, Vec<(String, String)>)> = Vec::new();
    let mut explicit: Vec<(PathBuf, String, String)> = Vec::new();
    for skill in skills {
        let description = skill.description.split_whitespace().collect::<Vec<_>>().join(" ");
        let dir = skill.path.parent().unwrap();
        let conventional = skill.path.file_name().is_some_and(|f| f == "SKILL.md") && dir.file_name().is_some_and(|d| *d == *skill.name);
        if !conventional {
            explicit.push((skill.path, skill.name, description));
            continue;
        }
        let root = dir.parent().unwrap().to_path_buf();
        match by_root.iter_mut().find(|(r, _)| *r == root) {
            Some((_, entries)) => entries.push((skill.name, description)),
            None => by_root.push((root, vec![(skill.name, description)])),
        }
    }
    by_root.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
    ];
    for (root, mut entries) in by_root {
        let root = root.display();
        lines.extend([String::new(), format!("## Skills in {root}/"), format!("Each skill file is at {root}/<name>/SKILL.md"), String::new()]);
        entries.sort();
        for (name, description) in entries {
            lines.push(format!("- {name}: {description}"));
        }
    }
    if !explicit.is_empty() {
        lines.extend([String::new(), "## Skills with explicit file paths".to_string(), String::new()]);
        explicit.sort();
        for (path, name, description) in explicit {
            lines.push(format!("- {name} (file: {}): {description}", path.display()));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_reads_plain_quoted_and_folded_values() {
        let text = "--- \nname: \"say \\\"hi\\\"\"\ndescription: >\n  first line\n  second line\nlicense: MIT\n---\nbody";
        let fields = frontmatter(text).unwrap();
        assert_eq!(fields[0], ("name".into(), "say \"hi\"".into()));
        assert_eq!(fields[1], ("description".into(), "first line second line".into()));
        assert_eq!(fields[2], ("license".into(), "MIT".into()));
    }

    #[test]
    fn nested_agents_md_is_attached_once_and_only_below_cwd() {
        let cwd = std::env::temp_dir().join(format!("agent-nested-{}", std::process::id()));
        std::fs::create_dir_all(cwd.join("sub/deep")).unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "root").unwrap();
        std::fs::write(cwd.join("sub/AGENTS.md"), "sub rules").unwrap();
        let mut loaded = HashSet::new();
        let file = cwd.join("sub/deep/x.rs");
        let first = nested_context(&cwd, &file, &mut loaded);
        let second = nested_context(&cwd, &file, &mut loaded);
        std::fs::remove_dir_all(&cwd).unwrap();
        assert_eq!(first, format!("<project_instructions path=\"{}\">\nsub rules\n</project_instructions>", cwd.join("sub/AGENTS.md").display()));
        assert_eq!(second, "");
        assert_eq!(nested_context(&cwd, &cwd.join("x.rs"), &mut loaded), "");
    }

    #[test]
    fn project_template_replaces_a_global_one_and_takes_the_arguments() {
        let root = std::env::temp_dir().join(format!("agent-templates-{}", std::process::id()));
        std::fs::create_dir_all(root.join("home/prompts")).unwrap();
        std::fs::create_dir_all(root.join("repo/.agents/prompts")).unwrap();
        std::fs::write(root.join("home/prompts/review.md"), "global").unwrap();
        std::fs::write(root.join("home/prompts/commit.md"), "---\ndescription: Commit\n---\nCommit. Context: $ARGUMENTS").unwrap();
        std::fs::write(root.join("repo/.agents/prompts/review.md"), "Review $ARGUMENTS").unwrap();
        let templates = load_templates(&root.join("home"), &root.join("repo"));
        let expanded: Vec<(String, String, String)> = templates.iter().map(|t| (t.name.clone(), t.description.clone(), expand_template(t, "HEAD~2").unwrap())).collect();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            expanded,
            [("commit".into(), "Commit".into(), "Commit. Context: HEAD~2".into()), ("review".into(), String::new(), "Review HEAD~2".into())]
        );
    }

    #[test]
    fn template_placeholders_follow_pi() {
        let args = split_args(r#"src "two words" 'x y' last"#);
        assert_eq!(args, ["src", "two words", "x y", "last"]);
        let cases = [
            ("$1 and $2", "src and two words"),
            ("$@ | $ARGUMENTS", "src two words x y last | src two words x y last"),
            ("${2:-none} ${9:-none} ${@:-none}", "two words none src two words x y last"),
            ("${@:3} / ${@:2:2} / ${@:0:1}", "x y last / two words x y / src"),
            ("$9|$0|$ alone|${1}|$x", "||$ alone|${1}|$x"),
        ];
        for (template, expected) in cases {
            assert_eq!(substitute_args(template, &args), expected, "{template}");
        }
        assert_eq!(substitute_args("${@:-default}", &[]), "default");
        assert_eq!(substitute_args("$1", &["$2".into(), "no".into()]), "$2");
    }
}
