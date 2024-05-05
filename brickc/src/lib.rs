use brick::{
    id::FunctionID, lower_code, typecheck_module, CompileError, LinearFunction, LowerResults,
};
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, FunctionSection, MemorySection, MemoryType, Module,
    StartSection, TypeSection,
};

mod function_bodies;
mod function_headers;
/**
 * Note: currently in WASM, there is only a 0-memory. However, the spec is forwards-compatible with
 * more
 */
const MAIN_MEMORY: u32 = 0;
const STACK_PAGES: u64 = 16;
const HEAP_MINIMUM_PAGES: u64 = 48;
const MEMORY_MINIMUM_PAGES: u64 = STACK_PAGES + HEAP_MINIMUM_PAGES;
const MAXIMUM_MEMORY: u64 = 16_384;
const WASM_PAGE_SIZE: u64 = 65_536;

pub fn compile(
    module_name: &str,
    source_name: &'static str,
    contents: String,
    is_start_function: bool,
) -> Result<Module, CompileError> {
    let LowerResults {
        statements,
        mut functions,
        declarations: _,
        ty_declarations: _,
    } = lower_code(module_name, source_name, contents)?;
    let main = LinearFunction {
        id: FunctionID::new(),
        body: statements,
    };
    functions.insert(0, main);

    let mut module = Module::new();

    let mut ty_section = TypeSection::new();
    let mut fn_section = FunctionSection::new();
    let mut codes = CodeSection::new();
    let mut exports = ExportSection::new();
    let mut memories = MemorySection::new();

    let type_index = 0;
    for function in functions.iter() {
        function_headers::encode(type_index, function, &mut ty_section, &mut fn_section);
        codes.function(&function_bodies::encode(&function));
    }

    memories.memory(MemoryType {
        minimum: MEMORY_MINIMUM_PAGES,
        maximum: Some(MAXIMUM_MEMORY),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    exports.export("memory", ExportKind::Memory, MAIN_MEMORY);
    exports.export("main", ExportKind::Func, 0);

    module.section(&ty_section);
    module.section(&fn_section);
    module.section(&memories);
    module.section(&exports);
    if is_start_function {
        module.section(&StartSection { function_index: 0 });
    }
    module.section(&codes);

    Ok(module)
}
