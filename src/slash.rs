//! Slash command parser.

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Model(String),
    Clear,
    Compact,
    Permissions(Option<String>),
    Tools,
    Plugins,
    Scope(Option<String>),
    SessionNew(Option<String>),
    Unknown(String),
}

pub fn parse(input: &str) -> Option<Command> {
    let text = input.trim();
    if text.is_empty() || !text.starts_with('/') {
        return None;
    }
    let body = text[1..].trim();
    if body.is_empty() {
        return Some(Command::Help);
    }
    let mut parts = body.splitn(2, ' ');
    let name = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    let rest = if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    };

    match name.to_ascii_lowercase().as_str() {
        "help" | "?" => Some(Command::Help),
        "model" => Some(Command::Model(rest.unwrap_or_default())),
        "clear" => Some(Command::Clear),
        "compact" => Some(Command::Compact),
        "permissions" | "perms" => Some(Command::Permissions(rest)),
        "tools" => Some(Command::Tools),
        "plugins" => Some(Command::Plugins),
        "scope" => Some(Command::Scope(rest)),
        "new" | "session" => Some(Command::SessionNew(rest)),
        _ => Some(Command::Unknown(name.to_string())),
    }
}

pub fn help_text() -> &'static str {
    "/help                 Show this help\n\
     /model [name]         Show or switch model\n\
     /scope [name]         coding|research|plan|ask|computer_use\n\
     /permissions [mode]   full_access|read_only|workspace_write|deny_all\n\
     /clear                Clear conversation\n\
     /compact              Compact context\n\
     /tools                List tools\n\
     /plugins              List plugins\n\
     /new [name]           New session"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model() {
        assert_eq!(
            parse("/model gpt-4o"),
            Some(Command::Model("gpt-4o".into()))
        );
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse("/help"), Some(Command::Help));
        assert_eq!(parse("not a command"), None);
    }

    #[test]
    fn parse_scope() {
        assert_eq!(
            parse("/scope coding"),
            Some(Command::Scope(Some("coding".into())))
        );
    }
}
