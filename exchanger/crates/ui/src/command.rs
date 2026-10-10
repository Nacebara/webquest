//! Команды клиентского бота (SPEC §6.1): `/start`, `/terms`, `/history`, `/help`, `/privacy`.

/// Команда из текста сообщения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/start` и необязательный параметр диплинка.
    Start(Option<String>),
    Terms,
    History,
    Help,
    Privacy,
}

impl Command {
    /// Имена команд для `setMyCommands`, в порядке меню.
    pub const NAMES: [&'static str; 5] = ["start", "terms", "history", "help", "privacy"];

    /// Разобрать `/cmd`, `/cmd@bot` и `/start payload`. Команда другому боту (`/help@other`)
    /// — `None`, если известно имя нашего бота.
    pub fn parse(text: &str, bot_username: Option<&str>) -> Option<Self> {
        let text = text.trim();
        let rest = text.strip_prefix('/')?;
        let (head, tail) = match rest.split_once(char::is_whitespace) {
            Some((head, tail)) => (head, tail.trim()),
            None => (rest, ""),
        };
        let (name, mention) = match head.split_once('@') {
            Some((name, mention)) => (name, Some(mention)),
            None => (head, None),
        };
        if let (Some(mention), Some(ours)) = (mention, bot_username)
            && !mention.eq_ignore_ascii_case(ours.trim_start_matches('@'))
        {
            return None;
        }
        match name.to_ascii_lowercase().as_str() {
            "start" => Some(Command::Start((!tail.is_empty()).then(|| tail.to_owned()))),
            "terms" => Some(Command::Terms),
            "history" => Some(Command::History),
            "help" => Some(Command::Help),
            "privacy" => Some(Command::Privacy),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_with_mentions_and_payload() {
        assert_eq!(Command::parse("/start", None), Some(Command::Start(None)));
        assert_eq!(
            Command::parse("/start CQabcdefghij", Some("exch_bot")),
            Some(Command::Start(Some("CQabcdefghij".into())))
        );
        assert_eq!(
            Command::parse("/Terms@Exch_Bot", Some("@exch_bot")),
            Some(Command::Terms)
        );
        assert_eq!(Command::parse("/help@other_bot", Some("exch_bot")), None);
        assert_eq!(Command::parse("/help@other_bot", None), Some(Command::Help));
        assert_eq!(Command::parse("history", None), None);
        assert_eq!(Command::parse("/unknown", None), None);
        for name in Command::NAMES {
            assert!(
                Command::parse(&format!("/{name}"), None).is_some(),
                "{name}"
            );
        }
    }
}
