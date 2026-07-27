#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncScope {
    All,
    Todo,
    Grocery,
    Habit,
    ScribbleBox,
    ScribbleKeep,
    ScribbleNote,
}

impl SyncScope {
    pub fn includes(&self, target: SyncScope) -> bool {
        match self {
            SyncScope::All => true,
            scope => scope == &target,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_scope_includes() {
        assert!(SyncScope::All.includes(SyncScope::Todo));
        assert!(SyncScope::All.includes(SyncScope::Grocery));
        assert!(SyncScope::Todo.includes(SyncScope::Todo));
        assert!(!SyncScope::Todo.includes(SyncScope::Grocery));
    }
}
