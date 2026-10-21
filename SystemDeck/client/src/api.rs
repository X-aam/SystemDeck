//! Blocking HTTP client for the SystemDeck server API.

use std::{fmt, time::Duration};

use reqwest::{blocking::Client, Method, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use uuid::Uuid;
use systemdeck_core::{api::*, Team, UserInfo, Vault, VaultSummary};

#[derive(Debug)]
pub enum ApiError {
    /// Token missing, expired or revoked: the user must sign in again.
    Unauthorized(String),
    Forbidden(String),
    Server(u16, String),
    /// Server unreachable: keep working from the local cache.
    Network(String),
    Local(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Unauthorized(m) => write!(f, "Not signed in: {m}"),
            ApiError::Forbidden(m) => write!(f, "Access denied: {m}"),
            ApiError::Server(code, m) => write!(f, "Server error {code}: {m}"),
            ApiError::Network(m) => write!(f, "Cannot reach server: {m}"),
            ApiError::Local(m) => write!(f, "Local storage error: {m}"),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        ApiError::Local(format!("{err:#}"))
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Accepts `server`, `server:8740` or a full URL.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

#[derive(Clone)]
pub struct Api {
    base: String,
    token: Option<String>,
    http: Client,
}

impl Api {
    pub fn new(base: &str, token: Option<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("building HTTP client");
        Self { base: normalize_url(base), token, http }
    }

    fn call<B: Serialize + ?Sized, T: DeserializeOwned>(&self, method: Method, path: &str, body: Option<&B>) -> ApiResult<T> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return resp.json::<T>().map_err(|e| ApiError::Server(status.as_u16(), format!("unexpected response: {e}")));
        }
        let message = resp.json::<ErrorBody>().map(|b| b.error).unwrap_or_else(|_| status.to_string());
        Err(match status {
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized(message),
            StatusCode::FORBIDDEN => ApiError::Forbidden(message),
            _ => ApiError::Server(status.as_u16(), message),
        })
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        self.call::<(), T>(Method::GET, path, None)
    }

    fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T> {
        self.call(Method::POST, path, Some(body))
    }

    fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T> {
        self.call(Method::PUT, path, Some(body))
    }

    fn delete(&self, path: &str) -> ApiResult<Value> {
        self.call::<(), Value>(Method::DELETE, path, None)
    }

    // ---- session ----

    pub fn login(&self, username: &str, password: &str) -> ApiResult<LoginResponse> {
        self.post("/api/login", &LoginRequest { username: username.to_string(), password: password.to_string() })
    }

    pub fn logout(&self) -> ApiResult<Value> {
        self.post("/api/logout", &())
    }

    pub fn change_password(&self, old_password: &str, new_password: &str) -> ApiResult<Value> {
        self.post(
            "/api/me/password",
            &ChangePasswordRequest { old_password: old_password.to_string(), new_password: new_password.to_string() },
        )
    }

    // ---- sync ----

    pub fn vaults(&self) -> ApiResult<Vec<VaultSummary>> {
        self.get("/api/vaults")
    }

    pub fn changes(&self, vault: Uuid, since: u64) -> ApiResult<ChangesResponse> {
        self.get(&format!("/api/vaults/{vault}/changes?since={since}"))
    }

    pub fn push(&self, vault: Uuid, req: &PushRequest) -> ApiResult<PushResponse> {
        self.post(&format!("/api/vaults/{vault}/push"), req)
    }

    // ---- SSH session recordings ----

    pub fn recording_start(&self, req: &RecordingStart) -> ApiResult<RecordingStarted> {
        self.post("/api/recordings", req)
    }

    /// Appends raw asciicast bytes.
    pub fn recording_chunk(&self, id: Uuid, data: Vec<u8>) -> ApiResult<()> {
        let mut req = self
            .http
            .post(format!("{}/api/recordings/{id}/chunk", self.base))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(data);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let message = resp.json::<ErrorBody>().map(|b| b.error).unwrap_or_else(|_| status.to_string());
        Err(match status {
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized(message),
            StatusCode::FORBIDDEN => ApiError::Forbidden(message),
            _ => ApiError::Server(status.as_u16(), message),
        })
    }

    pub fn recording_finish(&self, id: Uuid) -> ApiResult<Value> {
        self.post(&format!("/api/recordings/{id}/finish"), &())
    }

    // ---- in-session AI assistant ----

    pub fn ai_models(&self) -> ApiResult<AiModelsInfo> {
        self.get("/api/ai/models")
    }

    pub fn ai_chat(&self, req: &AiChatRequest) -> ApiResult<AiChatResponse> {
        // The server waits on a local model, which can take well over the
        // default 20 s request timeout, so allow much longer for this call.
        let mut r = self.http.post(format!("{}/api/ai/chat", self.base)).json(req).timeout(Duration::from_secs(180));
        if let Some(token) = &self.token {
            r = r.bearer_auth(token);
        }
        let resp = r.send().map_err(|e| ApiError::Network(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return resp.json::<AiChatResponse>().map_err(|e| ApiError::Server(status.as_u16(), format!("unexpected response: {e}")));
        }
        let message = resp.json::<ErrorBody>().map(|b| b.error).unwrap_or_else(|_| status.to_string());
        Err(match status {
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized(message),
            StatusCode::FORBIDDEN => ApiError::Forbidden(message),
            _ => ApiError::Server(status.as_u16(), message),
        })
    }

    // ---- administration ----

    pub fn admin_users(&self) -> ApiResult<Vec<UserInfo>> {
        self.get("/api/admin/users")
    }

    pub fn admin_create_user(&self, req: &CreateUserRequest) -> ApiResult<UserInfo> {
        self.post("/api/admin/users", req)
    }

    pub fn admin_update_user(&self, username: &str, req: &UpdateUserRequest) -> ApiResult<UserInfo> {
        self.put(&format!("/api/admin/users/{username}"), req)
    }

    pub fn admin_teams(&self) -> ApiResult<Vec<Team>> {
        self.get("/api/admin/teams")
    }

    pub fn admin_create_team(&self, req: &TeamRequest) -> ApiResult<Team> {
        self.post("/api/admin/teams", req)
    }

    pub fn admin_update_team(&self, id: Uuid, req: &TeamRequest) -> ApiResult<Team> {
        self.put(&format!("/api/admin/teams/{id}"), req)
    }

    pub fn admin_delete_team(&self, id: Uuid) -> ApiResult<Value> {
        self.delete(&format!("/api/admin/teams/{id}"))
    }

    pub fn admin_vaults(&self) -> ApiResult<Vec<Vault>> {
        self.get("/api/admin/vaults")
    }

    pub fn admin_create_vault(&self, req: &VaultRequest) -> ApiResult<Vault> {
        self.post("/api/admin/vaults", req)
    }

    pub fn admin_update_vault(&self, id: Uuid, req: &VaultRequest) -> ApiResult<Vault> {
        self.put(&format!("/api/admin/vaults/{id}"), req)
    }

    pub fn admin_delete_vault(&self, id: Uuid) -> ApiResult<Value> {
        self.delete(&format!("/api/admin/vaults/{id}"))
    }
}
