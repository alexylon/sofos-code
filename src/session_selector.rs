use crate::error::Result;
use crate::history::SessionMetadata;
use colored::Colorize;
use std::io::{self, Write};

pub fn select_session(sessions: Vec<SessionMetadata>) -> Result<Option<String>> {
    if sessions.is_empty() {
        println!("{}", "No saved sessions found.".yellow());
        return Ok(None);
    }

    println!("\n{}", "Select a session to resume:".bright_cyan().bold());
    println!();

    for (i, session) in sessions.iter().enumerate() {
        let date = format_timestamp(session.updated_at);
        let msg_count = format!("{} messages", session.message_count);

        println!(
            "  {} {} {}",
            format!("[{}]", i + 1).bright_green().bold(),
            session.preview.bright_white(),
            format!("({} • {})", date, msg_count).dimmed()
        );
    }

    println!();
    print!("{} ", "Enter number (or 'q' to cancel):".dimmed());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if input.is_empty() || input == "q" || input == "quit" {
        return Ok(None);
    }

    match input.parse::<usize>() {
        Ok(num) if num > 0 && num <= sessions.len() => Ok(Some(sessions[num - 1].id.clone())),
        _ => {
            println!("{}", "Invalid selection".red());
            Ok(None)
        }
    }
}

fn format_timestamp(timestamp: u64) -> String {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time is before UNIX epoch")
        .as_secs();
    let diff = now - timestamp;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let mins = diff / 60;
        format!("{} min{} ago", mins, if mins == 1 { "" } else { "s" })
    } else if diff < 86400 {
        let hours = diff / 3600;
        format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
    } else if diff < 604800 {
        let days = diff / 86400;
        format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
    } else {
        use chrono::{DateTime, Utc};
        let dt = DateTime::<Utc>::from(UNIX_EPOCH + Duration::from_secs(timestamp));
        dt.format("%Y-%m-%d").to_string()
    }
}
