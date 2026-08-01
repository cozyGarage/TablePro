use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    ToolsRead,
    ToolsWrite,
    ResourcesRead,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenPermissions {
    ReadOnly,
    ReadWrite,
    FullAccess,
}

impl TokenPermissions {
    pub fn scopes(self) -> Vec<McpScope> {
        match self {
            Self::ReadOnly => vec![McpScope::ToolsRead, McpScope::ResourcesRead],
            Self::ReadWrite => vec![McpScope::ToolsRead, McpScope::ToolsWrite, McpScope::ResourcesRead],
            Self::FullAccess => vec![
                McpScope::ToolsRead,
                McpScope::ToolsWrite,
                McpScope::ResourcesRead,
                McpScope::Admin,
            ],
        }
    }

    pub fn allows(self, required: McpScope) -> bool {
        self.scopes().contains(&required)
    }
}

pub fn authorize_scopes(perms: TokenPermissions, required: McpScope) -> Result<(), String> {
    if perms.allows(required) {
        Ok(())
    } else {
        Err(format!("token lacks scope {:?}; has {:?}", required, perms.scopes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_blocks_write() {
        assert!(authorize_scopes(TokenPermissions::ReadOnly, McpScope::ToolsWrite).is_err());
        assert!(authorize_scopes(TokenPermissions::ReadWrite, McpScope::ToolsWrite).is_ok());
    }
}
