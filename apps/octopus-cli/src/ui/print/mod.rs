use std::io::{self, BufRead};

use crate::cli::{InputFormat, OutputFormat};
use crate::soul::KimiSoul;

pub struct PrintUI {
    soul: KimiSoul,
    input_format: InputFormat,
    output_format: OutputFormat,
}

impl PrintUI {
    pub fn new(soul: KimiSoul, input_format: InputFormat, output_format: OutputFormat) -> Self {
        Self {
            soul,
            input_format,
            output_format,
        }
    }

    pub async fn run(&mut self, command: Option<String>) -> io::Result<i32> {
        let input = match command {
            Some(cmd) => cmd,
            None => match self.input_format {
                InputFormat::Text => {
                    let mut buffer = String::new();
                    let stdin = io::stdin();
                    for line in stdin.lock().lines() {
                        buffer.push_str(&line?);
                        buffer.push('\n');
                    }
                    buffer.trim().to_string()
                }
                InputFormat::StreamJson => {
                    let mut buffer = String::new();
                    let stdin = io::stdin();
                    for line in stdin.lock().lines() {
                        buffer.push_str(&line?);
                        buffer.push('\n');
                    }
                    buffer.trim().to_string()
                }
            },
        };

        if input.is_empty() {
            eprintln!("No input provided");
            return Ok(1);
        }

        let result = match self.soul.run(&input).await {
            Ok(response) => {
                match self.output_format {
                    OutputFormat::Text => {
                        println!("{}", response);
                    }
                    OutputFormat::StreamJson => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "final_message",
                                "content": response
                            })
                        );
                    }
                }
                Ok(0)
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                Ok(1)
            }
        };

        self.soul.shutdown().await;
        result
    }
}
