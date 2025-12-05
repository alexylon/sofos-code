use crate::api::{AnthropicClient, ContentBlock, CreateMessageRequest, MorphClient};
use crate::conversation::ConversationHistory;
use crate::error::{Result, SofosError};
use crate::tools::{add_code_search_tool, get_tools, get_tools_with_morph, ToolExecutor};
use colored::Colorize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;

pub struct Repl {
    client: AnthropicClient,
    conversation: ConversationHistory,
    tool_executor: ToolExecutor,
    editor: DefaultEditor,
    model: String,
    max_tokens: u32,
    recursion_depth: u32,
}

impl Repl {
    pub fn new(
        api_key: String,
        model: String,
        max_tokens: u32,
        workspace: PathBuf,
        morph_client: Option<MorphClient>,
    ) -> Result<Self> {
        let client = AnthropicClient::new(api_key)?;
        let tool_executor = ToolExecutor::new(workspace, morph_client)?;
        
        let has_morph = tool_executor.has_morph();
        let has_code_search = tool_executor.has_code_search();
        let conversation = ConversationHistory::with_features(has_morph, has_code_search);
        
        let editor = DefaultEditor::new()
            .map_err(|e| SofosError::Config(format!("Failed to create editor: {}", e)))?;

        Ok(Self {
            client,
            conversation,
            tool_executor,
            editor,
            model,
            max_tokens,
            recursion_depth: 0,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        println!("{}", "Sofos - AI Coding Assistant".bright_cyan().bold());
        println!("{}", "Type your message or 'exit' to quit.".dimmed());
        println!("{}", "Type 'clear' to clear conversation history.".dimmed());
        println!();

        loop {
            let readline = self.editor.readline(&format!("{} ", ">>>".bright_green()));

            match readline {
                Ok(line) => {
                    let line = line.trim();

                    if line.is_empty() {
                        continue;
                    }

                    let _ = self.editor.add_history_entry(line);

                    match line.to_lowercase().as_str() {
                        "exit" | "quit" => {
                            println!("{}", "Goodbye!".bright_cyan());
                            break;
                        }
                        "clear" => {
                            self.conversation.clear();
                            println!("{}", "Conversation history cleared.".bright_yellow());
                            continue;
                        }
                        _ => {}
                    }

                    if let Err(e) = self.process_message(line) {
                        eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    }

                    println!();
                }
                Err(ReadlineError::Interrupted) => {
                    println!("{}", "Use 'exit' to quit.".dimmed());
                }
                Err(ReadlineError::Eof) => {
                    println!("{}", "Goodbye!".bright_cyan());
                    break;
                }
                Err(e) => {
                    eprintln!("{} {}", "Error:".bright_red().bold(), e);
                    break;
                }
            }
        }

        Ok(())
    }

    fn get_available_tools(&self) -> Vec<crate::api::Tool> {
        let mut tools = if self.tool_executor.has_morph() {
            get_tools_with_morph()
        } else {
            get_tools()
        };

        if self.tool_executor.has_code_search() {
            add_code_search_tool(&mut tools);
        }

        tools
    }

    fn process_message(&mut self, user_input: &str) -> Result<()> {
        self.conversation.add_user_message(user_input.to_string());

        let request = CreateMessageRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: self.conversation.messages().to_vec(),
            system: Some(self.conversation.system_prompt().to_string()),
            tools: Some(self.get_available_tools()),
            stream: None,
        };

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| SofosError::Config(format!("Failed to create async runtime: {}", e)))?;

        let response = runtime.block_on(self.client.create_message(request))?;

        self.recursion_depth = 0;
        self.handle_response(response.content, &runtime)?;

        Ok(())
    }

    fn handle_response(
        &mut self,
        content_blocks: Vec<ContentBlock>,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<()> {
        const MAX_RECURSION_DEPTH: u32 = 10;

        if self.recursion_depth >= MAX_RECURSION_DEPTH {
            eprintln!(
                "{} Maximum recursion depth reached. Stopping to prevent infinite loop.",
                "Warning:".bright_yellow().bold()
            );
            return Ok(());
        }

        let mut text_output = Vec::new();
        let mut tool_uses = Vec::new();

        for block in &content_blocks {
            match block {
                ContentBlock::Text { text } => {
                    text_output.push(text.clone());
                }
                ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push((id.clone(), name.clone(), input.clone()));
                }
            }
        }

        if !text_output.is_empty() {
            println!("{}", "Assistant:".bright_blue().bold());
            for text in &text_output {
                println!("{}", text);
            }
            println!();
        }

        // Always add assistant content to conversation history FIRST
        self.conversation.add_assistant_content(&content_blocks);

        if !tool_uses.is_empty() {
            for (_tool_id, tool_name, tool_input) in &tool_uses {
                println!(
                    "{} {}",
                    "Using tool:".bright_yellow().bold(),
                    tool_name.bright_yellow()
                );

                let result = runtime.block_on(self.tool_executor.execute(tool_name, tool_input));

                match result {
                    Ok(output) => {
                        println!("{}", output.dimmed());
                        println!();
                        self.conversation.add_tool_result(tool_name, &output);
                    }
                    Err(e) => {
                        let error_msg = format!("Tool execution failed: {}", e);
                        eprintln!("{} {}", "Error:".bright_red().bold(), error_msg);
                        println!();
                        self.conversation.add_tool_result(tool_name, &error_msg);
                    }
                }
            }

            // After executing tools, get another response from Claude
            let request = CreateMessageRequest {
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                messages: self.conversation.messages().to_vec(),
                system: Some(self.conversation.system_prompt().to_string()),
                tools: Some(self.get_available_tools()),
                stream: None,
            };

            let response = runtime.block_on(self.client.create_message(request))?;

            // Increment recursion depth before recursive call
            self.recursion_depth += 1;
            return self.handle_response(response.content, runtime);
        }

        Ok(())
    }

    pub fn process_single_prompt(&mut self, prompt: &str) -> Result<()> {
        println!("{} {}", ">>>".bright_green(), prompt);
        println!();
        self.process_message(prompt)?;
        Ok(())
    }
}
