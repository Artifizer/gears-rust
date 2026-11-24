//! Load-time and call-time verification: the graph contract, and input
//! shapes against configured limits. Pure functions over names and numbers;
//! nothing here touches a session.

use thiserror::Error;
use toolkit_macros::domain_model;

/// A graph's declared tensor contract violated, at load or at call time.
#[domain_model]
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error("model contract mismatch for {model_id}: {message}")]
pub struct RegistryError {
    pub model_id: String,
    pub message: String,
}

/// A graph's input and output tensor names.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphContract {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// A set of tensor names, sorted once so each comparison against it
/// allocates nothing proportional to the contract.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SortedNames(Box<[String]>);

impl SortedNames {
    #[must_use]
    pub fn new(names: &[String]) -> Self {
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        Self(sorted.into_boxed_slice())
    }

    /// Whether `supplied` names exactly this set, order ignored. `supplied`
    /// is sorted in place.
    fn matches(&self, supplied: &mut [&str]) -> bool {
        supplied.sort_unstable();
        self.0.len() == supplied.len() && self.0.iter().zip(supplied.iter()).all(|(a, b)| a == b)
    }
}

/// Verifies a loaded ONNX session's actual input/output tensor names match
/// what was configured — "transposed-axis protection" (dlp-bench's term):
/// fail loudly at load time rather than silently misinterpreting tensors
/// later. Order-independent (ONNX doesn't guarantee name ordering, and
/// `ort` takes named, not positional, inputs).
///
/// # Errors
///
/// [`RegistryError`] citing both sets for the first side that differs.
pub fn verify_contract(
    model_id: &str,
    expected: &GraphContract,
    actual: &GraphContract,
) -> Result<(), RegistryError> {
    verify_names(
        model_id,
        "input",
        &SortedNames::new(&expected.inputs),
        &actual.inputs,
    )?;
    verify_names(
        model_id,
        "output",
        &SortedNames::new(&expected.outputs),
        &actual.outputs,
    )?;
    Ok(())
}

fn verify_names<S: AsRef<str>>(
    model_id: &str,
    kind: &str,
    expected: &SortedNames,
    actual: &[S],
) -> Result<(), RegistryError> {
    let mut supplied: Vec<&str> = actual.iter().map(AsRef::as_ref).collect();
    if expected.matches(&mut supplied) {
        return Ok(());
    }
    Err(RegistryError {
        model_id: model_id.to_owned(),
        message: format!(
            "{kind} names mismatch: expected {:?}, got {supplied:?}",
            expected.0
        ),
    })
}

/// Checks the set of supplied input names against the model's declared
/// contract: every expected name present, and nothing extra.
///
/// # Errors
///
/// Returns a [`RegistryError`] citing both sets.
pub fn verify_input_names<S: AsRef<str>>(
    model_id: &str,
    expected: &SortedNames,
    supplied: &[S],
) -> Result<(), RegistryError> {
    verify_names(model_id, "input", expected, supplied)
}

/// Limits applied to one call's inputs. Values come from `ModelConfig`; this
/// layer knows nothing about where they were configured.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct InputLimits {
    /// Maximum elements in any one tensor.
    pub elements_per_tensor: usize,
    /// Maximum tensors in one call.
    pub tensors_per_call: usize,
    /// Maximum extent of any single dimension.
    pub extent_per_dimension: usize,
    /// Maximum UTF-8 bytes of text accepted for tokenization.
    pub text_bytes: usize,
}

/// Why an input was rejected. Carries shapes, names and sizes — never tensor
/// contents or tokenized text.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRejection {
    pub tensor: String,
    pub message: String,
}

