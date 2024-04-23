use anyhow::bail;
use brick::{check_types, eval_preserve_vm, Value};
use data_test_driver::TestValue;

#[test]
fn data() {
    let mut working_dir = std::env::current_dir().unwrap();
    working_dir.pop();
    working_dir.push("tests");
    data_test_driver::test_folder(
        working_dir,
        |contents| -> anyhow::Result<()> {
            check_types(contents)?;
            Ok(())
        },
        |contents, expected| -> anyhow::Result<TestValue> {
            let (mut results, memory) = eval_preserve_vm(contents)?;
            look_for_value(&mut results, &memory[..], expected)
        },
    );
}

fn look_for_value(
    results: &mut Vec<Value>,
    memory: &[u8],
    expected: &TestValue,
) -> anyhow::Result<TestValue> {
    match expected {
        TestValue::Void => {
            if results.len() == 0 {
                Ok(TestValue::Void)
            } else {
                bail!("non-void result returned from test case");
            }
        }
        TestValue::Null => {
            if results.len() == 1 && results[0] == Value::Byte(0) {
                Ok(TestValue::Null)
            } else {
                bail!("wrong number of results returned: {}", results.len());
            }
        }
        TestValue::Nullable(expected) => {
            let first = results.pop();
            if first != Some(Value::Byte(1)) {
                bail!("expected non-null marker, found {first:?}");
            }
            Ok(TestValue::Nullable(Box::new(look_for_value(
                results, memory, expected,
            )?)))
        }
        TestValue::Float(_) | TestValue::Int(_) => {
            if results.len() == 1 {
                Ok(value_to_test_value(results.remove(0)))
            } else {
                bail!("wrong number of results returned: {}", results.len());
            }
        }
        TestValue::String(_) => {
            if results.len() == 2 {
                let pointer = results.pop().unwrap();
                let Value::Size(pointer) = pointer else {
                    bail!("non-pointer type returned: {:?}", pointer);
                };
                let length = results.pop().unwrap();
                let Value::Size(length) = length else {
                    bail!("non-length type returned: {:?}", length);
                };
                let bytes = &memory[pointer..(pointer + length)];
                let string = std::str::from_utf8(bytes)?;
                Ok(TestValue::String(string.to_string()))
            } else {
                bail!("wrong number of results returned: {}", results.len());
            }
        }
    }
}

fn value_to_test_value(val: Value) -> TestValue {
    match val {
        Value::FunctionID(_) => todo!(),
        Value::Size(_) => todo!(),
        Value::Byte(byte) => TestValue::Int(byte as i64),
        Value::Int32(val) => TestValue::Int(val as i64),
        Value::Int64(val) => TestValue::Int(val),
        Value::Float32(val) => TestValue::Float(val as f64),
        Value::Float64(val) => TestValue::Float(val),
    }
}
