//! Adapter from `crate::runtime::NamedTensor` to the plain borrowed
//! `(shape, data)` view `decode.rs` (ported from dlp-bench) expects. Borrows
//! rather than copies: `span_probs` is `words x 8 x labels` floats per scan.

use crate::domain::error::DomainError;
use crate::runtime::{NamedTensor, TensorData};

/// A named F32 output tensor, flattened row-major — the only shape
/// `decode.rs` needs to know about.
#[derive(Debug, Clone, Copy)]
pub struct TensorView<'a> {
    pub shape: &'a [usize],
    pub data: &'a [f32],
}

/// Why an argmax has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgmaxError {
    Empty,
    NaN,
}

impl TensorView<'_> {
    /// Index of the tensor's largest value, flattened.
    ///
    /// Ties keep the **first** maximum, as `torch.argmax` does. An empty
    /// tensor or one holding a NaN has no meaningful argmax and is an error —
    /// for `count_logits` a silent `0` would read as "no entities".
    ///
    /// Used for `count_logits`' argmax gate (`decode::count_gate_open`).
    pub fn argmax(&self) -> Result<usize, ArgmaxError> {
        let (&first, rest) = self.data.split_first().ok_or(ArgmaxError::Empty)?;
        if first.is_nan() {
            return Err(ArgmaxError::NaN);
        }
        let (mut best, mut max) = (0, first);
        for (i, &v) in rest.iter().enumerate() {
            if v.is_nan() {
                return Err(ArgmaxError::NaN);
            }
            if v > max {
                max = v;
                best = i + 1;
            }
        }
        Ok(best)
    }
}

pub fn f32_tensor<'a>(model_id: &str, t: &'a NamedTensor) -> Result<TensorView<'a>, DomainError> {
    let TensorData::F32(data) = &t.data else {
        return Err(DomainError::contract(
            model_id,
            format!(
                "expected F32 tensor for '{}', got a different dtype",
                t.name
            ),
        ));
    };
    Ok(TensorView {
        shape: &t.shape,
        data,
    })
}

pub fn find_tensor<'a>(
    model_id: &str,
    outputs: &'a [NamedTensor],
    name: &str,
) -> Result<&'a NamedTensor, DomainError> {
    outputs.iter().find(|t| t.name == name).ok_or_else(|| {
        DomainError::contract(model_id, format!("model returned no '{name}' output"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(data: &[f32]) -> TensorView<'_> {
        TensorView { shape: &[], data }
    }

    #[test]
    fn argmax_keeps_the_first_of_tied_maxima() {
        assert_eq!(view(&[0.0, 2.0, 2.0, 1.0]).argmax(), Ok(1));
        assert_eq!(view(&[3.0, 3.0]).argmax(), Ok(0));
    }

    #[test]
    fn argmax_of_empty_or_nan_is_an_error() {
        assert_eq!(view(&[]).argmax(), Err(ArgmaxError::Empty));
        assert_eq!(view(&[f32::NAN, 1.0]).argmax(), Err(ArgmaxError::NaN));
        assert_eq!(view(&[1.0, f32::NAN]).argmax(), Err(ArgmaxError::NaN));
    }

    #[test]
    fn wrong_dtype_and_missing_output_are_contract_errors() {
        let t = NamedTensor::new("x", vec![1], TensorData::I64(vec![1]));
        assert!(matches!(
            f32_tensor("m", &t),
            Err(DomainError::OutputContract { .. })
        ));
        assert!(matches!(
            find_tensor("m", &[t], "y"),
            Err(DomainError::OutputContract { .. })
        ));
    }
}
