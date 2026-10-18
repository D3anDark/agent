use serde_json::{Value, json};

/// rpiv-todo's task list: the model edits it with the `todo` tool, the user with /todos.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Todos {
    pub tasks: Vec<Task>,
    next_id: u64,
    /// Changes the user made in /todos, told to the model before the next prompt.
    pub user_notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: u64,
    pub subject: String,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub status: Status,
    pub blocked_by: Vec<u64>,
    pub owner: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

impl Status {
    fn parse(text: &str) -> Option<Status> {
        match text {
            "pending" => Some(Status::Pending),
            "in_progress" => Some(Status::InProgress),
            "completed" => Some(Status::Completed),
            "deleted" => Some(Status::Deleted),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::InProgress => "in_progress",
            Status::Completed => "completed",
            Status::Deleted => "deleted",
        }
    }

    /// rpiv-todo's invariants: completed only goes to deleted, deleted is terminal.
    fn can_become(self, to: Status) -> bool {
        self == to
            || match self {
                Status::Pending | Status::InProgress => true,
                Status::Completed => to == Status::Deleted,
                Status::Deleted => false,
            }
    }
}

impl Todos {
    /// Runs one `todo` tool call; the bool is true when the list changed and must be saved.
    pub fn apply(&mut self, input: &Value) -> Result<(String, bool), String> {
        let id = input["id"].as_u64();
        let status = match input["status"].as_str() {
            Some(text) => Some(Status::parse(text).ok_or(format!("Error: unknown status {text}"))?),
            None => None,
        };
        let text = |key: &str| input[key].as_str().map(String::from);
        let ids = |key: &str| -> Vec<u64> { input[key].as_array().into_iter().flatten().filter_map(Value::as_u64).collect() };
        match input["action"].as_str().unwrap_or_default() {
            "create" => {
                let subject = text("subject").filter(|s| !s.trim().is_empty()).ok_or("Error: subject required for create")?;
                let blocked_by = ids("blockedBy");
                for dep in &blocked_by {
                    self.live(*dep, "blockedBy")?;
                }
                let id = self.next_id.max(1);
                self.next_id = id + 1;
                self.tasks.push(Task {
                    id,
                    subject: subject.clone(),
                    description: text("description").filter(|s| !s.is_empty()),
                    active_form: text("activeForm").filter(|s| !s.is_empty()),
                    status: Status::Pending,
                    blocked_by,
                    owner: text("owner").filter(|s| !s.is_empty()),
                    metadata: input.get("metadata").filter(|m| m.is_object()).cloned(),
                });
                Ok((format!("Created #{id}: {subject} (pending)"), true))
            }
            "update" => {
                let id = id.ok_or("Error: id required for update")?;
                let index = self.index_of(id)?;
                let (add, remove) = (ids("addBlockedBy"), ids("removeBlockedBy"));
                let fields = ["subject", "description", "activeForm", "status", "owner", "metadata"];
                if fields.iter().all(|key| input.get(key).is_none_or(Value::is_null)) && add.is_empty() && remove.is_empty() {
                    return Err("Error: update requires at least one mutable field: subject, description, activeForm, status, owner, metadata, addBlockedBy, or removeBlockedBy".into());
                }
                let before = self.tasks[index].clone();
                let mut task = before.clone();
                if let Some(to) = status {
                    if !before.status.can_become(to) {
                        return Err(format!("Error: illegal transition {} → {}", before.status.as_str(), to.as_str()));
                    }
                    task.status = to;
                }
                task.blocked_by.retain(|dep| !remove.contains(dep));
                for dep in add {
                    if dep == id {
                        return Err(format!("Error: cannot block #{id} on itself"));
                    }
                    self.live(dep, "addBlockedBy")?;
                    if !task.blocked_by.contains(&dep) {
                        task.blocked_by.push(dep);
                    }
                }
                if self.reaches(&task.blocked_by, id) {
                    return Err("Error: addBlockedBy would create a cycle in the blockedBy graph".into());
                }
                task.subject = text("subject").unwrap_or(task.subject);
                task.description = text("description").or(task.description);
                task.active_form = text("activeForm").or(task.active_form);
                task.owner = text("owner").or(task.owner);
                if let Some(changes) = input["metadata"].as_object() {
                    let mut merged = task.metadata.take().unwrap_or_else(|| json!({}));
                    for (key, value) in changes {
                        if value.is_null() {
                            merged.as_object_mut().unwrap().remove(key);
                        } else {
                            merged[key] = value.clone();
                        }
                    }
                    task.metadata = merged.as_object().is_some_and(|m| !m.is_empty()).then_some(merged);
                }
                if task == before {
                    return Ok((format!("No change: #{id} already matches the requested values (status: {})", task.status.as_str()), false));
                }
                let transition = if before.status != task.status { format!(" ({} → {})", before.status.as_str(), task.status.as_str()) } else { String::new() };
                self.tasks[index] = task;
                Ok((format!("Updated #{id}{transition}"), true))
            }
            "list" => {
                let include_deleted = input["includeDeleted"].as_bool() == Some(true);
                let lines: Vec<String> = self.tasks.iter().filter(|t| include_deleted || t.status != Status::Deleted).filter(|t| status.is_none_or(|s| t.status == s)).map(list_line).collect();
                Ok((if lines.is_empty() { "No tasks".into() } else { lines.join("\n") }, false))
            }
            "get" => {
                let task = &self.tasks[self.index_of(id.ok_or("Error: id required for get")?)?];
                Ok((self.details(task), false))
            }
            "delete" => {
                let index = self.index_of(id.ok_or("Error: id required for delete")?)?;
                let task = &mut self.tasks[index];
                if task.status == Status::Deleted {
                    return Err(format!("Error: #{} is already deleted", task.id));
                }
                task.status = Status::Deleted;
                Ok((format!("Deleted #{}: {}", task.id, task.subject), true))
            }
            "clear" => {
                let count = self.tasks.len();
                self.tasks.clear();
                self.next_id = 1;
                Ok((format!("Cleared {count} tasks"), true))
            }
            other => Err(format!("Error: unknown action {other}")),
        }
    }

