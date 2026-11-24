//! Input-building helpers.
//!
//! Convenience only. Nothing here is part of the gear's contract — these are
//! free functions over the same public types a consumer could assemble by
//! hand, and the gear neither knows nor cares whether they were used.
//!
//! They exist because tensor assembly is the most mechanical and most
//! error-prone step in calling [`crate::runtime::ModelRuntime::run`]: a wrong
//! shape or a missing input is a runtime failure on live traffic, and every
//! consumer of a BERT-style model writes the same dozen lines to avoid it.

use super::error::RuntimeError;
use super::types::{NamedTensor, TensorData};

/// Builds the input tensors a BERT-style token classifier expects, from an
/// encoding's ids and attention mask and the graph's declared input names.
///
/// Supplies `input_ids` and `attention_mask` always; `token_type_ids` —
/// zero-filled, single-segment — and `position_ids` — `0..n` — only when the
/// graph declares them. That conditional is the point: some exports declare
/// them and some do not, omitting a declared input is an error, and supplying
/// an undeclared one is too, so the correct set cannot be hardcoded.
///
/// `ids` and `attention_mask` are moved into the tensors, not copied; take
/// them out of a [`TokenEncoding`](super::TokenEncoding) and keep its offsets for decoding.
///
/// Pass `declared_inputs` from [`super::LoadedModelInfo::input_names`]; a name
/// this function cannot synthesize is an error rather than a silent omission,
/// because the failure would otherwise surface as a shape error from ONNX
/// Runtime with no indication of which input was missing.
///
/// # Batch size
///
/// Shapes are `[1, tokens]`. Coalescing several texts into one call is a
/// consumer decision with its own costs — the texts must be padded to a common
/// length and the results split apart again — so it is deliberately not done
/// here.
///
/// # Errors
///
/// [`RuntimeError::ContractMismatch`] when `declared_inputs` names something
/// outside the set above, names one twice, or omits `input_ids` or
/// `attention_mask`; [`RuntimeError::InvalidInput`] when `ids` and
/// `attention_mask` differ in length.
pub fn token_classifier_inputs(
    model_id: &str,
    ids: Vec<i64>,
    attention_mask: Vec<i64>,
    declared_inputs: &[String],
) -> Result<Vec<NamedTensor>, RuntimeError> {
    let n = ids.len();
    if attention_mask.len() != n {
        return Err(RuntimeError::InvalidInput {
            model_id: model_id.to_owned(),
            tensor: "attention_mask".to_owned(),
            message: format!(
                "{} attention-mask entries for {n} token ids",
                attention_mask.len()
            ),
        });
    }
    let contract = |message: String| RuntimeError::ContractMismatch {
        model_id: model_id.to_owned(),
        message,
    };

    let mut ids = Some(ids);
    let mut attention_mask = Some(attention_mask);
    let mut seen_type_ids = false;
    let mut seen_position_ids = false;
    let mut inputs = Vec::with_capacity(declared_inputs.len());

    for name in declared_inputs {
        let data = match name.as_str() {
            "input_ids" => ids.take(),
            "attention_mask" => attention_mask.take(),
            // Single segment. The second-sentence case this distinguishes does
            // not arise when scanning one span of text.
            "token_type_ids" => (!std::mem::replace(&mut seen_type_ids, true)).then(|| vec![0; n]),
            // Absolute positions. Zeros here would place every token at
            // position 0 and give wrong logits with no error.
            "position_ids" => (!std::mem::replace(&mut seen_position_ids, true))
                .then(|| (0..n).map(position).collect::<Result<Vec<_>, _>>())
                .transpose()
                .map_err(|()| contract(format!("{n} tokens do not fit i64 positions")))?,
            other => {
                return Err(contract(format!(
                    "input '{other}' is not a BERT-style token-classifier input; this \
                     helper builds input_ids, attention_mask, token_type_ids and \
                     position_ids only. Build this model's inputs directly."
                )));
            }
        };
        let data = data.ok_or_else(|| contract(format!("input '{name}' is declared twice")))?;
        inputs.push(NamedTensor::new(
            name.clone(),
            vec![1, n],
            TensorData::I64(data),
        ));
    }

    if ids.is_some() || attention_mask.is_some() {
        return Err(contract(
            "the graph does not declare both input_ids and attention_mask; this helper is \
             for BERT-style token classifiers"
                .to_owned(),
        ));
    }
    Ok(inputs)
}