/// Validates one tensor's declared shape against the data it carries, and
/// against the configured limits.
///
/// Pure arithmetic, deliberately: this is the check that must run *before* any
/// allocation proportional to the declared shape, so it cannot depend on
/// having built anything.
///
/// # Errors
///
/// Returns an [`InputRejection`] naming the tensor and what was wrong.
pub fn validate_tensor(
    name: &str,
    shape: &[usize],
    element_count: usize,
    limits: InputLimits,
) -> Result<(), InputRejection> {
    let reject = |message: String| InputRejection {
        tensor: name.to_owned(),
        message,
    };

    if shape.is_empty() {
        return Err(reject(
            "shape is empty; a tensor needs at least rank 1".to_owned(),
        ));
    }

    for (axis, &dim) in shape.iter().enumerate() {
        if dim == 0 {
            return Err(reject(format!("dimension {axis} is zero")));
        }
        if dim > limits.extent_per_dimension {
            return Err(reject(format!(
                "dimension {axis} is {dim}, over the limit of {}",
                limits.extent_per_dimension
            )));
        }
    }

    // Checked, so a shape whose product overflows is a validation error rather
    // than a wrap into a plausible-looking small number.
    let declared = shape
        .iter()
        .try_fold(1_usize, |acc, &d| acc.checked_mul(d))
        .ok_or_else(|| reject(format!("shape {shape:?} overflows when multiplied out")))?;

    if declared != element_count {
        return Err(reject(format!(
            "shape {shape:?} declares {declared} elements but data has {element_count}"
        )));
    }

    if declared > limits.elements_per_tensor {
        return Err(reject(format!(
            "{declared} elements, over the limit of {}",
            limits.elements_per_tensor
        )));
    }

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn contract(inputs: &[&str], outputs: &[&str]) -> GraphContract {
        GraphContract {
            inputs: names(inputs),
            outputs: names(outputs),
        }
    }

    #[test]
    fn verify_contract_passes_when_names_match_regardless_of_order() {
        let result = verify_contract(
            "m1",
            &contract(&["input_ids", "attention_mask"], &["logits"]),
            &contract(&["attention_mask", "input_ids"], &["logits"]),
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn verify_contract_fails_on_missing_input() {
        let err = verify_contract(
            "m1",
            &contract(&["input_ids", "attention_mask"], &["logits"]),
            &contract(&["input_ids"], &["logits"]),
        )
        .expect_err("missing input");
        assert!(err.message.starts_with("input names"), "{err}");
        assert!(err.message.contains("attention_mask"), "{err}");
    }

    #[test]
    fn verify_contract_fails_on_extra_output() {
        let err = verify_contract(
            "m1",
            &contract(&["input_ids"], &["logits"]),
            &contract(&["input_ids"], &["logits", "extra"]),
        )
        .expect_err("extra output");
        assert!(err.message.starts_with("output names"), "{err}");
        assert!(err.message.contains("extra"), "{err}");
    }

    #[test]
    fn a_repeated_name_does_not_stand_in_for_a_missing_one() {
        let expected = SortedNames::new(&names(&["a", "b"]));
        assert!(verify_input_names("m", &expected, &["a", "a"]).is_err());
        assert!(verify_input_names("m", &expected, &["b", "a"]).is_ok());
    }

    fn limits() -> InputLimits {
        InputLimits {
            elements_per_tensor: 100,
            tensors_per_call: 4,
            extent_per_dimension: 50,
            text_bytes: 1000,
        }
    }

    #[test]
    fn shape_matching_data_is_accepted() {
        assert!(validate_tensor("input_ids", &[2, 3], 6, limits()).is_ok());
    }

    #[test]
    fn shape_disagreeing_with_data_is_rejected_naming_both() {
        let err = validate_tensor("input_ids", &[2, 3], 5, limits()).unwrap_err();
        assert_eq!(err.tensor, "input_ids");
        assert_eq!(
            err.message,
            "shape [2, 3] declares 6 elements but data has 5"
        );
    }

    /// The check that must not become a wrap into a plausible small number.
    /// Needs a permissive per-dimension limit, since that guard would
    /// otherwise reject these dimensions before the multiply is reached.
    #[test]
    fn overflowing_shape_is_a_rejection_not_a_panic() {
        let permissive = InputLimits {
            elements_per_tensor: usize::MAX,
            extent_per_dimension: usize::MAX,
            ..limits()
        };
        // Two of these multiply past usize::MAX.
        let huge = 1_usize << (usize::BITS - 2);
        let err = validate_tensor("t", &[huge, 4], 8, permissive).unwrap_err();
        assert!(err.message.contains("overflow"), "{}", err.message);
    }

    #[test]
    fn zero_and_oversized_dimensions_are_rejected() {
        assert_eq!(
            validate_tensor("t", &[2, 0], 0, limits())
                .unwrap_err()
                .message,
            "dimension 1 is zero"
        );
        assert_eq!(
            validate_tensor("t", &[51], 51, limits())
                .unwrap_err()
                .message,
            "dimension 0 is 51, over the limit of 50"
        );
    }

    /// Both limits are inclusive: exactly at the limit is accepted, one over
    /// is not.
    #[test]
    fn limits_are_inclusive_boundaries() {
        assert!(validate_tensor("t", &[50], 50, limits()).is_ok());
        assert!(validate_tensor("t", &[50, 2], 100, limits()).is_ok());
        let err = validate_tensor(
            "t",
            &[1, 101],
            101,
            InputLimits {
                extent_per_dimension: 200,
                ..limits()
            },
        )
        .unwrap_err();
        assert_eq!(err.message, "101 elements, over the limit of 100");
    }

    #[test]
    fn empty_shape_is_rejected() {
        assert!(validate_tensor("t", &[], 1, limits()).is_err());
    }

    proptest! {
        /// Accepted exactly when the shape is non-empty, every dimension is in
        /// `1..=extent`, the product equals the element count, and the product
        /// is within the element limit: the whole rule, as an oracle.
        #[test]
        fn validate_tensor_agrees_with_its_specification(
            shape in proptest::collection::vec(0_usize..64, 0..4),
            elements in 0_usize..300,
            extent in 1_usize..64,
            max_elements in 1_usize..300,
        ) {
            let limits = InputLimits {
                elements_per_tensor: max_elements,
                tensors_per_call: 1,
                extent_per_dimension: extent,
                text_bytes: 1,
            };
            let product = shape.iter().try_fold(1_usize, |acc, &d| acc.checked_mul(d));
            let expected = !shape.is_empty()
                && shape.iter().all(|&d| (1..=extent).contains(&d))
                && product == Some(elements)
                && elements <= max_elements;
            let result = validate_tensor("t", &shape, elements, limits);
            prop_assert_eq!(result.is_ok(), expected, "{:?}", result);
            if let Err(rejection) = result {
                prop_assert_eq!(rejection.tensor, "t");
            }
        }

        /// Never panics, whatever the numbers.
        #[test]
        fn validate_tensor_never_panics(
            shape in proptest::collection::vec(any::<usize>(), 0..6),
            elements in any::<usize>(),
        ) {
            let permissive = InputLimits {
                elements_per_tensor: usize::MAX,
                tensors_per_call: 1,
                extent_per_dimension: usize::MAX,
                text_bytes: 1,
            };
            drop(validate_tensor("t", &shape, elements, permissive));
        }
    }

    /// Rejection messages reach logs and consumers, so they carry shapes,
    /// names and counts — and cannot carry tensor contents, because
    /// `validate_tensor` is never given them: it takes an element *count*, not
    /// the elements.
    #[test]
    fn rejection_messages_name_the_tensor_and_its_shape() {
        let err = validate_tensor("input_ids", &[2, 2], 3, limits()).unwrap_err();
        assert_eq!(err.tensor, "input_ids");
        assert_eq!(
            err.message,
            "shape [2, 2] declares 4 elements but data has 3"
        );
    }

    #[test]
    fn input_names_are_checked_against_the_contract() {
        let expected = SortedNames::new(&names(&["input_ids", "attention_mask"]));
        assert!(verify_input_names("m1", &expected, &["attention_mask", "input_ids"]).is_ok());

        let err = verify_input_names(
            "m1",
            &expected,
            &["input_ids", "attention_mask", "surprise"],
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("surprise"),
            "cites the offender: {err}"
        );
        assert!(err.to_string().contains("m1"), "names the model: {err}");

        assert!(verify_input_names("m1", &expected, &["input_ids"]).is_err());
    }
}
