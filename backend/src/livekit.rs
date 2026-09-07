use crate::config::AppConfig;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct LiveKitClaims {
    iss: String,
    sub: String,
    iat: u64,
    exp: u64,
    nbf: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    video: Option<LiveKitVideoGrant>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveKitVideoGrant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_admin: Option<bool>,
    pub room_join: bool,
    pub room: String,
    pub can_publish: bool,
    pub can_subscribe: bool,
}

#[derive(Clone)]
pub struct LiveKitClient {
    http: reqwest::Client,
    api_url: String,
    api_key: String,
    api_secret: String,
}

impl LiveKitClient {
    pub fn new(config: &AppConfig, http: reqwest::Client) -> Self {
        Self {
            http,
            api_url: config.livekit_internal_url.clone(),
            api_key: config.livekit_api_key.clone(),
            api_secret: config.livekit_api_secret.clone(),
        }
    }

    /// Mint a short-lived RoomService admin token.
    ///
    /// Returns the encode error rather than an empty string: sending `Bearer `
    /// makes LiveKit answer 401, so a signing failure used to surface as
    /// "LiveKit API error: 401" and read as a key mismatch.
    fn service_token(&self, room: &str) -> Result<String, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let claims = LiveKitClaims {
            iss: self.api_key.clone(),
            sub: String::new(),
            iat: now,
            exp: now + 600,
            nbf: now,
            video: Some(LiveKitVideoGrant {
                room_admin: Some(true),
                room_join: false,
                room: room.to_string(),
                can_publish: false,
                can_subscribe: false,
            }),
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.api_secret.as_bytes()),
        )
        .map_err(|e| format!("LiveKit service token signing failed: {e}"))
    }

    pub async fn delete_room(&self, room_name: &str) -> Result<(), String> {
        let token = self.service_token(room_name)?;
        let res = self
            .http
            .post(format!(
                "{}/twirp/livekit.RoomService/DeleteRoom",
                self.api_url
            ))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "room": room_name }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if res.status().is_success() || res.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(format!("LiveKit API error: {}", res.status()))
        }
    }

    pub async fn remove_participant(&self, room: &str, identity: &str) -> Result<(), String> {
        let token = self.service_token(room)?;
        let res = self
            .http
            .post(format!(
                "{}/twirp/livekit.RoomService/RemoveParticipant",
                self.api_url
            ))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "room": room, "identity": identity }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if res.status().is_success() || res.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(format!("LiveKit API error: {}", res.status()))
        }
    }

    pub async fn mute_published_track(
        &self,
        room: &str,
        identity: &str,
        track_sid: &str,
        muted: bool,
    ) -> Result<(), String> {
        let token = self.service_token(room)?;
        let res = self
            .http
            .post(format!(
                "{}/twirp/livekit.RoomService/MutePublishedTrack",
                self.api_url
            ))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "room": room,
                "identity": identity,
                "trackSid": track_sid,
                "muted": muted,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if res.status().is_success() || res.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(format!("LiveKit API error: {}", res.status()))
        }
    }

    /// Create a LiveKit access token for a participant.
    ///
    /// Takes `role` directly (not free-form metadata) so it's impossible to
    /// mint a role-less token — the client reads `metadata.role` to gate
    /// presenter-only UI, and an empty role would silently bypass those
    /// checks. An empty `role` returns an error.
    pub fn create_access_token(
        &self,
        identity: &str,
        name: &str,
        room: &str,
        role: &str,
        expires_at: Option<u64>,
    ) -> Result<String, String> {
        if role.is_empty() {
            return Err("LiveKit access token requires a non-empty role".into());
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Cap at 1h, and never outlive the room.
        const MAX_TTL: u64 = 3600;
        let cap = now + MAX_TTL;
        let exp = expires_at.map(|e| e.min(cap)).unwrap_or(cap);

        let claims = LiveKitClaims {
            iss: self.api_key.clone(),
            sub: identity.to_string(),
            iat: now,
            exp,
            nbf: now,
            video: Some(LiveKitVideoGrant {
                room_admin: None,
                room_join: true,
                room: room.to_string(),
                can_publish: true,
                can_subscribe: true,
            }),
        };

        // LiveKit expects identity and name in the JWT metadata
        #[derive(Serialize)]
        struct FullClaims {
            #[serde(flatten)]
            base: LiveKitClaims,
            name: String,
            metadata: String,
        }

        let full = FullClaims {
            base: claims,
            name: name.to_string(),
            metadata: serde_json::json!({ "role": role }).to_string(),
        };

        encode(
            &Header::new(Algorithm::HS256),
            &full,
            &EncodingKey::from_secret(self.api_secret.as_bytes()),
        )
        .map_err(|e| e.to_string())
    }
}