    /// The user's /todos edit: cycles pending → in progress → completed → pending, ignoring the model's transition rules.
    pub fn cycle(&mut self, index: usize) {
        let task = &mut self.tasks[index];
        task.status = match task.status {
            Status::Pending => Status::InProgress,
            Status::InProgress => Status::Completed,
            Status::Completed | Status::Deleted => Status::Pending,
        };
        self.user_notes.push(format!("The user set todo #{} \"{}\" to {}.", task.id, task.subject, task.status.as_str()));
    }

    pub fn delete(&mut self, index: usize) {
        let task = &mut self.tasks[index];
        task.status = Status::Deleted;
        self.user_notes.push(format!("The user deleted todo #{} \"{}\".", task.id, task.subject));
    }

    /// Tasks not deleted, in creation order.
    pub fn visible(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|t| t.status != Status::Deleted)
    }

    /// The widget shows while some visible task is still open.
    pub fn has_open(&self) -> bool {
        self.visible().any(|t| t.status != Status::Completed)
    }

    pub fn entry(&self) -> Value {
        let tasks: Vec<Value> = self
            .tasks
            .iter()
            .map(|t| {
                json!({"id": t.id, "subject": t.subject, "description": t.description, "activeForm": t.active_form, "status": t.status.as_str(),
                       "blockedBy": t.blocked_by, "owner": t.owner, "metadata": t.metadata})
            })
            .collect();
        json!({"type": "todo", "tasks": tasks, "next_id": self.next_id})
    }

    pub fn from_entry(entry: &Value) -> Todos {
        let text = |task: &Value, key: &str| task[key].as_str().map(String::from);
        let tasks = entry["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| Task {
                id: t["id"].as_u64().unwrap_or_default(),
                subject: text(t, "subject").unwrap_or_default(),
                description: text(t, "description"),
                active_form: text(t, "activeForm"),
                status: t["status"].as_str().and_then(Status::parse).unwrap_or(Status::Pending),
                blocked_by: t["blockedBy"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect(),
                owner: text(t, "owner"),
                metadata: t.get("metadata").filter(|m| m.is_object()).cloned(),
            })
            .collect();
        Todos { tasks, next_id: entry["next_id"].as_u64().unwrap_or(1), user_notes: Vec::new() }
    }

    fn index_of(&self, id: u64) -> Result<usize, String> {
        self.tasks.iter().position(|t| t.id == id).ok_or(format!("Error: #{id} not found"))
    }

    fn live(&self, id: u64, field: &str) -> Result<(), String> {
        match self.tasks.iter().find(|t| t.id == id) {
            None => Err(format!("Error: {field}: #{id} not found")),
            Some(t) if t.status == Status::Deleted => Err(format!("Error: {field}: #{id} is deleted")),
            Some(_) => Ok(()),
        }
    }

    /// True when following blockedBy edges from `start` leads back to `target`.
    fn reaches(&self, start: &[u64], target: u64) -> bool {
        let mut stack = start.to_vec();
        let mut seen = Vec::new();
        while let Some(id) = stack.pop() {
            if id == target {
                return true;
            }
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if let Some(task) = self.tasks.iter().find(|t| t.id == id) {
                stack.extend(&task.blocked_by);
            }
        }
        false
    }

    fn details(&self, task: &Task) -> String {
        let mut lines = vec![format!("#{} [{}] {}", task.id, task.status.as_str(), task.subject)];
        if let Some(description) = &task.description {
            lines.push(format!("  description: {description}"));
        }
        if let Some(form) = &task.active_form {
            lines.push(format!("  activeForm: {form}"));
        }
        if !task.blocked_by.is_empty() {
            lines.push(format!("  blockedBy: {}", refs(&task.blocked_by)));
        }
        let blocks: Vec<u64> = self.tasks.iter().filter(|t| t.blocked_by.contains(&task.id)).map(|t| t.id).collect();
        if !blocks.is_empty() {
            lines.push(format!("  blocks: {}", refs(&blocks)));
        }
        if let Some(owner) = &task.owner {
            lines.push(format!("  owner: {owner}"));
        }
        lines.join("\n")
    }
}

