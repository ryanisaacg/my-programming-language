use bytemuck::{Pod, Zeroable};

use crate::{
    id::ConstantID,
    multi_error::{merge_result_list, merge_results, merge_results_or_value},
    parser::{
        AstArena, AstNode, AstNodeValue, FunctionDeclarationValue, FunctionHeaderValue,
        InterfaceDeclarationValue, NameAndType, ParsedFile, SelfParameter, StructDeclarationValue,
        UnionDeclarationValue, UnionDeclarationVariant,
    },
    typecheck::{
        CollectionType, FuncType, InterfaceType, ModuleType, PointerKind, PrimitiveType,
        StructType, TypecheckError, UnionType,
    },
    ExpressionType, SourceRange, TypeDeclaration,
};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, Ordering},
};

pub struct DeclarationContext {
    pub intrinsic_module: FileDeclarations,
    pub auto_destructors_module: FileDeclarations,
    pub files: HashMap<&'static str, FileDeclarations>,
    pub id_to_decl: HashMap<TypeID, TypeDeclaration>,
    pub id_to_func: HashMap<FunctionID, FuncType>,
    pub intrinsic_to_id: HashMap<IntrinsicFunction, FunctionID>,
    pub array_intrinsics: HashMap<&'static str, CollectionIntrinsic>,
    pub dict_intrinsics: HashMap<&'static str, CollectionIntrinsic>,
    pub rc_intrinsics: HashMap<&'static str, CollectionIntrinsic>,
    pub cell_intrinsics: HashMap<&'static str, CollectionIntrinsic>,
    pub extern_function_bindings: Vec<(String, FunctionID)>,
    pub extern_function_exports: Vec<(String, FunctionID)>,
}

