// SPDX-License-Identifier: BUSL-1.1
//! Implémentation réelle de l'[`Embedder`] via **Candle** (architecture BERT).
//!
//! Invariant ADR : cet `Embedder` **ne télécharge jamais** et **ne détecte
//! jamais** le matériel. Il reçoit un dossier LOCAL (déjà provisionné par le
//! setup hardware-aware) et un [`Device`] déjà résolu.

use std::path::Path;

use candle_core::{DType, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use super::{Device, Embedder};
use crate::{CoreError, Result};

/// Modèle baseline unique V1 (ADR-003).
const MODEL_ID: &str = "all-MiniLM-L6-v2";
/// Dimension des vecteurs produits par le baseline.
const DIM: usize = 384;
/// Type de calcul des poids et des activations.
const DTYPE: DType = DType::F32;

/// Embedder Candle (BERT) chargé depuis un dossier de modèle **local**.
///
/// Charge `config.json`, `tokenizer.json` et `model.safetensors`, puis produit
/// des embeddings de phrase par *mean-pooling* masqué + normalisation L2.
pub struct CandleEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: candle_core::Device,
}

impl CandleEmbedder {
    /// Charge le modèle depuis un dossier LOCAL déjà provisionné.
    ///
    /// Le dossier doit contenir `config.json`, `tokenizer.json` et
    /// `model.safetensors`. Aucun accès réseau n'est effectué.
    ///
    /// # Errors
    /// Retourne [`CoreError::Embed`] si un fichier manque, si la config ou le
    /// tokenizer sont illisibles, ou si le chargement des poids échoue.
    pub fn load(model_dir: &Path, device: Device) -> Result<Self> {
        let config_path = model_dir.join("config.json");
        let tokenizer_path = model_dir.join("tokenizer.json");
        let weights_path = model_dir.join("model.safetensors");

        let device = to_candle_device(device);

        // Config BERT (sérialisée façon HuggingFace) → désérialisée par serde.
        let config_bytes =
            std::fs::read(&config_path).map_err(|e| CoreError::Embed(format!("lecture de config.json: {e}")))?;
        let config: Config = serde_json::from_slice(&config_bytes)
            .map_err(|e| CoreError::Embed(format!("parsing de config.json: {e}")))?;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| CoreError::Embed(format!("chargement du tokenizer: {e}")))?;
        // `encode_batch` ne pad pas tout seul : sans ceci, `stack_u32` reçoit des
        // lignes de longueurs différentes et échoue dès qu'un lot contient plus
        // d'un texte de tailles distinctes (le cas normal).
        tokenizer.with_padding(Some(PaddingParams::default()));
        // EMBED-TRUNC (BaseMyAI adversarial audit, 2026-07-22) : sans borne de
        // troncature, un texte de plusieurs milliers de mots — bien en deçà
        // des limites REST/MCP en caractères (`MAX_TEXT_LEN`), qui ne bornent
        // que le nombre d'octets, jamais le nombre de tokens — produit une
        // séquence tokenisée qui peut dépasser `max_position_embeddings` du
        // modèle chargé, avec un coût d'auto-attention O(seq_len²) non borné
        // et un comportement non garanti au-delà de cette longueur (panic ou
        // sortie dégradée selon la version de `candle-transformers`). Dérivée
        // du modèle réellement chargé (`config.max_position_embeddings`),
        // jamais une constante en dur — un futur modèle avec une fenêtre
        // différente reste correct sans y retoucher.
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: config.max_position_embeddings,
                ..TruncationParams::default()
            }))
            .map_err(|e| CoreError::Embed(format!("configuration de la troncature du tokenizer: {e}")))?;

        // SAFETY (candle) : mmap d'un fichier safetensors de confiance, local.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
                .map_err(|e| CoreError::Embed(format!("mmap des poids safetensors: {e}")))?
        };

        let model =
            BertModel::load(vb, &config).map_err(|e| CoreError::Embed(format!("chargement du modèle BERT: {e}")))?;

        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    /// Forward + mean-pooling masqué + normalisation L2 pour un lot de textes.
    fn forward_pooled(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Tokenisation par lot (padding à la longueur max du lot).
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| CoreError::Embed(format!("tokenisation: {e}")))?;

        let mut ids_rows = Vec::with_capacity(encodings.len());
        let mut mask_rows = Vec::with_capacity(encodings.len());
        for enc in &encodings {
            ids_rows.push(enc.get_ids().to_vec());
            mask_rows.push(enc.get_attention_mask().to_vec());
        }

        let token_ids = stack_u32(&ids_rows, &self.device)?;
        let attention_mask = stack_u32(&mask_rows, &self.device)?;
        let token_type_ids = token_ids
            .zeros_like()
            .map_err(|e| CoreError::Embed(format!("token_type_ids: {e}")))?;

        let hidden = self
            .model
            .forward(&token_ids, &token_type_ids, Some(&attention_mask))
            .map_err(|e| CoreError::Embed(format!("forward BERT: {e}")))?;

        // Mean-pooling masqué : on ignore les tokens de padding.
        let mask = attention_mask
            .to_dtype(DTYPE)
            .and_then(|m| m.unsqueeze(2))
            .map_err(|e| CoreError::Embed(format!("préparation du masque: {e}")))?;
        let summed = hidden
            .broadcast_mul(&mask)
            .and_then(|t| t.sum(1))
            .map_err(|e| CoreError::Embed(format!("somme pondérée: {e}")))?;
        let counts = mask
            .sum(1)
            .map_err(|e| CoreError::Embed(format!("somme du masque: {e}")))?;
        let pooled = summed
            .broadcast_div(&counts)
            .map_err(|e| CoreError::Embed(format!("moyenne: {e}")))?;

        // Normalisation L2 ligne par ligne.
        let normalized = normalize_l2(&pooled)?;

        let rows = normalized
            .to_vec2::<f32>()
            .map_err(|e| CoreError::Embed(format!("extraction des vecteurs: {e}")))?;
        Ok(rows)
    }
}

