use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use schemars::{gen::SchemaSettings, JsonSchema};
use crate::routes::sync::types::AppError;

#[derive(Serialize)]
pub struct GeminiRequest {
    pub contents: Vec<Content>,
    #[serde(rename = "generationConfig")]
    pub generation_config: GenerationConfig,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
}

#[derive(Serialize, Deserialize)]
pub struct Content {
    pub parts: Vec<Part>,
}

#[derive(Serialize, Deserialize)]
pub struct Part {
    pub text: String,
}

#[derive(Serialize)]
pub struct GenerationConfig {
    #[serde(rename = "responseMimeType")]
    pub response_mime_type: String,
    #[serde(rename = "responseSchema")]
    pub response_schema: Option<Value>,
}

#[derive(Deserialize)]
pub struct GeminiResponse {
    pub candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
pub struct Candidate {
    pub content: Content,
}

pub fn get_schema<T: JsonSchema>() -> Value {
    let settings = SchemaSettings::draft07();
    let gen = settings.into_generator();
    let schema = gen.into_root_schema_for::<T>();
    let mut schema_val = serde_json::to_value(schema).unwrap();
    if let Some(obj) = schema_val.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    schema_val
}

pub async fn call_gemini<T: DeserializeOwned + JsonSchema>(
    api_key: &str,
    system_prompt: Option<&str>,
    user_prompt: &str,
    model: &str,
) -> Result<T, AppError> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let schema = get_schema::<T>();

    let request = GeminiRequest {
        contents: vec![Content {
            parts: vec![Part {
                text: user_prompt.to_string(),
            }],
        }],
        system_instruction: system_prompt.map(|s| Content {
            parts: vec![Part {
                text: s.to_string(),
            }],
        }),
        generation_config: GenerationConfig {
            response_mime_type: "application/json".to_string(),
            response_schema: Some(schema),
        },
    };

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Failed to call Gemini: {:?}", e);
            AppError::Gemini(format!("Gemini API call failed: {}", e))
        })?;

    let gemini_resp: GeminiResponse = response.json().await.map_err(|e| {
        tracing::error!("Failed to parse Gemini response: {:?}", e);
        AppError::Gemini(format!("Failed to parse Gemini response: {}", e))
    })?;

    let text = gemini_resp
        .candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .map(|p| p.text.as_str())
        .ok_or_else(|| {
            tracing::error!("Empty Gemini response");
            AppError::Gemini("Empty Gemini response".to_string())
        })?;

    let result: T = serde_json::from_str(text).map_err(|e| {
        tracing::error!("Failed to deserialize Gemini output into target type: {}. Text: {}", e, text);
        AppError::Serialization(e)
    })?;

    Ok(result)
}
