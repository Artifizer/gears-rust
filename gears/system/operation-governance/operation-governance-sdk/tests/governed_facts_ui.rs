//! Compile-time guarantees of `#[derive(GovernedFacts)]`.
//! Regenerate expected errors with `TRYBUILD=overwrite cargo test -p cf-gears-operation-governance-sdk --test governed_facts_ui`.

#[test]
fn governed_facts_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass_*.rs");
    t.compile_fail("tests/ui/fail_*.rs");
}
