use brick::{eval, CompileError};

#[tokio::test]
async fn basic_moves() {
    let result = eval(
        r#"
struct Data {}

fn test(): Data {
    let x = Data{};
    let y = x;
    x
}
    "#,
    )
    .await;
    if let Err(CompileError::BorrowcheckError(error)) = result {
        assert_eq!(error.len(), 1);
    } else {
        panic!("{:?}", result);
    }
}

#[tokio::test]
async fn conditional_move() {
    let result = eval(
        r#"
struct Data {}

fn test(): Data {
    let x = Data{};
    if 1 > 5 {
        let y = x;
    }
    x
}
    "#,
    )
    .await;
    if let Err(CompileError::BorrowcheckError(error)) = result {
        assert_eq!(error.len(), 1);
    } else {
        panic!("{:?}", result);
    }
}