fn list_line(task: &Task) -> String {
    let form = match (&task.active_form, task.status) {
        (Some(form), Status::InProgress) => format!(" ({form})"),
        _ => String::new(),
    };
    let block = if task.blocked_by.is_empty() { String::new() } else { format!(" ⛓ {}", task.blocked_by.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(",")) };
    format!("[{}] #{} {}{form}{block}", task.status.as_str(), task.id, task.subject)
}

fn refs(ids: &[u64]) -> String {
    ids.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(todos: &mut Todos, input: Value) -> String {
        match todos.apply(&input) {
            Ok((text, _)) | Err(text) => text,
        }
    }

    #[test]
    fn transitions_follow_rpiv_todo_rules() {
        let mut todos = Todos::default();
        assert_eq!(run(&mut todos, json!({"action": "create", "subject": "a", "activeForm": "doing a"})), "Created #1: a (pending)");
        run(&mut todos, json!({"action": "create", "subject": "b", "blockedBy": [1]}));
        assert_eq!(run(&mut todos, json!({"action": "update", "id": 1, "status": "in_progress"})), "Updated #1 (pending → in_progress)");
        assert_eq!(run(&mut todos, json!({"action": "update", "id": 1, "status": "in_progress"})), "No change: #1 already matches the requested values (status: in_progress)");
        assert_eq!(run(&mut todos, json!({"action": "list"})), "[in_progress] #1 a (doing a)\n[pending] #2 b ⛓ #1");
        assert_eq!(run(&mut todos, json!({"action": "update", "id": 1, "addBlockedBy": [2]})), "Error: addBlockedBy would create a cycle in the blockedBy graph");
        run(&mut todos, json!({"action": "update", "id": 1, "status": "completed"}));
        assert_eq!(run(&mut todos, json!({"action": "update", "id": 1, "status": "pending"})), "Error: illegal transition completed → pending");
        assert_eq!(run(&mut todos, json!({"action": "delete", "id": 2})), "Deleted #2: b");
        assert_eq!(run(&mut todos, json!({"action": "list"})), "[completed] #1 a");
        assert!(!todos.has_open());
        assert_eq!(Todos::from_entry(&todos.entry()), todos);
    }
}
