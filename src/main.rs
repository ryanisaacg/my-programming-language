use brick::{backend::compile, parser::parse_str, typecheck::typecheck};
use std::fs;

fn main() {
    let (statement, arena) = parse_str("a:=1.0 + 2.0;a=0.5+a;a").unwrap();
    let (statement, arena) = typecheck(&statement, &arena).unwrap();
    println!("{:?}, {:?}", statement, arena);
    let binary = compile(statement, &arena);
    fs::write("out.wasm", binary).expect("Unable to write file");
}
