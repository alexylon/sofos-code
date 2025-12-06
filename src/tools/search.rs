use crate::error::{Result, SofosError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
            .build()
            .map_err(|e| SofosError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client })
    }

    /// Search the web using DuckDuckGo
    pub async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| SofosError::Search(format!("Search request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(SofosError::Search(format!(
                "Search failed with status: {}",
                response.status()
            )));
        }

        let html = response
            .text()
            .await
            .map_err(|e| SofosError::Search(format!("Failed to read response: {}", e)))?;

        self.parse_duckduckgo_html(&html, max_results)
    }

    fn parse_duckduckgo_html(&self, html: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        // Simple HTML parsing - look for result divs
        let parts: Vec<&str> = html.split("result__a").collect();

        for part in parts.iter().skip(1).take(max_results) {
            let url = if let Some(href_start) = part.find("href=\"") {
                let href_start = href_start + 6;
                if let Some(href_end) = part[href_start..].find("\"") {
                    let url = &part[href_start..href_start + href_end];
                    // DuckDuckGo uses redirect URLs, extract the actual URL
                    if url.starts_with("//duckduckgo.com/l/?uddg=") {
                        if let Some(uddg_start) = url.find("uddg=") {
                            let encoded = &url[uddg_start + 5..];
                            if let Some(amp) = encoded.find("&") {
                                urlencoding::decode(&encoded[..amp])
                                    .ok()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default()
                            } else {
                                urlencoding::decode(encoded)
                                    .ok()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default()
                            }
                        } else {
                            String::new()
                        }
                    } else {
                        url.to_string()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            let title = if let Some(title_start) = part.find(">") {
                let title_start = title_start + 1;
                if let Some(title_end) = part[title_start..].find("</a>") {
                    html_escape::decode_html_entities(&part[title_start..title_start + title_end])
                        .to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            let snippet = if let Some(snippet_start) = part.find("result__snippet") {
                if let Some(content_start) = part[snippet_start..].find(">") {
                    let content_start = snippet_start + content_start + 1;
                    if let Some(content_end) = part[content_start..].find("</") {
                        html_escape::decode_html_entities(
                            &part[content_start..content_start + content_end],
                        )
                        .to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            if !url.is_empty() && !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_search_creation() {
        let search_tool = WebSearchTool::new();
        assert!(search_tool.is_ok());
    }
}