fn position(i: usize) -> Result<i64, ()> {
    i64::try_from(i).map_err(drop)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::runtime::TokenEncoding;

    fn encoding(n: usize) -> TokenEncoding {
        TokenEncoding {
            ids: (100..100 + i64::try_from(n).expect("test size fits")).collect(),
            attention_mask: vec![1; n],
            offsets: vec![(0, 1); n],
            special_tokens_mask: vec![false; n],
        }
    }

    fn build(n: usize, declared: &[&str]) -> Result<Vec<NamedTensor>, RuntimeError> {
        let enc = encoding(n);
        let declared: Vec<String> = declared.iter().map(|s| (*s).to_owned()).collect();
        token_classifier_inputs("m", enc.ids, enc.attention_mask, &declared)
    }

    fn data_of<'a>(out: &'a [NamedTensor], name: &str) -> &'a TensorData {
        &out.iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .data
    }

    #[test]
    fn builds_only_what_the_graph_declares() {
        let out = build(4, &["input_ids", "attention_mask"]).expect("builds");

        assert_eq!(out.len(), 2, "token_type_ids must not appear undeclared");
        assert_eq!(out[0].name, "input_ids");
        assert_eq!(out[0].shape, vec![1, 4]);
        assert_eq!(out[0].data, TensorData::I64(vec![100, 101, 102, 103]));
        assert_eq!(out[1].data, TensorData::I64(vec![1, 1, 1, 1]));
    }

    #[test]
    fn adds_token_type_ids_when_declared() {
        let out = build(3, &["input_ids", "attention_mask", "token_type_ids"]).expect("builds");
        assert_eq!(out.len(), 3);
        assert_eq!(
            data_of(&out, "token_type_ids"),
            &TensorData::I64(vec![0, 0, 0])
        );
    }

    /// The bug this replaced: zeros put every token at position 0.
    #[test]
    fn position_ids_count_up_from_zero() {
        let out = build(4, &["input_ids", "attention_mask", "position_ids"]).expect("builds");
        assert_eq!(
            data_of(&out, "position_ids"),
            &TensorData::I64(vec![0, 1, 2, 3])
        );
    }

    #[test]
    fn order_follows_the_declaration() {
        let out = build(2, &["attention_mask", "input_ids"]).expect("builds");
        let names: Vec<_> = out.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["attention_mask", "input_ids"]);
    }

    #[test]
    fn unknown_input_is_an_error_not_an_omission() {
        // A GLiNER graph. Silently returning two tensors here would surface as
        // an opaque shape error from ONNX Runtime instead.
        let err = build(2, &["input_ids", "span_idx"]).expect_err("must reject");
        assert!(
            err.to_string().contains("span_idx"),
            "names the offender: {err}"
        );
    }

    #[test]
    fn a_name_declared_twice_is_a_contract_error() {
        for twice in ["input_ids", "token_type_ids", "position_ids"] {
            let err =
                build(2, &["input_ids", "attention_mask", twice, twice]).expect_err("must reject");
            assert!(
                matches!(err, RuntimeError::ContractMismatch { .. }),
                "{twice}: {err}"
            );
        }
    }

    #[test]
    fn a_graph_without_both_core_inputs_is_rejected() {
        let err = build(2, &["input_ids"]).expect_err("must reject");
        assert!(
            matches!(err, RuntimeError::ContractMismatch { .. }),
            "{err}"
        );
    }

    #[test]
    fn ids_and_mask_of_different_lengths_are_rejected() {
        let err = token_classifier_inputs(
            "m",
            vec![1, 2, 3],
            vec![1, 1],
            &["input_ids".to_owned(), "attention_mask".to_owned()],
        )
        .expect_err("must reject");
        assert!(matches!(err, RuntimeError::InvalidInput { .. }), "{err}");
    }
}