impl Embedder for CandleEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut rows = self.forward_pooled(&[text])?;
        rows.pop()
            .ok_or_else(|| CoreError::Embed("aucun vecteur produit".into()))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.forward_pooled(&refs)
    }

    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        DIM
    }
}

/// Convertit notre [`Device`] agnostique vers celui de Candle.
///
/// CUDA et Metal exigent les features Candle correspondantes ; sans elles (ou
/// si l'init GPU échoue), on **replie sur CPU** — repli universel garanti.
fn to_candle_device(device: Device) -> candle_core::Device {
    match device {
        Device::Cpu => candle_core::Device::Cpu,
        Device::Cuda(index) => candle_core::Device::new_cuda(index).unwrap_or(candle_core::Device::Cpu),
        Device::Metal => candle_core::Device::new_metal(0).unwrap_or(candle_core::Device::Cpu),
    }
}

/// Empile des lignes `u32` (longueur uniforme garantie par le padding) en un
/// tenseur `[batch, seq_len]`.
fn stack_u32(rows: &[Vec<u32>], device: &candle_core::Device) -> Result<Tensor> {
    let tensors = rows
        .iter()
        .map(|row| Tensor::new(row.as_slice(), device))
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(|e| CoreError::Embed(format!("création des tenseurs d'entrée: {e}")))?;
    Tensor::stack(&tensors, 0).map_err(|e| CoreError::Embed(format!("empilage du lot: {e}")))
}

/// Normalisation L2 ligne par ligne (`v / ||v||_2`).
fn normalize_l2(v: &Tensor) -> Result<Tensor> {
    let norm = v
        .sqr()
        .and_then(|s| s.sum_keepdim(1))
        .and_then(|s| s.sqrt())
        .map_err(|e| CoreError::Embed(format!("norme L2: {e}")))?;
    v.broadcast_div(&norm)
        .map_err(|e| CoreError::Embed(format!("normalisation L2: {e}")))
}
