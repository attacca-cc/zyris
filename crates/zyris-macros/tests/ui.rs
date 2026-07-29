#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass_basic.rs");
    t.compile_fail("tests/ui/fail_bi_stream.rs");
    t.compile_fail("tests/ui/fail_not_async.rs");
    t.compile_fail("tests/ui/fail_generic_method.rs");
    t.compile_fail("tests/ui/fail_bad_return.rs");
    t.compile_fail("tests/ui/fail_default_body.rs");
}