impl DeclarationContext {
    pub fn new(
        files: &[(&'static str, &ParsedFile)],
    ) -> Result<DeclarationContext, TypecheckError> {
        let mut ctx = DeclarationContext {
            intrinsic_module: FileDeclarations::new(),
            auto_destructors_module: FileDeclarations::new(),
            files: HashMap::new(),
            id_to_decl: HashMap::new(),
            id_to_func: HashMap::new(),
            intrinsic_to_id: HashMap::new(),
            array_intrinsics: HashMap::new(),
            dict_intrinsics: HashMap::new(),
            rc_intrinsics: HashMap::new(),
            cell_intrinsics: HashMap::new(),
            extern_function_bindings: Vec::new(),
            extern_function_exports: Vec::new(),
        };

        for (name, source) in files {
            ctx.assign_ids_to_names(name, source)?;
        }

        let mut results = Ok(());
        for (name, source) in files {
            merge_results(&mut results, ctx.fill_in_file_type_info(name, source));
        }

        add_intrinsics(&mut ctx);

        results?;
        Ok(ctx)
    }

    fn assign_ids_to_names(
        &mut self,
        module_name: &'static str,
        source: &ParsedFile,
    ) -> Result<(), TypecheckError> {
        let mut result = Ok(());

        let module = match self.files.get_mut(module_name) {
            Some(module) => module,
            None => {
                let file = FileDeclarations::new();
                self.id_to_decl.insert(
                    file.module_id,
                    TypeDeclaration::Module(ModuleType {
                        id: file.module_id,
                        exports: HashMap::new(),
                        provenance: None,
                    }),
                );
                self.files.insert(module_name, file);
                self.files.get_mut(module_name).unwrap()
            }
        };
        let TypeDeclaration::Module(module_decl) =
            self.id_to_decl.get_mut(&module.module_id).unwrap()
        else {
            unreachable!()
        };
        let mut name_origin = HashMap::new();

        for statement in source.top_level.iter() {
            let name = match &statement.value {
                AstNodeValue::StructDeclaration(StructDeclarationValue { name, .. })
                | AstNodeValue::UnionDeclaration(UnionDeclarationValue { name, .. })
                | AstNodeValue::InterfaceDeclaration(InterfaceDeclarationValue { name, .. }) => {
                    module_decl.exports.insert(
                        name.clone(),
                        (None, ExpressionType::ReferenceToType(module.new_type_id())),
                    );
                    Some(name)
                }
                AstNodeValue::FunctionDeclaration(FunctionDeclarationValue { name, .. })
                | AstNodeValue::ExternFunctionBinding(FunctionHeaderValue { name, .. }) => {
                    module_decl.exports.insert(
                        name.clone(),
                        (
                            None,
                            ExpressionType::ReferenceToFunction(module.new_func_id()),
                        ),
                    );
                    Some(name)
                }
                AstNodeValue::Import(paths) => paths.last(),
                AstNodeValue::ConstDeclaration { name, .. } => Some(name),
                _ => None,
            };
            if let Some(name) = name {
                // Check for name conflicts - if an existing declaration has the same
                // name, we'll need to generate an error
                if let Some(existing_provenance) = name_origin.insert(name, &statement.provenance) {
                    merge_results(
                        &mut result,
                        Err(TypecheckError::DeclarationNameConflict {
                            first: existing_provenance.clone(),
                            second: statement.provenance.clone(),
                            name: name.clone(),
                        }),
                    );
                }
            }
        }

        result
    }

    fn fill_in_file_type_info(
        &mut self,
        module_name: &'static str,
        source: &ParsedFile,
    ) -> Result<(), TypecheckError> {
        let mut result = Ok(());

        let mut imports = Vec::new();
        for statement in source.top_level.iter() {
            let AstNodeValue::Import(path) = &statement.value else {
                continue;
            };
            if let Some((name, constant_id, expr)) = merge_results_or_value(
                &mut result,
                resolve_import(self, path, &statement.provenance),
            ) {
                imports.push((name, (constant_id, expr)));
            }
        }

        let file = self.files.get_mut(module_name).unwrap();
        for (name, expr) in imports {
            file.imports.insert(name, expr);
        }
        let module_id = file.module_id;

        let mut names_to_type_id = HashMap::new();
        let TypeDeclaration::Module(module) = &self.id_to_decl[&module_id] else {
            unreachable!()
        };
        for (name, (_, expr)) in module.exports.iter() {
            if let ExpressionType::ReferenceToType(ty_id) = expr {
                names_to_type_id.insert(name.as_str(), *ty_id);
            }
        }
        for (name, expr) in file.imports.iter() {
            if let (_, ExpressionType::ReferenceToType(ty_id)) = expr {
                names_to_type_id.insert(name.as_str(), *ty_id);
            }
        }

        let mut declarations = Vec::new();
        for statement in source.top_level.iter() {
            let decl = match &statement.value {
                AstNodeValue::StructDeclaration(decl) => merge_results_or_value(
                    &mut result,
                    fill_in_struct_info(
                        &source.arena,
                        &names_to_type_id,
                        file,
                        &mut self.id_to_func,
                        decl,
                        &statement.provenance,
                    ),
                ),
                AstNodeValue::UnionDeclaration(decl) => merge_results_or_value(
                    &mut result,
                    fill_in_union_decl(
                        &source.arena,
                        &names_to_type_id,
                        decl,
                        &statement.provenance,
                    ),
                ),
                AstNodeValue::InterfaceDeclaration(decl) => merge_results_or_value(
                    &mut result,
                    fill_in_interface_decl(
                        &source.arena,
                        &names_to_type_id,
                        file,
                        &mut self.id_to_func,
                        decl,
                        &statement.provenance,
                    ),
                ),
                _ => None,
            };
            if let Some(decl) = decl {
                declarations.push(decl);
            }
            let func = match &statement.value {
                AstNodeValue::ExternFunctionBinding(func) => {
                    let id = get_id_for_func_name(file, &self.id_to_decl, func.name.as_str());
                    self.extern_function_bindings.push((func.name.clone(), id));
                    merge_results_or_value(
                        &mut result,
                        fill_in_fn_header(
                            &source.arena,
                            &names_to_type_id,
                            id,
                            func,
                            false,
                            &statement.provenance,
                            None,
                        ),
                    )
                }
                AstNodeValue::FunctionDeclaration(func) => {
                    let id = get_id_for_func_name(file, &self.id_to_decl, func.name.as_str());
                    if func.is_extern {
                        self.extern_function_exports.push((func.name.clone(), id));
                    }
                    merge_results_or_value(
                        &mut result,
                        fill_in_fn_decl(
                            &source.arena,
                            &names_to_type_id,
                            id,
                            func,
                            false,
                            &statement.provenance,
                            None,
                        ),
                    )
                }
                _ => None,
            };
            if let Some(func) = func {
                self.id_to_func.insert(func.id, func);
            }
        }

        let mut constant_ids = Vec::new();
        for statement in source.top_level.iter() {
            let AstNodeValue::ConstDeclaration {
                name,
                type_hint,
                value: _,
                variable_id,
            } = &statement.value
            else {
                continue;
            };
            let Some(type_hint) = type_hint else {
                merge_results(
                    &mut result,
                    Err(TypecheckError::TopLevelConstantMustHaveType(
                        statement.provenance.clone(),
                    )),
                );
                continue;
            };
            let Some(const_ty) = merge_results_or_value(
                &mut result,
                resolve_type_expr(
                    &source.arena,
                    &names_to_type_id,
                    source.arena.get(*type_hint),
                ),
            ) else {
                continue;
            };
            constant_ids.push((name.to_string(), (Some(*variable_id), const_ty)));
        }

        for decl in declarations {
            self.id_to_decl.insert(decl.id(), decl);
        }

        let TypeDeclaration::Module(module) = self.id_to_decl.get_mut(&module_id).unwrap() else {
            unreachable!()
        };
        module.exports.extend(constant_ids);

        result
    }
}

pub struct FileDeclarations {
    id: FileID,
    pub module_id: TypeID,
    type_id_counter: AtomicU32,
    func_id_counter: AtomicU32,
    imports: HashMap<String, (Option<ConstantID>, ExpressionType)>,
}

impl FileDeclarations {
    pub(crate) fn new() -> FileDeclarations {
        let id = FileID::new();

        FileDeclarations {
            id,
            module_id: TypeID(id, 1),
            type_id_counter: AtomicU32::new(2),
            func_id_counter: AtomicU32::new(1),
            imports: HashMap::new(),
        }
    }

    pub fn new_type_id(&self) -> TypeID {
        let type_id = self.type_id_counter.fetch_add(1, Ordering::Relaxed);
        TypeID(self.id, type_id)
    }

    pub fn new_func_id(&self) -> FunctionID {
        let func_id = self.func_id_counter.fetch_add(1, Ordering::Relaxed);
        FunctionID(self.id, func_id)
    }

    pub fn imports(&self) -> &HashMap<String, (Option<ConstantID>, ExpressionType)> {
        &self.imports
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileID(u32);

impl FileID {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        static PACKAGE_COUNTER: AtomicU32 = AtomicU32::new(1);
        Self(PACKAGE_COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeID(FileID, u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FunctionID(FileID, u32);

unsafe impl Zeroable for FunctionID {}

unsafe impl Pod for FunctionID {}

fn get_id_for_func_name(
    module: &FileDeclarations,
    id_to_decl: &HashMap<TypeID, TypeDeclaration>,
    name: &str,
) -> FunctionID {
    let TypeDeclaration::Module(module) = &id_to_decl[&module.module_id] else {
        unreachable!()
    };
    let (_, ExpressionType::ReferenceToFunction(func_id)) = &module.exports[name] else {
        unreachable!()
    };
    *func_id
}

fn resolve_import(
    declarations: &DeclarationContext,
    path: &[String],
    provenance: &SourceRange,
) -> Result<(String, Option<ConstantID>, ExpressionType), TypecheckError> {
    let mut path_iter = path.iter().peekable();
    let (mut imported_name, module_id) = if path_iter.next().unwrap() == "self" {
        let filename = path_iter
            .next()
            .ok_or(TypecheckError::IllegalImport(provenance.clone()))?;
        let file = &declarations.files.get(filename.as_str());
        let file =
            file.ok_or_else(|| TypecheckError::FileNotFound(provenance.clone(), filename.clone()))?;
        let module_id = &file.module_id;
        (filename, module_id)
    } else {
        // Should non-self imports grab from local modules? should it work only for packages? not sure
        todo!("package imports")
    };
    let mut imported_value = &declarations.id_to_decl[module_id];

    while let Some(current_path) = path_iter.next() {
        let current_module = imported_value
            .as_module()
            .ok_or_else(|| TypecheckError::ImportPathMustBeModule(provenance.clone()))?;
        match &current_module.exports[current_path.as_str()] {
            (Some(const_id), expr) => {
                return Ok((current_path.clone(), Some(*const_id), expr.clone()));
            }
            (None, ExpressionType::ReferenceToType(ty_id)) => {
                match declarations.id_to_decl.get(ty_id) {
                    Some(decl) => {
                        imported_value = decl;
                    }
                    None => {
                        // If the given type doesn't have a declaration yet, we can assume it's not
                        // a module
                        if path_iter.peek().is_some() {
                            return Err(TypecheckError::ImportPathMustBeModule(provenance.clone()));
                        } else {
                            let ty_id = *ty_id;

                            return Ok((
                                current_path.clone(),
                                None,
                                ExpressionType::ReferenceToType(ty_id),
                            ));
                        }
                    }
                }
            }
            (None, ExpressionType::ReferenceToFunction(fn_id)) => {
                // If there are further components in the import path, that's illegal
                if path_iter.peek().is_some() {
                    return Err(TypecheckError::ImportPathMustBeModule(provenance.clone()));
                } else {
                    let fn_id = *fn_id;

                    return Ok((
                        current_path.clone(),
                        None,
                        ExpressionType::ReferenceToFunction(fn_id),
                    ));
                }
            }
            _ => todo!("can't import that yet"),
        }
        imported_name = current_path;
    }
    // If a function wasn't imported, bring the last type traversed into scope
    let ty_id = imported_value.id();
    Ok((
        imported_name.to_string(),
        None,
        ExpressionType::ReferenceToType(ty_id),
    ))
}

fn fill_in_struct_info(
    ast: &AstArena,
    names_to_type_id: &HashMap<&str, TypeID>,
    module: &FileDeclarations,
    id_to_func: &mut HashMap<FunctionID, FuncType>,
    decl: &StructDeclarationValue,
    provenance: &SourceRange,
) -> Result<TypeDeclaration, TypecheckError> {
    let id = names_to_type_id[decl.name.as_str()];

    let fields: HashMap<_, _> = merge_result_list(decl.fields.iter().map(
        |NameAndType {
             name,
             ty,
             provenance,
         }| {
            let ty = resolve_type_expr(ast, names_to_type_id, ast.get(*ty))?;
            if matches!(ty, ExpressionType::Pointer(_, _)) {
                return Err(TypecheckError::IllegalReferenceInsideDataType(
                    provenance.clone(),
                ));
            }
            Ok((name.clone(), ty))
        },
    ))?;

    let mut result = Ok(());

    let mut associated_functions = HashMap::new();
    for node in decl.associated_functions.iter() {
        let node = ast.get(*node);
        match &node.value {
            AstNodeValue::FunctionDeclaration(func) => {
                let func_id = module.new_func_id();
                associated_functions.insert(func.name.clone(), func_id);
                if let Some(func_type) = merge_results_or_value(
                    &mut result,
                    fill_in_fn_decl(
                        ast,
                        names_to_type_id,
                        func_id,
                        func,
                        true,
                        &node.provenance,
                        Some(&ExpressionType::InstanceOf(id)),
                    ),
                ) {
                    id_to_func.insert(func_id, func_type);
                }
            }
            _ => panic!("Associated function should not be anything but function declaration"),
        }
    }

    let mut is_affine = false;
    for property in decl.properties.iter() {
        match property.as_str() {
            "Resource" => is_affine = true,
            _ => {
                merge_results(
                    &mut result,
                    Err(TypecheckError::UnknownProperty(
                        property.clone(),
                        provenance.clone(),
                    )),
                );
            }
        }
    }

    result?;

    Ok(TypeDeclaration::Struct(StructType {
        id,
        fields,
        associated_functions,
        is_affine,
        provenance: Some(provenance.clone()),
    }))
}

fn fill_in_interface_decl(
    ast: &AstArena,
    names_to_type_id: &HashMap<&str, TypeID>,
    module: &FileDeclarations,
    id_to_func: &mut HashMap<FunctionID, FuncType>,
    interface: &InterfaceDeclarationValue,
    provenance: &SourceRange,
) -> Result<TypeDeclaration, TypecheckError> {
    let id = names_to_type_id[interface.name.as_str()];

    let mut associated_functions = HashMap::new();

    let mut results = Ok(());
    for node in interface.associated_functions.iter() {
        let func_id = module.new_func_id();
        let node = ast.get(*node);

        let func_type = match &node.value {
            AstNodeValue::RequiredFunction(func) => {
                associated_functions.insert(func.name.clone(), func_id);
                fill_in_fn_header(
                    ast,
                    names_to_type_id,
                    func_id,
                    func,
                    true,
                    &node.provenance,
                    Some(&ExpressionType::InstanceOf(id)),
                )
            }
            AstNodeValue::FunctionDeclaration(func) => {
                associated_functions.insert(func.name.clone(), func_id);
                fill_in_fn_decl(
                    ast,
                    names_to_type_id,
                    func_id,
                    func,
                    true,
                    &node.provenance,
                    Some(&ExpressionType::InstanceOf(id)),
                )
            }
            _ => panic!("Associated function should not be anything but function declaration"),
        };
        if let Some(func_type) = merge_results_or_value(&mut results, func_type) {
            id_to_func.insert(func_id, func_type);
        }
    }

    results?;
    Ok(TypeDeclaration::Interface(InterfaceType {
        id,
        associated_functions,
        provenance: Some(provenance.clone()),
    }))
}

fn fill_in_union_decl(
    ast: &AstArena,
    names_to_type_id: &HashMap<&str, TypeID>,
    UnionDeclarationValue {
        variants: variant_ast,
        name,
        properties,
    }: &UnionDeclarationValue,
    provenance: &SourceRange,
) -> Result<TypeDeclaration, TypecheckError> {
    let variants = merge_result_list(variant_ast.iter().map(|variant| {
        Ok(match variant {
            UnionDeclarationVariant::WithValue(NameAndType {
                name,
                ty,
                provenance,
            }) => {
                let ty = resolve_type_expr(ast, names_to_type_id, ast.get(*ty))?;
                if matches!(ty, ExpressionType::Pointer(_, _)) {
                    return Err(TypecheckError::IllegalReferenceInsideDataType(
                        provenance.clone(),
                    ));
                }
                (name.clone(), Some(ty))
            }
            UnionDeclarationVariant::WithoutValue(name) => (name.clone(), None),
        })
    }))?;

    let mut is_affine = false;
    for property in properties.iter() {
        match property.as_str() {
            "Resource" => is_affine = true,
            _ => {
                return Err(TypecheckError::UnknownProperty(
                    property.clone(),
                    provenance.clone(),
                ))
            }
        }
    }

    Ok(TypeDeclaration::Union(UnionType {
        id: names_to_type_id[name.as_str()],
        variant_order: variant_ast
            .iter()
            .map(|variant| match variant {
                UnionDeclarationVariant::WithValue(name_and_type) => name_and_type.name.clone(),
                UnionDeclarationVariant::WithoutValue(name) => name.clone(),
            })
            .collect(),
        variants,
        is_affine,
        provenance: Some(provenance.clone()),
    }))
}

fn fill_in_fn_decl(
    ast: &AstArena,
    names_to_type_id: &HashMap<&str, TypeID>,
    id: FunctionID,
    FunctionDeclarationValue {
        self_param,
        params: ast_params,
        returns,
        is_coroutine,
        ..
    }: &FunctionDeclarationValue,
    is_associated: bool,
    provenance: &SourceRange,
    self_context: Option<&ExpressionType>,
) -> Result<FuncType, TypecheckError> {
    let self_param_ty = include_self_param(self_param.as_ref(), self_context, provenance)?;
    let mut params =
        Vec::with_capacity(if self_param.is_some() { 1 } else { 0 } + ast_params.len());
    if let Some(self_param_ty) = self_param_ty {
        params.push(self_param_ty);
    }
    for (_, NameAndType { ty, .. }) in ast_params {
        params.push(resolve_type_expr(ast, names_to_type_id, ast.get(*ty))?);
    }

    Ok(FuncType {
        id,
        type_param_count: 0,
        params,
        returns: returns
            .as_ref()
            .map(|returns| resolve_type_expr(ast, names_to_type_id, ast.get(*returns)))
            .unwrap_or(Ok(ExpressionType::Void))?,
        is_associated,
        is_coroutine: *is_coroutine,
        provenance: Some(provenance.clone()),
    })
}

fn fill_in_fn_header(
    ast: &AstArena,
    names_to_type_id: &HashMap<&str, TypeID>,
    id: FunctionID,
    FunctionHeaderValue {
        self_param,
        params: ast_params,
        returns,
        ..
    }: &FunctionHeaderValue,
    is_associated: bool,
    provenance: &SourceRange,
    self_context: Option<&ExpressionType>,
) -> Result<FuncType, TypecheckError> {
    let self_param_ty = include_self_param(self_param.as_ref(), self_context, provenance)?;
    let mut params =
        Vec::with_capacity(if self_param.is_some() { 1 } else { 0 } + ast_params.len());
    if let Some(self_param_ty) = self_param_ty {
        params.push(self_param_ty);
    }
    for NameAndType { ty, .. } in ast_params {
        params.push(resolve_type_expr(ast, names_to_type_id, ast.get(*ty))?);
    }

    Ok(FuncType {
        id,
        type_param_count: 0,
        params,
        returns: returns
            .as_ref()
            .map(|returns| resolve_type_expr(ast, names_to_type_id, ast.get(*returns)))
            .unwrap_or(Ok(ExpressionType::Void))?,
        is_associated,
        is_coroutine: false,
        provenance: Some(provenance.clone()),
    })
}

fn include_self_param(
    self_param: Option<&SelfParameter>,
    self_context: Option<&ExpressionType>,
    provenance: &SourceRange,
) -> Result<Option<ExpressionType>, TypecheckError> {
    Ok(if let Some(self_param) = self_param {
        let Some(self_context) = self_context else {
            return Err(TypecheckError::SelfParameterInNonAssociatedFunc(
                provenance.clone(),
            ));
        };
        Some(match self_param {
            crate::parser::SelfParameter::Unique => {
                ExpressionType::Pointer(PointerKind::Unique, Box::new(self_context.clone()))
            }
            crate::parser::SelfParameter::Shared => {
                ExpressionType::Pointer(PointerKind::Shared, Box::new(self_context.clone()))
            }
            crate::parser::SelfParameter::Owned => self_context.clone(),
        })
    } else {
        None
    })
}

pub fn resolve_type_expr(
    ast: &AstArena,
    name_to_type_id: &HashMap<&str, TypeID>,
    node: &AstNode,
) -> Result<ExpressionType, TypecheckError> {
    Ok(match &node.value {
        AstNodeValue::Name { value: name, .. } => match name.as_str() {
            "bool" => ExpressionType::Primitive(PrimitiveType::Bool),
            "i32" => ExpressionType::Primitive(PrimitiveType::Int32),
            "f32" => ExpressionType::Primitive(PrimitiveType::Float32),
            "i64" => ExpressionType::Primitive(PrimitiveType::Int64),
            "f64" => ExpressionType::Primitive(PrimitiveType::Float64),
            "char" => ExpressionType::Primitive(PrimitiveType::Char),
            "string" => ExpressionType::Collection(CollectionType::String),
            "size" => ExpressionType::Primitive(PrimitiveType::PointerSize),
            other => ExpressionType::InstanceOf(*name_to_type_id.get(other).ok_or(
                TypecheckError::NameNotFound(node.provenance.clone(), other.to_string()),
            )?),
        },
        AstNodeValue::VoidType => ExpressionType::Void,
        AstNodeValue::UniqueType(inner) => ExpressionType::Pointer(
            PointerKind::Unique,
            Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*inner))?),
        ),
        AstNodeValue::SharedType(inner) => ExpressionType::Pointer(
            PointerKind::Shared,
            Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*inner))?),
        ),
        AstNodeValue::ArrayType(inner) => ExpressionType::Collection(CollectionType::Array(
            Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*inner))?),
        )),
        AstNodeValue::RcType(inner) => {
            ExpressionType::Collection(CollectionType::ReferenceCounter(Box::new(
                resolve_type_expr(ast, name_to_type_id, ast.get(*inner))?,
            )))
        }
        AstNodeValue::DictType(key, value) => ExpressionType::Collection(CollectionType::Dict(
            Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*key))?),
            Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*value))?),
        )),
        AstNodeValue::NullableType(inner) => ExpressionType::Nullable(Box::new(resolve_type_expr(
            ast,
            name_to_type_id,
            ast.get(*inner),
        )?)),
        AstNodeValue::GeneratorType { yield_ty, param_ty } => ExpressionType::Generator {
            yield_ty: Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*yield_ty))?),
            param_ty: Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*param_ty))?),
        },
        AstNodeValue::CellType(inner_ty) => ExpressionType::Collection(CollectionType::Cell(
            Box::new(resolve_type_expr(ast, name_to_type_id, ast.get(*inner_ty))?),
        )),
        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::Declaration(_, _, _, _)
        | AstNodeValue::ConstDeclaration { .. }
        | AstNodeValue::Import(_)
        | AstNodeValue::Return(_)
        | AstNodeValue::Yield(_)
        | AstNodeValue::Statement(_)
        | AstNodeValue::Deref(_)
        | AstNodeValue::Int(_)
        | AstNodeValue::Null
        | AstNodeValue::Float(_)
        | AstNodeValue::Bool(_)
        | AstNodeValue::BinExpr(_, _, _)
        | AstNodeValue::If(_)
        | AstNodeValue::While(_, _)
        | AstNodeValue::Loop(_)
        | AstNodeValue::Call(_, _)
        | AstNodeValue::TakeUnique(_)
        | AstNodeValue::TakeRef(_)
        | AstNodeValue::RecordLiteral { .. }
        | AstNodeValue::ArrayLiteral(_)
        | AstNodeValue::ArrayLiteralLength(_, _)
        | AstNodeValue::Block(_)
        | AstNodeValue::StringLiteral(_)
        | AstNodeValue::CharLiteral(_)
        | AstNodeValue::UnaryExpr(_, _)
        | AstNodeValue::DictLiteral(_)
        | AstNodeValue::Match(_)
        | AstNodeValue::BorrowDeclaration(..)
        | AstNodeValue::ReferenceCountLiteral(_)
        | AstNodeValue::CellLiteral(_) => {
            panic!("ICE: Illegal node in type name: {:?}", node.value);
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicFunction {
    ArrayLength,
    ArrayPush,
    ArrayFree,

    DictionaryInsert,
    DictionaryContains,

    RcClone,
    RcDecrement,
    RcFree,

    CellGet,
    CellSet,
}

pub struct CollectionIntrinsic {
    pub intrinsic_fn: IntrinsicFunction,
    pub fn_id: FunctionID,
    pub ptr_ty: PointerKind,
}

fn add_intrinsics(ctx: &mut DeclarationContext) {
    let mut array_intrinsics = HashMap::new();
    add_intrinsic(
        ctx,
        &mut array_intrinsics,
        "len",
        IntrinsicFunction::ArrayLength,
        1,
        vec![ExpressionType::Pointer(
            PointerKind::Shared,
            Box::new(ExpressionType::TypeParameterReference(0)),
        )],
        ExpressionType::Primitive(PrimitiveType::PointerSize),
        PointerKind::Shared,
    );
    add_intrinsic(
        ctx,
        &mut array_intrinsics,
        "push",
        IntrinsicFunction::ArrayPush,
        1,
        vec![
            ExpressionType::Pointer(
                PointerKind::Unique,
                Box::new(ExpressionType::Collection(CollectionType::Array(Box::new(
                    ExpressionType::TypeParameterReference(0),
                )))),
            ),
            ExpressionType::TypeParameterReference(0),
        ],
        ExpressionType::Void,
        PointerKind::Unique,
    );
    ctx.array_intrinsics = array_intrinsics;

    let mut dict_intrinsics = HashMap::new();
    add_intrinsic(
        ctx,
        &mut dict_intrinsics,
        "contains_key",
        IntrinsicFunction::DictionaryContains,
        2,
        vec![
            ExpressionType::Pointer(
                PointerKind::Shared,
                Box::new(ExpressionType::Collection(CollectionType::Dict(
                    Box::new(ExpressionType::TypeParameterReference(0)),
                    Box::new(ExpressionType::TypeParameterReference(1)),
                ))),
            ),
            ExpressionType::Pointer(
                PointerKind::Shared,
                Box::new(ExpressionType::TypeParameterReference(0)),
            ),
        ],
        ExpressionType::Primitive(PrimitiveType::Bool),
        PointerKind::Shared,
    );
    add_intrinsic(
        ctx,
        &mut dict_intrinsics,
        "insert",
        IntrinsicFunction::DictionaryInsert,
        2,
        vec![
            ExpressionType::Pointer(
                PointerKind::Unique,
                Box::new(ExpressionType::Collection(CollectionType::Dict(
                    Box::new(ExpressionType::TypeParameterReference(0)),
                    Box::new(ExpressionType::TypeParameterReference(1)),
                ))),
            ),
            ExpressionType::TypeParameterReference(0),
            ExpressionType::TypeParameterReference(1),
        ],
        ExpressionType::Void,
        PointerKind::Unique,
    );
    ctx.dict_intrinsics = dict_intrinsics;

    let mut rc_intrinsics = HashMap::new();
    add_intrinsic(
        ctx,
        &mut rc_intrinsics,
        "clone",
        IntrinsicFunction::RcClone,
        1,
        vec![ExpressionType::Pointer(
            PointerKind::Shared,
            Box::new(ExpressionType::Collection(
                CollectionType::ReferenceCounter(Box::new(ExpressionType::TypeParameterReference(
                    0,
                ))),
            )),
        )],
        ExpressionType::Collection(CollectionType::ReferenceCounter(Box::new(
            ExpressionType::TypeParameterReference(0),
        ))),
        PointerKind::Shared,
    );
    ctx.rc_intrinsics = rc_intrinsics;

    let mut cell_intrinsics = HashMap::new();
    add_intrinsic(
        ctx,
        &mut cell_intrinsics,
        "get",
        IntrinsicFunction::CellGet,
        1,
        vec![
            ExpressionType::Pointer(
                PointerKind::Shared,
                Box::new(ExpressionType::Collection(CollectionType::Cell(Box::new(
                    ExpressionType::TypeParameterReference(0),
                )))),
            ),
            ExpressionType::Pointer(
                PointerKind::Unique,
                Box::new(ExpressionType::TypeParameterReference(0)),
            ),
        ],
        ExpressionType::Void,
        PointerKind::Shared,
    );
    add_intrinsic(
        ctx,
        &mut cell_intrinsics,
        "set",
        IntrinsicFunction::CellSet,
        1,
        vec![
            ExpressionType::Pointer(
                PointerKind::Shared,
                Box::new(ExpressionType::Collection(CollectionType::Cell(Box::new(
                    ExpressionType::TypeParameterReference(0),
                )))),
            ),
            ExpressionType::TypeParameterReference(0),
        ],
        ExpressionType::Void,
        PointerKind::Shared,
    );
    ctx.cell_intrinsics = cell_intrinsics;
}

#[allow(clippy::too_many_arguments)]
fn add_intrinsic(
    ctx: &mut DeclarationContext,
    collection_fns: &mut HashMap<&'static str, CollectionIntrinsic>,
    name: &'static str,
    intrinsic_fn: IntrinsicFunction,
    type_param_count: usize,
    params: Vec<ExpressionType>,
    returns: ExpressionType,
    ptr_ty: PointerKind,
) {
    let fn_id = ctx.intrinsic_module.new_func_id();
    collection_fns.insert(
        name,
        CollectionIntrinsic {
            intrinsic_fn,
            fn_id,
            ptr_ty,
        },
    );
    ctx.id_to_func.insert(
        fn_id,
        FuncType {
            id: fn_id,
            is_associated: true,
            type_param_count,
            params,
            returns,
            is_coroutine: false,
            provenance: None,
        },
    );
    ctx.intrinsic_to_id.insert(intrinsic_fn, fn_id);
}
