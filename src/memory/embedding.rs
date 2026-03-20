/// Text embedding utilities for vector-based memory search.
///
/// Embedding computation requires:
///   EMBEDDING_API_URL — e.g. "https://api.openai.com/v1/embeddings"
///   OPENAI_API_KEY or EMBEDDING_API_KEY — bearer token
///   EMBEDDING_MODEL (optional) — defaults to "text-embedding-3-small"
///
/// When these env vars are absent or the API call fails, all functions
/// return None/empty so the caller can fall back to keyword scoring.

/// Call an OpenAI-compatible embeddings endpoint and return the vector.
pub async fn compute_embedding(text: &str) -> Option<Vec<f32>> {
    let api_url = std::env::var("EMBEDDING_API_URL")
        .ok()
        .filter(|s| !s.is_empty())?;
    let api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("EMBEDDING_API_KEY"))
        .ok()
        .filter(|s| !s.is_empty())?;
    let model = std::env::var("EMBEDDING_MODEL")
        .unwrap_or_else(|_| "text-embedding-3-small".to_string());

    let client = reqwest::Client::new();
    let resp = client
        .post(&api_url)
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "model": model,
            "input": text,
        }))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            "Embedding API returned non-success status"
        );
        return None;
    }

    let body: serde_json::Value = resp.json().await.ok()?;
    let vec: Vec<f32> = body["data"][0]["embedding"]
        .as_array()?
        .iter()
        .filter_map(|v| v.as_f64().map(|f| f as f32))
        .collect();

    if vec.is_empty() { None } else { Some(vec) }
}

/// Cosine similarity between two same-length vectors. Returns a value in [0, 1].
/// Returns 0.0 for mismatched lengths or zero-norm vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| *x as f64 * *y as f64)
        .sum();
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(0.0, 1.0)
}

/// Serialize a float32 vector to little-endian bytes for SQLite BLOB storage.
pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize little-endian bytes back to a float32 vector.
pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![0.1f32, 0.5, -0.3, 1.0];
        let bytes = encode_embedding(&v);
        let decoded = decode_embedding(&bytes);
        assert_eq!(v.len(), decoded.len());
        for (a, b) in v.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim < 1e-6);
    }

    #[test]
    fn cosine_length_mismatch_returns_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }
}
