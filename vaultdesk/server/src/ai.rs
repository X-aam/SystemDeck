//! AI features backed by a local OpenAI-compatible endpoint (Ollama,
//! llama.cpp, vLLM, LM Studio...). Nothing leaves the network the server runs
//! on; the endpoint is whatever the admin configures.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use systemdeck_core::api::AiChatTurn;

#[derive(Clone)]
pub struct Ai {
    /// Base URL ending in `/v1` (e.g. `http://localhost:11434/v1`).
    base: String,
    model: String,
    key: Option<String>,
    http: reqwest::Client,
}

/// The structured result of summarising a session recording.
#[derive(Debug, Deserialize)]
pub struct Summary {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

impl Ai {
    /// None when no endpoint is configured (AI features then return 503).
    pub fn new(base: Option<String>, model: Option<String>, key: Option<String>) -> Option<Self> {
        let base = base?.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return None;
        }
        Some(Self {
            base,
            model: model.unwrap_or_else(|| "llama3.1".into()),
            key: key.filter(|k| !k.is_empty()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(150))
                .connect_timeout(std::time::Duration::from_secs(8))
                .build()
                .ok()?,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Posts a chat-completions body and returns the first choice's content.
    async fn send(&self, body: Value) -> Result<String> {
        let mut req = self.http.post(format!("{}/chat/completions", self.base)).json(&body);
        if let Some(key) = &self.key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.context("calling the AI endpoint")?;
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("AI endpoint returned {code}: {}", text.chars().take(300).collect::<String>());
        }
        #[derive(Deserialize)]
        struct ChatResp {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: Msg,
        }
        #[derive(Deserialize)]
        struct Msg {
            content: String,
        }
        let parsed: ChatResp = resp.json().await.context("parsing the AI reply")?;
        parsed.choices.into_iter().next().map(|c| c.message.content).context("the AI returned no choices")
    }

    /// One chat completion. `json` asks the model for a JSON object reply.
    async fn chat(&self, system: &str, user: &str, want_json: bool) -> Result<String> {
        let mut body = json!({
            "model": self.model,
            "messages": [ { "role": "system", "content": system }, { "role": "user", "content": user } ],
            "temperature": 0.2,
            "stream": false,
        });
        if want_json {
            body["response_format"] = json!({ "type": "json_object" });
        }
        self.send(body).await
    }

    /// A multi-turn conversation: a system prompt, prior turns, then a new
    /// user message. Used by the in-session assistant.
    pub async fn converse(&self, system: &str, history: &[AiChatTurn], user: &str) -> Result<String> {
        let mut messages = vec![json!({ "role": "system", "content": system })];
        for turn in history {
            let role = if turn.role == "assistant" { "assistant" } else { "user" };
            messages.push(json!({ "role": role, "content": turn.content }));
        }
        messages.push(json!({ "role": "user", "content": user }));
        self.send(json!({ "model": self.model, "messages": messages, "temperature": 0.3, "stream": false })).await
    }

    /// Lists the model ids this endpoint serves (GET /v1/models). Many local
    /// runners (Ollama, vLLM, LM Studio) host several models behind one URL.
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let mut req = self.http.get(format!("{}/models", self.base));
        if let Some(key) = &self.key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.context("listing models from the AI endpoint")?;
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("AI endpoint returned {code}: {}", text.chars().take(200).collect::<String>());
        }
        #[derive(Deserialize)]
        struct Models {
            #[serde(default)]
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            id: String,
        }
        let mut ids: Vec<String> = resp.json::<Models>().await.context("parsing the model list")?.data.into_iter().map(|m| m.id).collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Quick reachability/auth check: a one-token prompt. Returns the reply.
    pub async fn ping(&self) -> Result<String> {
        self.chat("You are a health check.", "Reply with the single word OK.", false).await
    }

    /// Summarises a session transcript and flags sensitive actions.
    pub async fn summarise(&self, protocol: &str, who: &str, host: &str, transcript: &str) -> Result<Summary> {
        let system = "You review IT administrator session recordings for an audit log. \
            Given a transcript, reply with a JSON object: \
            {\"summary\": \"2-4 plain-English sentences on what the operator did\", \
            \"flags\": [\"short notes on sensitive or risky actions\"]}. \
            Flag things like: privilege escalation (sudo/su/runas), creating or changing users or passwords, \
            deleting data (rm -rf, DROP, delete), firewall or network changes, stopping/restarting services, \
            disabling security tooling, or mass changes. If nothing is sensitive, use an empty flags array. \
            Base everything only on the transcript; do not invent actions.";
        let user = format!(
            "Protocol: {protocol}\nOperator: {who}\nTarget: {host}\n\n--- transcript (may be truncated) ---\n{transcript}"
        );
        let raw = self.chat(system, &user, true).await?;
        // Models sometimes wrap JSON in prose or code fences; extract the object.
        let slice = raw
            .find('{')
            .and_then(|start| raw.rfind('}').map(|end| &raw[start..=end]))
            .unwrap_or(raw.as_str());
        serde_json::from_str(slice).with_context(|| format!("AI did not return valid JSON: {}", raw.chars().take(300).collect::<String>()))
    }
}
