mod common;
use common::run_test;

#[test]
fn auto_dereference() {
    assert_eq!(
        10i64,
        run_test(
            r#"
fn test(): i64 {
    let a = 5
    let b = shared a
    let c = a + b
    c
}
"#,
            ()
        )
        .unwrap()
    );
}

#[test]
fn mutation_auto_dereference() {
    assert_eq!(
        6i64,
        run_test(
            r#"
fn test(): i64 {
    let a = 5
    increment(unique a)
    a
}

fn increment(val: unique i64): void {
    val = val + 1
}

"#,
            ()
        )
        .unwrap()
    );
}
