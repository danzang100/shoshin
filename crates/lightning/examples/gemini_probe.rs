//! Throwaway probes of the Gemini Interactions API (spec 001, task T1).
//!
//! Usage: cargo run --example gemini_probe -- <out_dir>
//! Writes `<case>.request.json` and `<case>.response.json` per case. The API key is
//! sent only in the header and never written to disk. Model is fixed to the one
//! approved for tests.
use std::{env, fs, path::PathBuf};

use lightning::tools::registry::tool_specs;
use serde_json::{Value, json};

const MODEL: &str = "gemini-3.5-flash-lite";
const URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

struct Probe {
    client: reqwest::Client,
    key: String,
    out: PathBuf,
}

impl Probe {
    async fn run(&self, case: &str, key: Option<&str>, body: Value) -> Result<Value, Box<dyn std::error::Error>> {
        fs::write(self.out.join(format!("{case}.request.json")), serde_json::to_string_pretty(&body)?)?;
        let res = self
            .client
            .post(URL)
            .header("x-goog-api-key", key.unwrap_or(&self.key))
            .json(&body)
            .send()
            .await?;
        let status = res.status().as_u16();
        let text = res.text().await?;
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        let wrapped = json!({ "http_status": status, "body": parsed });
        fs::write(self.out.join(format!("{case}.response.json")), serde_json::to_string_pretty(&wrapped)?)?;
        println!("{case}: HTTP {status}");
        Ok(wrapped["body"].clone())
    }
}

fn tool_decls(strip: bool) -> Vec<Value> {
    tool_specs()
        .into_iter()
        .map(|spec| {
            let mut params = spec.parameters.clone();
            if strip {
                if let Some(obj) = params.as_object_mut() {
                    obj.remove("$schema");
                    obj.remove("title");
                }
            }
            json!({ "type": "function", "name": spec.name, "description": spec.description, "parameters": params })
        })
        .collect()
}

fn user_step(text: &str) -> Value {
    json!({ "type": "user_input", "content": [{ "type": "text", "text": text }] })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();
    let out = PathBuf::from(env::args().nth(1).expect("usage: gemini_probe <out_dir>"));
    fs::create_dir_all(&out)?;
    let p = Probe { client: reqwest::Client::new(), key: env::var("GEMINI_API_KEY")?, out };

    // 1. plain text + system instruction + usage + generation_config
    p.run("01_text_system", None, json!({
        "model": MODEL, "store": false,
        "system_instruction": "You answer in exactly one short sentence.",
        "input": "What is Rust?",
    })).await?;

    // 2. stateless multi-turn, text only (steps as input)
    let t1 = p.run("02_multiturn_turn1", None, json!({
        "model": MODEL, "store": false, "input": [user_step("My favourite colour is teal. Remember it.")],
    })).await?;
    let mut hist = vec![user_step("My favourite colour is teal. Remember it.")];
    hist.extend(t1["steps"].as_array().cloned().unwrap_or_default());
    hist.push(user_step("What is my favourite colour?"));
    p.run("02_multiturn_turn2", None, json!({ "model": MODEL, "store": false, "input": hist })).await?;

    // 3. tool call with raw schemars output (with $schema/title) and with stripped output
    let ask = "List the files in the workspace root using a tool.";
    p.run("03a_tool_raw_schema", None, json!({
        "model": MODEL, "store": false, "input": [user_step(ask)], "tools": tool_decls(false),
    })).await?;
    let call = p.run("03b_tool_stripped_schema", None, json!({
        "model": MODEL, "store": false, "input": [user_step(ask)], "tools": tool_decls(true),
    })).await?;

    // 4. send function_result back statelessly, steps echoed verbatim
    let steps = call["steps"].as_array().cloned().unwrap_or_default();
    let fc = steps.iter().find(|s| s["type"] == "function_call").cloned();
    if let Some(fc) = fc {
        let mut hist = vec![user_step(ask)];
        hist.extend(steps.clone());
        hist.push(json!({
            "type": "function_result", "name": fc["name"], "call_id": fc["id"],
            "result": [{ "type": "text", "text": "Cargo.toml\nsrc/\nREADME.md\n" }],
        }));
        p.run("04_function_result_ok", None, json!({
            "model": MODEL, "store": false, "input": hist, "tools": tool_decls(true),
        })).await?;

        // 5. error result (is_error) then see how the model reacts
        let mut hist = vec![user_step(ask)];
        hist.extend(steps.clone());
        hist.push(json!({
            "type": "function_result", "name": fc["name"], "call_id": fc["id"], "is_error": true,
            "result": [{ "type": "text", "text": "Permission denied accessing: ." }],
        }));
        p.run("05_function_result_error", None, json!({
            "model": MODEL, "store": false, "input": hist, "tools": tool_decls(true),
        })).await?;

        // 6. echo history WITHOUT the thought steps, to learn whether they are mandatory
        let mut hist = vec![user_step(ask)];
        hist.extend(steps.iter().filter(|s| s["type"] != "thought").cloned());
        hist.push(json!({
            "type": "function_result", "name": fc["name"], "call_id": fc["id"],
            "result": [{ "type": "text", "text": "Cargo.toml\nsrc/\n" }],
        }));
        p.run("06_no_thoughts_echoed", None, json!({
            "model": MODEL, "store": false, "input": hist, "tools": tool_decls(true),
        })).await?;
    } else {
        println!("no function_call in 03b; skipping 04-06 (see 03b.response.json)");
    }

    // 7. token cap -> finish status
    p.run("07_max_tokens", None, json!({
        "model": MODEL, "store": false, "input": "Write a 500 word essay about rivers.",
        "generation_config": { "max_output_tokens": 16 },
    })).await?;

    // 8. thinking level
    p.run("08_thinking_high", None, json!({
        "model": MODEL, "store": false, "input": "What is 17 * 23?",
        "generation_config": { "thinking_level": "high", "thinking_summaries": "auto" },
    })).await?;

    // 9. error shapes
    p.run("09a_bad_model", None, json!({ "model": "gemini-does-not-exist", "input": "hi", "store": false })).await?;
    p.run("09b_bad_key", Some("not-a-real-key"), json!({ "model": MODEL, "input": "hi", "store": false })).await?;
    p.run("09c_empty_input", None, json!({ "model": MODEL, "input": [], "store": false })).await?;
    p.run("09d_nullable_schema", None, json!({
        "model": MODEL, "store": false, "input": "hi",
        "tools": [{ "type": "function", "name": "t", "description": "d",
            "parameters": { "type": "object", "properties": { "x": { "type": ["string", "null"] } } } }],
    })).await?;
    p.run("09e_unmatched_call_id", None, json!({
        "model": MODEL, "store": false,
        "input": [user_step("hi"), { "type": "function_result", "name": "list_dir", "call_id": "nope", "result": [{"type":"text","text":"x"}] }],
        "tools": tool_decls(true),
    })).await?;

    println!("done");
    Ok(())
}
