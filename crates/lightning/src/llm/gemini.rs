use std::collections::HashMap;
use dotenv::dotenv;
use reqwest::header::{ HeaderMap};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    let api_key = env::var("GEMINI_API_KEY")?;

    let mut headers = HeaderMap::new();

    headers.insert("x-goog-api-key", format!("Bearer {}", api_key).parse()?);
    headers.insert("Content-Type", "application/json".parse()?);

    let mut body = HashMap::new();

    body.insert("model", "gemini-3.8-flash");
    body.insert("input", "Hello, world!");

    let client = reqwest::Client::new();
    let res = client
    .post("https://generativelanguage.googleapis.com/v1beta/interactions")
    .headers(headers)
    .json(&body)
    .send()
    .await?;

    println!("{res:#?}");
    Ok(())
}