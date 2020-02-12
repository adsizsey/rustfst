use std::fmt::Display;

use crate::algorithms::fst_convert;
use crate::fst_traits::{AllocableFst, MutableFst, SerializableFst};
use crate::semirings::{SerializableSemiring, WeaklyDivisibleSemiring};
use crate::tests_openfst::FstTestData;

use failure::Fallible;

pub fn test_fst_convert<F>(test_data: &FstTestData<F>) -> Fallible<()>
where
    F: SerializableFst + MutableFst + Display + AllocableFst,
    F::W: SerializableSemiring + WeaklyDivisibleSemiring,
{
    // Invert
    let fst = test_data.raw.clone();
    let fst_converted: F = fst_convert(fst.clone());
    assert_eq!(
        fst_converted,
        fst,
        "{}",
        error_message_fst!(fst_converted, fst, "Invert")
    );
    Ok(())
}
