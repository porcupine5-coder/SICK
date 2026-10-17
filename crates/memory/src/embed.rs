//! Local bag-of-words embeddings (Phase 4 semantic memory).
//!
//! Deterministic, offline, and dependency-free: tokens are hashed into a
//! fixed 384-dim vector (unigrams + adjacent bigrams) with sublinear term
//! weights, then L2-normalized. Semantic search ranks by cosine similarity.
//!
//! The phase plan allows a TF-IDF/bag-of-words fallback in place of shipping a
//! neural embedding model; this module is that fallback. Swapping in a real
//! model later only requires replacing [`embed`] — storage and search work on
//! raw `f32` arrays of [`EMBEDDING_DIM`] length (vector-length mismatches are
//! filtered out at search time, so old rows simply stop matching).

/// Dimensionality of every embedding produced by [`embed`].
pub const EMBEDDING_DIM: usize = 384;

/// Embed `text` into a normalized [`EMBEDDING_DIM`]-dim feature-hash vector.
pub fn embed(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; EMBEDDING_DIM];

    let tokens: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect();

    for token in &tokens {
        add_feature(&mut vector, token);
    }
    for pair in tokens.windows(2) {
        add_feature(&mut vector, &format!("{} {}", pair[0], pair[1]));
    }

    // Sublinear term weighting, then L2 normalization.
    for value in &mut vector {
        *value = value.sqrt();
    }
    normalize(&mut vector);
    vector
}

/// Serialize an embedding as little-endian `f32` bytes (SQLite BLOB).
pub fn to_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Deserialize [`to_bytes`] output (trailing partial values are ignored).
pub fn from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Cosine similarity; `0.0` when dimensions differ or a vector is all zero.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

fn add_feature(vector: &mut [f32], feature: &str) {
    let hash = fnv1a(feature.as_bytes());
    let index = (hash % EMBEDDING_DIM as u64) as usize;
    vector[index] += 1.0;
}

fn normalize(vector: &mut [f32]) {
    let norm: f32 = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_has_fixed_dimension_and_unit_norm() {
        let vector = embed("write a Rust file");
        assert_eq!(vector.len(), EMBEDDING_DIM);
        let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }

    #[test]
    fn identical_texts_are_identical_and_similar_texts_beat_unrelated() {
        let a = embed("write a rust file for parsing json");
        let b = embed("write a rust file for parsing json");
        assert!(cosine(&a, &b) > 0.999);

        let related = embed("rust file parsing json reader");
        let unrelated = embed("banana smoothie recipe with mango");
        assert!(
            cosine(&a, &related) > cosine(&a, &unrelated),
            "related: {}, unrelated: {}",
            cosine(&a, &related),
            cosine(&a, &unrelated)
        );
    }

    #[test]
    fn empty_text_is_all_zero_and_cosine_is_safe() {
        let empty = embed("");
        assert!(empty.iter().all(|v| *v == 0.0));
        assert_eq!(cosine(&empty, &empty), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn bytes_round_trip() {
        let vector = embed("round trip me");
        let bytes = to_bytes(&vector);
        assert_eq!(bytes.len(), EMBEDDING_DIM * 4);
        assert_eq!(from_bytes(&bytes), vector);
        assert!(from_bytes(&[1, 2, 3]).is_empty());
    }
}
