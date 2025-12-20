use std::io;
use std::io::Write;

pub fn confirm_action(prompt: &str) -> crate::error::Result<bool> {
    print!("{} (y/n): ", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}
